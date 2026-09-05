//! The server's control over its own screen: cursor warp, cursor
//! hide/show, input grab. Implements [`Engine`] from the core crate.
//!
//! Cursor *control* (warp, hide/show, grab) is delegated to the capture
//! thread over [`CaptureCommand`]s — every action on the physical cursor
//! must run on the connection that holds the pointer grab, or the X
//! server would ignore warps issued by a different client. The
//! clipboard is *not* here: it lives on its own lock as a standalone
//! service (see [`super::clipboard`]), because a clipboard call can
//! block on the selection owner and must never serialize with cursor
//! control.

use kvmshare_core::server::Engine;

use super::capture::CaptureCommand;

/// Server-side engine over an X display.
pub struct X11Engine {
    /// Where cursor-control commands go (the capture thread executes them
    /// on its own connection — see the module docs).
    cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
}

impl X11Engine {
    pub fn new(display: Option<&str>, cmd_tx: std::sync::mpsc::Sender<CaptureCommand>) -> Result<Self, String> {
        let _ = display; // the display is validated by capture::start
        Ok(Self { cmd_tx })
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

}
