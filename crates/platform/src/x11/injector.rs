//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, hide the local cursor while being controlled,
//! read/write the clipboard. Implements [`Injector`] from the core crate.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use kvmshare_core::client::Injector;
use kvmshare_log::log_warn;
use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use super::buttons;

/// Core X event codes used by XTest's fake-input requests. (x11rb does
/// not export these as constants, so they live here next to their use.)
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;

/// The client-side injector over an X display.
pub struct X11Injector {
    conn: RustConnection,
    root: xproto::Window,
    clipboard: Option<arboard::Clipboard>,
    /// True while we are hiding the local cursor (between `enter` and
    /// `leave`). Lets `leave` only show the cursor if we hid it.
    cursor_hidden: bool,
    /// Last clipboard content applied from the server; the clipboard
    /// poller skips it so remote content is never echoed back.
    last_remote: Option<(String, Vec<u8>)>,
}

impl X11Injector {
    pub fn new(display: Option<&str>) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
        let root = conn.setup().roots[screen_num].root;

        if conn.extension_information(xfixes::X11_EXTENSION_NAME).map_err(|e| format!("XFixes query: {e}"))?.is_none() {
            return Err("XFixes extension not available".into());
        }
        conn.xfixes_query_version(5, 0).map_err(|e| format!("XFixes version: {e}"))?;

        let clipboard = if display.is_none() { arboard::Clipboard::new().ok() } else { None };
        Ok(Self { conn, root, clipboard, cursor_hidden: false, last_remote: None })
    }

    /// The default screen, used for the reported screen shape.
    fn default_screen(&self) -> &xproto::Screen {
        &self.conn.setup().roots[0]
    }
}

impl Injector for X11Injector {
    fn screen_info(&mut self) -> ScreenInfo {
        let s = self.default_screen();
        ScreenInfo { width: s.width_in_pixels as u32, height: s.height_in_pixels as u32, scale: 1.0 }
    }

    fn move_cursor(&mut self, x: i32, y: i32) {
        let _ = self.conn.warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }

    fn button(&mut self, button: u8, pressed: bool) {
        let Some(x11_button) = buttons::to_x11(button) else { return };
        let ty = if pressed { BUTTON_PRESS } else { BUTTON_RELEASE };
        let _ = self.conn.xtest_fake_input(ty, x11_button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        let _ = self.conn.flush();
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        // Clamp to a sane number of notches per message.
        let notches = (dx.abs() + dy.abs()).clamp(1, 10);
        let Some(button) = buttons::wheel_to_x11(dx, dy) else { return };
        for _ in 0..notches {
            let _ = self.conn.xtest_fake_input(BUTTON_PRESS, button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
            let _ = self.conn.xtest_fake_input(BUTTON_RELEASE, button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        }
        let _ = self.conn.flush();
    }

    fn key(&mut self, kind: KeyKind, key: u32) {
        // Canonical HID usage -> evdev -> X keycode (the standard
        // `keycode = evdev + 8` mapping). Unknown usages are dropped: a
        // wrong key would be worse than no key.
        let Some(evdev) = crate::keys::evdev_from_hid(key) else { return };
        let keycode = evdev + 8;
        let is_press = matches!(kind, KeyKind::Down | KeyKind::Repeat);
        let ty = if is_press { KEY_PRESS } else { KEY_RELEASE };
        let _ = self.conn.xtest_fake_input(ty, keycode as u8, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        let _ = self.conn.flush();
    }

    fn enter(&mut self) {
        // Hide our own cursor so the server's stream is the only visible
        // one. This is the fix for the classic KVM "leftover cursor"
        // problem on every platform, not just X11.
        let _ = xfixes::hide_cursor(&self.conn, self.root);
        let _ = self.conn.flush();
        self.cursor_hidden = true;
    }

    fn leave(&mut self) {
        if self.cursor_hidden {
            let _ = xfixes::show_cursor(&self.conn, self.root);
            let _ = self.conn.flush();
            self.cursor_hidden = false;
        }
    }

    fn clipboard(&mut self, mime: &str, data: &[u8]) {
        if mime != "text/plain" {
            log_warn!("clipboard: ignoring non-text mime {mime:?}");
            return;
        }
        if let Some(cb) = &mut self.clipboard {
            if let Ok(text) = std::str::from_utf8(data) {
                if let Err(e) = cb.set_text(text.to_owned()) {
                    log_warn!("clipboard: set failed: {e}");
                }
            }
            self.last_remote = Some((mime.to_owned(), data.to_vec()));
        }
    }

    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
        let text = self.clipboard.as_mut()?.get_text().ok()?;
        Some(("text/plain".into(), text.into_bytes()))
    }

    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        self.last_remote.clone()
    }
}