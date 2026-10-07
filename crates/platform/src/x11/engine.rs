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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kvmshare_core::server::Engine;

use super::capture::CaptureCommand;

/// How long [`Engine::set_media_capture`] waits for the capture thread to
/// answer. The capture loop is woken by a byte on the wake pipe, so a
/// healthy round-trip is microseconds; the bound exists so a wedged capture
/// thread delays a policy change instead of hanging the input path. The
/// supervisor is already watching that thread, so a timeout here is
/// diagnosis, not recovery.
const MEDIA_REPLY_TIMEOUT: Duration = Duration::from_millis(250);

/// Server-side engine over an X display.
pub struct X11Engine {
    /// Where cursor-control commands go (the capture thread executes them
    /// on its own connection — see the module docs).
    cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
    /// Write end of the capture loop's wake pipe: every command writes a
    /// byte here so the loop's poll(2) returns immediately instead of
    /// sleeping through the command.
    wake: UnixStream,
    /// Whether the media keys are suppressed, maintained by the capture
    /// thread — the connection that holds the grabs is the only one that
    /// can say so truthfully.
    media_active: Arc<AtomicBool>,
}

impl X11Engine {
    pub fn new(
        display: Option<&str>,
        cmd_tx: std::sync::mpsc::Sender<CaptureCommand>,
        wake: UnixStream,
        media_active: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let _ = display; // the display is validated by capture::start
        Ok(Self { cmd_tx, wake, media_active })
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

    fn set_media_capture(&mut self, active: bool) -> Result<(), String> {
        // Unlike every other command here this one waits for an answer.
        // The caller (the routing policy) has to know whether suppression
        // is really in force: it is the difference between "media keys are
        // routed" and "media keys are routed *and* also act here", and
        // guessing wrong makes the log line a lie in whichever direction
        // the guess failed. A policy change is a human action, so a
        // microsecond round-trip on it costs nothing on the input path.
        let (reply_tx, reply_rx) = std::sync::mpsc::sync_channel(0);
        self.send(CaptureCommand::MediaCapture(active, reply_tx));
        match reply_rx.recv_timeout(MEDIA_REPLY_TIMEOUT) {
            Ok(result) => result,
            Err(_) => Err(format!(
                "input capture did not answer within {MEDIA_REPLY_TIMEOUT:?}"
            )),
        }
    }

    fn media_capture_active(&self) -> bool {
        self.media_active.load(Ordering::Acquire)
    }

    fn media(&mut self, command: kvmshare_protocol::message::MediaCommand) {
        // Fire-and-forget: the tap is performed on the capture connection
        // (the one that has to lift the grabs for it), and nothing on the
        // input path waits for a media key to be delivered.
        self.send(CaptureCommand::Media(command));
    }
}
