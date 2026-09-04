//! The server's control over its own screen: cursor warp, cursor
//! hide/show, clipboard. Implements [`Engine`] from the core crate.
//!
//! The engine never *injects* input — it only warps its own cursor and
//! touches its own clipboard. Because `SetCursorPos` is a programmatic
//! warp, it does not generate raw input events, so parking/re-centering
//! the hidden cursor can never feed phantom motion back into the session
//! (the same property the X11 backend gets from XI2 raw events).

use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::server::Engine;

use super::clipboard::Clipboard;

/// Server-side engine over the local Windows desktop.
pub struct Win32Engine {
    clipboard: Clipboard,
    /// True while the local cursor is hidden (between `SwitchTo` and
    /// `SwitchToLocal`). `ShowCursor` is ref-counted, so we only touch it
    /// on transitions to keep the count balanced.
    cursor_hidden: bool,
}

impl Win32Engine {
    pub fn new() -> Self {
        Self { clipboard: Clipboard::new(), cursor_hidden: false }
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

    fn clipboard_set(&mut self, mime: &str, data: &[u8]) {
        self.clipboard.set_text(mime, data);
    }

    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
        self.clipboard.get_text()
    }

    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        self.clipboard.last_injected()
    }
}