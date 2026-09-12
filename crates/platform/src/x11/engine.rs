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

use std::io::Write;
use std::os::unix::net::UnixStream;

use kvmshare_core::server::Engine;

use super::capture::CaptureCommand;

/// Server-side engine over an X display.
pub struct X11Engine {
    /// Where cursor-control commands go (the capture thread executes them
    /// on its own connection — see the module docs).
    cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
    /// Write end of the capture loop's wake pipe: every command writes a
    /// byte here so the loop's poll(2) returns immediately instead of
    /// sleeping through the command.
    wake: UnixStream,
}

impl X11Engine {
    pub fn new(
        display: Option<&str>,
        cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
        wake: UnixStream,
    ) -> Result<Self, String> {
        let _ = display; // the display is validated by capture::start
        Ok(Self { cmd_tx, wake })
    }

    /// Send one command and wake the capture loop out of its idle poll.
    /// Nonblocking throughout: a full wake pipe drops the nudge, never
    /// the command (which already travelled over the channel).
    fn send(&self, cmd: CaptureCommand) {
        let _ = self.cmd_tx.send(cmd);
        let mut w = &self.wake;
        let _ = w.write(&[1]);
    }
}

impl Engine for X11Engine {
    fn warp_local(&mut self, x: i32, y: i32) {
        // Executed on the capture connection (the grab owner).
        self.send(CaptureCommand::Warp(x, y));
    }

    fn grab_input(&mut self, grabbed: bool) {
        // Pointer/keyboard grab lives on the capture connection so that
        // connection can keep warping the cursor while it holds the grab.
        self.send(CaptureCommand::Grab(grabbed));
    }

    fn isolate_input(&mut self, isolated: bool) {
        // Kernel-level device isolation (evdev reader) — see
        // `CaptureCommand::IsolateRemote`. Best-effort on the capture
        // connection like every other cursor control.
        self.send(CaptureCommand::IsolateRemote(isolated));
    }

    fn show_local_cursor(&mut self, visible: bool) {
        // Also executed on the capture connection (same reason as warp).
        self.send(CaptureCommand::CursorVisible(visible));
    }

    fn set_bound_chords(&mut self, chords: Vec<(u8, u32)>) {
        // Passive chord grabs live on the capture connection, like every
        // other cursor/input control — see
        // `CaptureCommand::BindChords` for the mechanism.
        self.send(CaptureCommand::BindChords(chords));
    }
}
