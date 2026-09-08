//! The server's control over its own screen: cursor warp, cursor
//! hide/show, and local-input isolation. Implements [`Engine`] from the
//! core crate.
//!
//! The engine never *injects* input — it only warps its own cursor.
//! Because `SetCursorPos` is a programmatic warp, it does not generate
//! raw input events, so parking/re-centering the hidden cursor can
//! never feed phantom motion back into the session (the same property
//! the X11 backend gets from XI2 raw events).
//!
//! When the cursor goes to a client the engine also silences this
//! machine's own hardware (keyboard, mouse, touchpad) so the local
//! desktop does not act on the same physical events being forwarded.
//! Windows has no kernel device grab, so isolation uses
//! [`BlockInput`] — which blocks legacy input (WM_MOUSEMOVE,
//! WM_KEYDOWN, …) from reaching applications while raw input (WM_INPUT)
//! keeps flowing to the capture window. The Windows *client* uses
//! low-level hooks for the same purpose; the server cannot, because
//! Raw Input and low-level hooks conflict for the same device types.
//!
//! The clipboard is *not* here: it lives on its own lock as a
//! standalone service (see [`super::clipboard`]), because a clipboard
//! call can block on another process holding the clipboard open and must
//! never serialize with cursor control.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::UI::{Input::KeyboardAndMouse as km, WindowsAndMessaging as wm};

use kvmshare_core::server::Engine;

/// The process-wide BlockInput state. `BlockInput` is process-scoped and
/// can only be toggled by the thread that called it; a static keeps the
/// state honest across the engine's calls.
static BLOCKED: AtomicBool = AtomicBool::new(false);

/// Server-side engine over the local Windows desktop.
pub struct Win32Engine {
    /// True while the local cursor is hidden (between `SwitchTo` and
    /// `SwitchToLocal`). `ShowCursor` is ref-counted, so we only touch it
    /// on transitions to keep the count balanced.
    cursor_hidden: bool,
    blocked: bool,
}

impl Win32Engine {
    pub fn new() -> Self {
        Self {
            cursor_hidden: false,
            blocked: false,
        }
    }

    /// Try to (un)block legacy input system-wide. Best-effort: returns
    /// `true` when the call succeeded and we now own the block state.
    /// Only the thread that blocked can unblock, so this must run on the
    /// same thread every time (the engine is used from one thread in
    /// practice — the main loop's serialized apply path).
    fn set_blocked(&mut self, blocked: bool) -> bool {
        let ok = if blocked {
            // SAFETY: trivial user32 call. Fails if another thread
            // already blocked input (only that thread can unblock).
            unsafe { km::BlockInput(1) != 0 }
        } else {
            unsafe { km::BlockInput(0) != 0 }
        };
        if ok {
            BLOCKED.store(blocked, Ordering::SeqCst);
            self.blocked = blocked;
        }
        ok
    }
}

impl Engine for Win32Engine {
    fn grab_input(&mut self, grabbed: bool) {
        // Grab is a weaker form of isolation (suppress core pointer/
        // keyboard delivery). On Windows, `BlockInput` is the mechanism
        // that actually stops legacy input from reaching local apps while
        // raw input (WM_INPUT) keeps flowing to our capture window. The
        // client uses low-level hooks for the same job — but they conflict
        // with Raw Input on the server, so BlockInput is the server's
        // isolation path.
        self.isolate_input(grabbed);
    }

    fn isolate_input(&mut self, isolated: bool) {
        if isolated {
            // Arm the block. If it fails (another thread owns it, or the
            // desktop is protected), isolation simply does nothing and local
            // input leaks — graceful degradation, not a fatal error.
            let _ = self.set_blocked(true);
        } else {
            // Release the block (same thread that armed it). If we never
            // armed it, nothing to release.
            if self.blocked {
                let _ = self.set_blocked(false);
            }
        }
    }

    fn warp_local(&mut self, x: i32, y: i32) {
        // SAFETY: trivial user32 call; a false return just means the
        // coordinates were invalid, which is harmless to ignore here.
        unsafe {
            wm::SetCursorPos(x, y);
        }
    }

    fn show_local_cursor(&mut self, visible: bool) {
        if visible == self.cursor_hidden {
            // SAFETY: ShowCursor toggles the display count; called only
            // on transitions so the count stays balanced.
            unsafe {
                wm::ShowCursor(visible as i32);
            }
            self.cursor_hidden = !visible;
        }
    }

}