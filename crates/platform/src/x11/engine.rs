//! The server's control over its own screen: cursor warp, cursor
//! hide/show, clipboard. Implements [`Engine`] from the core crate.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use kvmshare_core::server::Engine;

/// Clipboard access. `arboard` is a separate object because it manages
/// its own X selection state.
fn new_clipboard(display: Option<&str>) -> Option<arboard::Clipboard> {
    // arboard reads the DISPLAY env var itself; if a custom display was
    // requested we can't honor it through arboard's API, so only try the
    // default. That covers the common case (X11 session).
    if display.is_some() {
        return None;
    }
    arboard::Clipboard::new().ok()
}

/// Server-side engine over an X display.
pub struct X11Engine {
    conn: RustConnection,
    root: x11rb::protocol::xproto::Window,
    clipboard: Option<arboard::Clipboard>,
    /// Last clipboard content applied from a client; pollers skip it to
    /// avoid echoing remote content back.
    last_remote: Option<(String, Vec<u8>)>,
}

impl X11Engine {
    pub fn new(display: Option<&str>) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
        let root = conn.setup().roots[screen_num].root;

        // XFixes >= 2.0 is required for hide/show cursor.
        if conn.extension_information(xfixes::X11_EXTENSION_NAME).map_err(|e| format!("XFixes query: {e}"))?.is_none() {
            return Err("XFixes extension not available".into());
        }
        conn.xfixes_query_version(5, 0).map_err(|e| format!("XFixes version: {e}"))?;

        let clipboard = new_clipboard(display);
        Ok(Self { conn, root, clipboard, last_remote: None })
    }
}

impl Engine for X11Engine {
    fn warp_local(&mut self, x: i32, y: i32) {
        // src_win = NONE warps from the current position.
        let _ = self.conn.warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }

    fn show_local_cursor(&mut self, visible: bool) {
        let res = if visible { xfixes::show_cursor(&self.conn, self.root) } else { xfixes::hide_cursor(&self.conn, self.root) };
        if res.is_ok() {
            let _ = self.conn.flush();
        }
    }

    fn clipboard_set(&mut self, mime: &str, data: &[u8]) {
        // v1 supports plain text. Other mimes are acknowledged and
        // dropped with a warning — see docs/architecture.md.
        if mime != "text/plain" {
            eprintln!("clipboard: ignoring non-text mime {mime:?}");
            return;
        }
        if let Some(cb) = &mut self.clipboard {
            if let Ok(text) = std::str::from_utf8(data) {
                if let Err(e) = cb.set_text(text.to_owned()) {
                    eprintln!("clipboard: set failed: {e}");
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