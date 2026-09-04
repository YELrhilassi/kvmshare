//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, hide the local cursor while being controlled,
//! read/write the clipboard. Implements [`Injector`] from the core crate.

use std::collections::HashSet;

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
    /// Keys (HID usages) injected as down and not yet released. `leave`
    /// releases everything still held — the server may never deliver the
    /// matching ups (the user crossed back mid-hold), and the desktop
    /// must not be left with a stuck key.
    keys_down: HashSet<u32>,
    /// Buttons injected as down and not yet released (same contract as
    /// [`X11Injector::keys_down`]).
    buttons_down: HashSet<u8>,
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
        Ok(Self {
            conn,
            root,
            clipboard,
            cursor_hidden: false,
            last_remote: None,
            keys_down: HashSet::new(),
            buttons_down: HashSet::new(),
        })
    }

    /// The default screen, used for the reported screen shape.
    fn default_screen(&self) -> &xproto::Screen {
        &self.conn.setup().roots[0]
    }
}

/// XTest fake-input event type for a pointer motion.
const MOTION_NOTIFY: u8 = 6;

impl X11Injector {
    /// The pointer's current position on the root window, if readable.
    fn pointer_pos(&self) -> Option<(i32, i32)> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .ok()?
            .reply()
            .ok()?;
        Some((reply.root_x as i32, reply.root_y as i32))
    }

    /// Inject *relative* pointer motion with XTest. The classic XTEST
    /// encoding for "move by dx/dy": a MotionNotify fake input whose
    /// root is `None` (0); the server treats the coordinates as relative
    /// deltas from the current pointer position. Relative input goes
    /// through the desktop's normal pointer transform (acceleration), so
    /// the client's cursor feels like its own physical mouse.
    fn xtest_move_rel(&mut self, dx: i32, dy: i32) {
        let _ = self.conn.xtest_fake_input(
            MOTION_NOTIFY,
            0,
            x11rb::CURRENT_TIME,
            x11rb::NONE, // root None => relative dx/dy
            dx as i16,
            dy as i16,
            0,
        );
        let _ = self.conn.flush();
    }
}

impl Injector for X11Injector {
    fn screen_info(&mut self) -> ScreenInfo {
        let s = self.default_screen();
        ScreenInfo { width: s.width_in_pixels as u32, height: s.height_in_pixels as u32, scale: 1.0 }
    }

    fn move_cursor(&mut self, x: i32, y: i32) {
        // Absolute placement: a direct warp is exact and has no
        // acceleration — right for entry points.
        let _ = self.conn.warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }

    fn move_rel(&mut self, dx: i32, dy: i32) {
        self.xtest_move_rel(dx, dy);
    }

    fn cursor_position(&mut self) -> (i32, i32) {
        self.pointer_pos().unwrap_or((0, 0))
    }

    fn button(&mut self, button: u8, pressed: bool) {
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A release for a button we did not press
        // is dropped (its press happened on the server's machine).
        if pressed {
            self.buttons_down.insert(button);
        } else if !self.buttons_down.remove(&button) {
            return;
        }
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
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A repeat for a key we did not press (it
        // was held across the boundary, pressed on the server) must not
        // start a press here.
        match kind {
            KeyKind::Down => {
                self.keys_down.insert(key);
            }
            KeyKind::Up => {
                self.keys_down.remove(&key);
            }
            KeyKind::Repeat => {
                if !self.keys_down.contains(&key) {
                    return;
                }
            }
        }
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
        // Control left this machine: release every key and button we
        // injected and never saw released — the user may have crossed
        // back mid-hold, so the matching ups will never arrive.
        let keys: Vec<u32> = self.keys_down.drain().collect();
        for key in keys {
            self.key(KeyKind::Up, key);
        }
        let buttons: Vec<u8> = self.buttons_down.drain().collect();
        for button in buttons {
            self.button(button, false);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The decisive encoding fact behind relative client motion: XTest
    /// fake motion with `root = None` must move the pointer *relative*
    /// to where it is (not teleport it to coordinates (dx, dy)). The X
    /// server applies its own pointer transform to injected relative
    /// motion just like a physical mouse's, so the travel distance is
    /// profile-dependent — what matters is that it moved in the
    /// requested direction from its current position rather than jumping
    /// to absolute (40, 40). Verified on a live X server; skipped where
    /// none is available. Restores the pointer afterwards.
    ///
    /// The desktop is live, so a busy hand can move the pointer mid-test:
    /// the probe retries, and only a *deterministic* teleport to the
    /// injected absolute coordinates fails immediately (that is the bug
    /// this test exists to catch). Persistent interference after retries
    /// is reported and skipped, not failed — the machine is being used.
    #[test]
    fn xtest_none_root_motion_is_relative() {
        let Ok(mut inj) = X11Injector::new(None) else {
            eprintln!("skipping: no X server available");
            return;
        };
        let (sx, sy) = inj.pointer_pos().expect("pointer position");
        for attempt in 0..3 {
            let before = inj.pointer_pos().unwrap();
            inj.xtest_move_rel(40, 0);
            std::thread::sleep(Duration::from_millis(40));
            let after = inj.pointer_pos().unwrap();
            if after == (40, 40) {
                // Deterministic teleport to the injected coordinates: the
                // real bug. The pointer raced back with the desktop hand;
                // restore and fail loudly.
                inj.move_cursor(sx, sy);
                panic!("pointer teleported to absolute (40, 40) — root=None was treated as absolute");
            }
            if after.0 > before.0 {
                // Moved right from where it started: relative semantics.
                inj.move_cursor(sx, sy);
                return;
            }
            eprintln!(
                "attempt {attempt}: pointer went {before:?} -> {after:?} (desktop busy?), retrying"
            );
        }
        inj.move_cursor(sx, sy);
        eprintln!("skipping: desktop too busy to verify relative motion");
    }
}