//! Engine-command execution on the capture connection: warp, cursor
//! visibility, the input grab, and kernel-level device isolation.
//!
//! Split from the event loop so the grab/isolation state machine (the
//! part of capture that talks to evdev and changes what the desktop
//! sees) reads as one unit.

use std::sync::atomic::Ordering;
use std::time::Instant;

use x11rb::connection::Connection as _;
use x11rb::protocol::xfixes::{self};
use x11rb::protocol::xproto::{self, ConnectionExt as _};

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::{KeyKind, Message};

use super::events::CaptureCommand;
use super::events::{REPEAT_DELAY, REPEAT_INTERVAL};
use super::thread::InputCapture;
use super::thread::BEACON_PERIOD;

impl InputCapture {
    /// Execute one engine command on this connection.
    pub(crate) fn apply(&mut self, cmd: CaptureCommand) {
        match cmd {
            CaptureCommand::Warp(x, y) => {
                // The session commands visible-local pixels; the warp is
                // a root-pixel operation (see [`VisibleDesktop`]).
                // src_win = NONE warps from the current position.
                let (rx, ry) = self.visible.to_root(x, y);
                let _ = self.conn.warp_pointer(
                    x11rb::NONE,
                    self.root,
                    0,
                    0,
                    0,
                    0,
                    rx as i16,
                    ry as i16,
                );
                let _ = self.conn.flush();
            }
            CaptureCommand::CursorVisible(visible) => {
                let res = if visible {
                    xfixes::show_cursor(&self.conn, self.root)
                } else {
                    xfixes::hide_cursor(&self.conn, self.root)
                };
                if res.is_ok() {
                    let _ = self.conn.flush();
                }
            }
            CaptureCommand::Grab(grab) => self.set_grabbed(grab),
            CaptureCommand::IsolateRemote(remote) => {
                // The evdev reader grabs the physical devices at the
                // kernel (X goes fully silent) and starts forwarding;
                // releasing does the reverse and X capture resumes.
                // Also clear the held-key state: once the devices are
                // kernel-grabbed, X never sees the releases of keys that
                // were pressed before (or during) the isolation, so
                // synthesizing repeats for them here would replay stale
                // presses on the client later — the "media keys saved
                // and applied on the client" bug. The evdev reader
                // tracks its own presses instead.
                self.held.clear();
                if remote {
                    // Grab is async: the kernel grab engages within a
                    // couple of ms, before any forwarded event can leak.
                    self.evdev.set_remote(true);
                } else {
                    // Release is SYNCHRONOUS: the next command in this
                    // queue is the entry warp, and it must land on a
                    // live input stream. Waiting here (bounded) means
                    // the physical mouse is already ungrab'd before the
                    // warp — no swallowed motion at the seam.
                    self.evdev.release_and_wait();
                }
            }
        }
    }

    /// Grab (or release) the pointer and keyboard on this connection.
    ///
    /// While grabbed, physical input is redirected to us and the local
    /// desktop sees nothing of it; the raw stream — which the session
    /// actually consumes — is unaffected by grabs, so forwarding keeps
    /// working. A failed grab (another client holds one, e.g. a window
    /// manager popup) is logged and tolerated: input still forwards, only
    /// the local-echo suppression is lost until the grab succeeds.
    pub(crate) fn set_grabbed(&mut self, grab: bool) {
        if grab == self.grabbed.load(Ordering::Relaxed) {
            return;
        }
        let ok = if grab {
            let mask = xproto::EventMask::BUTTON_PRESS
                | xproto::EventMask::BUTTON_RELEASE
                | xproto::EventMask::POINTER_MOTION;
            // Each step fails as `None` on transport or reply errors.
            let pointer = self
                .conn
                .grab_pointer(
                    false,
                    self.root,
                    mask,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                    x11rb::NONE,
                    x11rb::NONE,
                    x11rb::CURRENT_TIME,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.status == xproto::GrabStatus::SUCCESS);
            let keyboard = self
                .conn
                .grab_keyboard(
                    false,
                    self.root,
                    x11rb::CURRENT_TIME,
                    xproto::GrabMode::ASYNC,
                    xproto::GrabMode::ASYNC,
                )
                .ok()
                .and_then(|c| c.reply().ok())
                .map(|r| r.status == xproto::GrabStatus::SUCCESS);
            match (pointer, keyboard) {
                (Some(true), Some(true)) => true,
                (p, k) => {
                    log_warn!("input grab not acquired (pointer: {p:?}, keyboard: {k:?})");
                    false
                }
            }
        } else {
            let _ = self.conn.ungrab_pointer(x11rb::CURRENT_TIME);
            let _ = self.conn.ungrab_keyboard(x11rb::CURRENT_TIME);
            let _ = self.conn.flush();
            false
        };
        self.grabbed
            .store(if grab { ok } else { false }, Ordering::Relaxed);
        if self.grabbed.load(Ordering::Relaxed) {
            log_debug!("local input grabbed (cursor is on a client)");
        } else if !grab {
            log_debug!("local input released");
        }
    }

    /// Forward the coalesced position beacon at [`BEACON_PERIOD`]
    /// cadence, if one is pending. Called from the poll loop, so a beacon
    /// is never delayed longer than one period after the pointer stops
    /// (the loop wakes at the beacon's cadence while one is pending) —
    /// edge parks are confirmed to the session within ~8 ms even under
    /// load.
    pub(crate) fn flush_beacon(&mut self) {
        let Some((x, y)) = self.beacon else { return };
        let now = Instant::now();
        let due = match self.last_beacon {
            Some(t) => now.duration_since(t) >= BEACON_PERIOD,
            None => true,
        };
        if !due {
            return;
        }
        self.last_beacon = Some(now);
        self.beacon = None;
        self.send(Message::MouseMoveAbs { x, y });
    }

    /// Synthesize auto-repeat for physically held keys. Raw events carry
    /// no repeats (they are device transitions only), so clients would
    /// otherwise see a single press for a held key.
    pub(crate) fn tick_repeats(&mut self) {
        if self.held.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due: Vec<u32> = Vec::new();
        for (key, h) in self.held.iter_mut() {
            if now.duration_since(h.down_at) >= REPEAT_DELAY
                && now.duration_since(h.last_repeat) >= REPEAT_INTERVAL
            {
                h.last_repeat = now;
                due.push(*key);
            }
        }
        for key in due {
            self.send(Message::Key {
                kind: KeyKind::Repeat,
                key,
            });
        }
    }

    pub(crate) fn send(&self, msg: Message) {
        // The channel is unbounded; the server main loop drains it at its
        // own pace. If the receiver is gone (shutdown), drop the message.
        let _ = self.tx.send(msg);
    }
}
