//! The server's control over its own screen: cursor warp, cursor
//! hide/show, and local-input isolation.
//!
//! Two of the cursor-control APIs involved are thread-affine:
//!
//! * **`ShowCursor`**'s display count is per-thread: hiding on one
//!   thread and showing on another never balances, and the cursor stays
//!   hidden. Routing every hide/show through the engine thread keeps the
//!   count balanced.
//! * The warp (`SetCursorPos`) must not race a hide/show on another
//!   thread, so it runs here too, in order with the rest.
//!
//! The engine therefore forwards every command over a channel to its
//! own thread, which executes them in order — the same single-owner
//! pattern the X11 backend uses (its capture thread executes every
//! engine command on the connection that holds the grab).
//!
//! **Isolation is not here.** It lives in the capture (see
//! [`super::capture`]): the capture thread's low-level hooks swallow
//! every input event while an atomic flag is set, so the local desktop
//! is inert the moment the cursor crosses to a client while the hooks
//! themselves keep capturing — and the flag is a plain store, flipped
//! synchronously on the crossing path. `BlockInput` was abandoned for
//! this role after hardware testing showed it suppresses the raw input
//! the capture depended on: the crossing fired, the block armed, and
//! the capture went deaf — no motion reached the client and nothing
//! could bring the cursor home. Hooks suppress raw input too, but the
//! hook procedures see every event regardless, so capture survives the
//! swallow (see the capture module docs for the measured evidence).
//!
//! The clipboard is *not* here: it lives on its own lock as a
//! standalone service (see [`super::clipboard`]), because a clipboard
//! call can block on another process holding the clipboard open and must
//! never serialize with cursor control.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::server::Engine;

/// One cursor-control command for the engine thread.
enum EngineCmd {
    /// Hide (`false`) or show (`true`) the local cursor. `ShowCursor` is
    /// ref-counted per thread, so it is only touched on actual
    /// transitions to keep the count balanced.
    CursorVisible(bool),
    /// Warp the local cursor to screen pixels `(x, y)`.
    Warp(i32, i32),
}

/// Server-side engine over the local Windows desktop.
pub struct Win32Engine {
    /// Commands for the engine thread. Dropped with the engine, which
    /// ends the thread (the channel's receiver sees the disconnect).
    cmd_tx: mpsc::Sender<EngineCmd>,
    /// The capture's isolation flag: while true, the capture thread's
    /// low-level hooks swallow local input. Flipped synchronously here
    /// (a plain store — no thread hop), so the isolation decision takes
    /// effect on the very next input event.
    isolate: Arc<AtomicBool>,
}

impl Win32Engine {
    pub fn new(isolate: Arc<AtomicBool>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        thread::Builder::new()
            .name("kvmshare-engine".into())
            .spawn(move || engine_loop(cmd_rx))
            .expect("cannot spawn Windows engine thread");
        Self { cmd_tx, isolate }
    }

    /// Queue one command. Nonblocking: the channel is unbounded and the
    /// receiver drains it continuously, so a crossing costs one send and
    /// never waits on the OS.
    fn send(&self, cmd: EngineCmd) {
        let _ = self.cmd_tx.send(cmd);
    }
}

impl Engine for Win32Engine {
    fn grab_input(&mut self, grabbed: bool) {
        // Grab is a weaker form of isolation (suppress core pointer/
        // keyboard delivery). On Windows the low-level hooks installed
        // by the capture are the mechanism that actually stops input
        // from reaching local apps — see the module docs. `BlockInput`
        // was replaced by them because it silenced the raw input the
        // capture depended on (the crossing fired, then the capture went
        // deaf; see `super::capture` for the measured evidence).
        self.isolate_input(grabbed);
    }

    fn isolate_input(&mut self, isolated: bool) {
        // A plain store on the shared flag: the capture thread's hooks
        // read it on every event and swallow while it is set. No thread
        // hop, no API that can fail — the crossing path stays
        // frictionless. The hooks die with the process or the capture
        // thread, so a crash or a wedged capture (which the server
        // supervisor detects and exits from) releases local input
        // automatically — this machine is never left input-trapped.
        self.isolate.store(isolated, Ordering::SeqCst);
    }

    fn warp_local(&mut self, x: i32, y: i32) {
        // Executed on the engine thread, in order with any cursor
        // visibility change.
        self.send(EngineCmd::Warp(x, y));
    }

    fn show_local_cursor(&mut self, visible: bool) {
        self.send(EngineCmd::CursorVisible(visible));
    }
}

/// The engine thread: the single owner of the thread-affine Windows
/// cursor calls. Executes commands in order; best-effort throughout — a
/// failed call just means a cosmetic hiccup, never a fatal error.
fn engine_loop(rx: mpsc::Receiver<EngineCmd>) {
    // Whether the local cursor is currently hidden, as far as this
    // thread's `ShowCursor` count is concerned.
    let mut cursor_hidden = false;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            EngineCmd::CursorVisible(v) => {
                // Act only on transitions so the per-thread display
                // count stays balanced: hide when currently shown, show
                // when currently hidden.
                if v == cursor_hidden {
                    // SAFETY: ShowCursor toggles the display count;
                    // called only on transitions, always from this
                    // thread, so the count stays balanced.
                    unsafe {
                        wm::ShowCursor(v as i32);
                    }
                    cursor_hidden = !v;
                }
            }
            EngineCmd::Warp(x, y) => {
                // SAFETY: SetCursorPos takes screen pixels; a false
                // return just means the coordinates were invalid, which
                // is harmless to ignore here.
                unsafe {
                    wm::SetCursorPos(x, y);
                }
            }
        }
    }
}