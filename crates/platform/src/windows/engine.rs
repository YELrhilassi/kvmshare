//! The server's control over its own screen: cursor warp, cursor
//! hide/show. Implements [`Engine`] from the core crate.
//!
//! The engine never *injects* input — it only warps its own cursor.
//! Because `SetCursorPos` is a programmatic warp, it does not generate
//! raw input events, so parking/re-centering the hidden cursor can
//! never feed phantom motion back into the session (the same property
//! the X11 backend gets from XI2 raw events). The clipboard is *not*
//! here: it lives on its own lock as a standalone service (see
//! [`super::clipboard`]), because a clipboard call can block on another
//! process holding the clipboard open and must never serialize with
//! cursor control.

use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::server::Engine;

/// Server-side engine over the local Windows desktop.
pub struct Win32Engine {
    /// True while the local cursor is hidden (between `SwitchTo` and
    /// `SwitchToLocal`). `ShowCursor` is ref-counted, so we only touch it
    /// on transitions to keep the count balanced.
    cursor_hidden: bool,
}

impl Win32Engine {
    pub fn new() -> Self {
        Self { cursor_hidden: false }
    }
}

impl Engine for Win32Engine {
    fn grab_input(&mut self, _grabbed: bool) {
        // Suppressing local input while the cursor is on a client needs a
        // low-level hook (WH_MOUSE_LL / WH_KEYBOARD_LL) that swallows
        // events. Not implemented yet — Windows-as-server parity is
        // tracked in docs/roadmap.md. Until then a Windows server that
        // sends its cursor to a client still lets the same input act
        // locally (see the X11 backend for the reference behavior).
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