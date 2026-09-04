//! The server's control over its own screen: cursor warp, cursor
//! hide/show, input grab, clipboard. Implements [`Engine`] from the core
//! crate.
//!
//! Cursor *control* (warp, hide/show, grab) is delegated to the capture
//! thread over [`CaptureCommand`]s — every action on the physical cursor
//! must run on the connection that holds the pointer grab, or the X
//! server would ignore warps issued by a different client. The clipboard
//! is reached through `arboard`, which manages its own selection
//! connection, so this engine holds no X connection of its own (the
//! capture connection validates XFixes at startup).

use kvmshare_core::server::Engine;
use kvmshare_log::log_warn;

use super::capture::CaptureCommand;

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
    /// Where cursor-control commands go (the capture thread executes them
    /// on its own connection — see the module docs).
    cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
    clipboard: Option<arboard::Clipboard>,
    /// Last clipboard content applied from a client; pollers skip it to
    /// avoid echoing remote content back.
    last_remote: Option<(String, Vec<u8>)>,
}

impl X11Engine {
    pub fn new(
        display: Option<&str>,
        cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
    ) -> Result<Self, String> {
        let clipboard = new_clipboard(display);
        Ok(Self { cmd_tx, clipboard, last_remote: None })
    }
}

impl Engine for X11Engine {
    fn warp_local(&mut self, x: i32, y: i32) {
        // Executed on the capture connection (the grab owner).
        let _ = self.cmd_tx.send(CaptureCommand::Warp(x, y));
    }

    fn grab_input(&mut self, grabbed: bool) {
        // Pointer/keyboard grab lives on the capture connection so that
        // connection can keep warping the cursor while it holds the grab.
        let _ = self.cmd_tx.send(CaptureCommand::Grab(grabbed));
    }

    fn isolate_input(&mut self, isolated: bool) {
        // Kernel-level device isolation (evdev reader) — see
        // `CaptureCommand::IsolateRemote`. Best-effort on the capture
        // connection like every other cursor control.
        let _ = self.cmd_tx.send(CaptureCommand::IsolateRemote(isolated));
    }

    fn show_local_cursor(&mut self, visible: bool) {
        // Also executed on the capture connection (same reason as warp).
        let _ = self.cmd_tx.send(CaptureCommand::CursorVisible(visible));
    }

    fn clipboard_set(&mut self, mime: &str, data: &[u8]) {
        // v1 supports plain text. Other mimes are acknowledged and
        // dropped with a warning — see docs/architecture.md.
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
