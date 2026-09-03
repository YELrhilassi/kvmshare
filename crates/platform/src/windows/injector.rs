//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, hide the local cursor while being controlled,
//! read/write the clipboard. Implements [`Injector`] from the core crate.
//!
//! Injection uses [`SendInput`] with *scan codes* (`KEYEVENTF_SCANCODE`),
//! the same layout-independent model as XTest on X11: the physical key
//! identity travels, and the local keyboard layout produces the
//! character. Mouse moves use absolute screen coordinates (`SetCursorPos`),
//! matching how the server addresses the cursor over the wire.

use windows_sys::Win32::UI::HiDpi as hidpi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::client::Injector;
use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use super::buttons;
use super::clipboard::Clipboard;

/// The client-side injector over the local Windows desktop.
pub struct Win32Injector {
    clipboard: Clipboard,
    /// True while we are hiding the local cursor (between `enter` and
    /// `leave`). Lets `leave` only show the cursor if we hid it.
    cursor_hidden: bool,
}

impl Win32Injector {
    pub fn new() -> Self {
        Self { clipboard: Clipboard::new(), cursor_hidden: false }
    }

    /// Physical screen dimensions for the current DPI (the process is
    /// per-monitor DPI aware, so `GetSystemMetrics` returns physical
    /// pixels).
    fn screen_size() -> (i32, i32) {
        // SAFETY: trivial user32 metrics queries.
        unsafe {
            let w = wm::GetSystemMetrics(wm::SM_CXSCREEN);
            let h = wm::GetSystemMetrics(wm::SM_CYSCREEN);
            (w, h)
        }
    }
}

impl Injector for Win32Injector {
    fn screen_info(&mut self) -> ScreenInfo {
        let (w, h) = Self::screen_size();
        // SAFETY: GetDpiForSystem is available on Windows 10 1607+;
        // older systems fail and we fall back to 96 (scale 1.0).
        let scale = (unsafe { hidpi::GetDpiForSystem() } as f64 / 96.0) as f32;
        ScreenInfo { width: w.max(0) as u32, height: h.max(0) as u32, scale }
    }

    fn move_cursor(&mut self, x: i32, y: i32) {
        // SAFETY: trivial user32 call; out-of-bounds coordinates simply
        // clamp to the nearest edge.
        unsafe {
            wm::SetCursorPos(x, y);
        }
    }

    fn button(&mut self, button: u8, pressed: bool) {
        let Some((flag, xbutton)) = buttons::sendinput_flags(button, pressed) else { return };
        // SAFETY: building a well-formed INPUT union and passing it to
        // SendInput; the union's active field matches INPUT_MOUSE.
        unsafe {
            let mut input: km::INPUT = std::mem::zeroed();
            input.r#type = km::INPUT_MOUSE;
            input.Anonymous.mi.dwFlags = flag;
            if let Some(data) = xbutton {
                input.Anonymous.mi.mouseData = data;
            }
            km::SendInput(1, &input, std::mem::size_of::<km::INPUT>() as i32);
        }
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        let Some(flag) = buttons::wheel_flag(dx, dy) else { return };
        // SAFETY: well-formed wheel event; data carries the notch delta
        // in WHEEL_DELTA units.
        unsafe {
            let mut input: km::INPUT = std::mem::zeroed();
            input.r#type = km::INPUT_MOUSE;
            input.Anonymous.mi.dwFlags = flag;
            input.Anonymous.mi.mouseData = buttons::wheel_data(dx, dy);
            km::SendInput(1, &input, std::mem::size_of::<km::INPUT>() as i32);
        }
    }

    fn key(&mut self, kind: KeyKind, key: u32) {
        // Canonical HID usage -> set-1 scancode (with the E0 extended
        // flag). Unknown usages are dropped: a wrong key would be worse
        // than no key.
        let Some((scan, extended)) = crate::keys::scancode_from_hid(key) else { return };
        let is_press = matches!(kind, KeyKind::Down | KeyKind::Repeat);
        // SAFETY: well-formed KEYBDINPUT with scan-code mode; the
        // layout-independent physical key is what travels.
        unsafe {
            let mut input: km::INPUT = std::mem::zeroed();
            input.r#type = km::INPUT_KEYBOARD;
            input.Anonymous.ki.wVk = 0; // scan-code mode: virtual key unused
            input.Anonymous.ki.wScan = scan;
            let mut flags = km::KEYEVENTF_SCANCODE;
            if extended {
                flags |= km::KEYEVENTF_EXTENDEDKEY;
            }
            if !is_press {
                flags |= km::KEYEVENTF_KEYUP;
            }
            input.Anonymous.ki.dwFlags = flags;
            km::SendInput(1, &input, std::mem::size_of::<km::INPUT>() as i32);
        }
    }

    fn enter(&mut self) {
        // Hide our own cursor so the server's stream is the only visible
        // one — the classic KVM "leftover cursor" fix, same as X11.
        if !self.cursor_hidden {
            // SAFETY: ShowCursor(0) decrements the display count.
            unsafe {
                wm::ShowCursor(0);
            }
            self.cursor_hidden = true;
        }
    }

    fn leave(&mut self) {
        if self.cursor_hidden {
            // SAFETY: balances the hide above.
            unsafe {
                wm::ShowCursor(1);
            }
            self.cursor_hidden = false;
        }
    }

    fn clipboard(&mut self, mime: &str, data: &[u8]) {
        self.clipboard.set_text(mime, data);
    }

    fn clipboard_get(&mut self) -> Option<(String, Vec<u8>)> {
        self.clipboard.get_text()
    }

    fn clipboard_last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        self.clipboard.last_injected()
    }
}