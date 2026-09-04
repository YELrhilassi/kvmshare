//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, hide the local cursor while being controlled,
//! read/write the clipboard. Implements [`Injector`] from the core crate.
//!
//! Injection uses [`SendInput`] with *scan codes* (`KEYEVENTF_SCANCODE`),
//! the same layout-independent model as XTest on X11: the physical key
//! identity travels, and the local keyboard layout produces the
//! character. Mouse moves are absolute SendInput events too (see
//! [`Injector::move_cursor`]) so buttons, moves and releases all flow
//! through the same input queue.

use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::HiDpi as hidpi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::client::Injector;
use std::collections::HashSet;

use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use super::buttons;
use super::clipboard::Clipboard;

/// How long injected input must keep failing before the client reports
/// itself blocked. One dropped event (a queue hiccup) must not yank
/// control home; a blocking window that stays foreground keeps every
/// event failing, so this trips ~0.4 s in.
const BLOCK_LINGER: Duration = Duration::from_millis(400);

/// Every Nth injected cursor move is verified against the real cursor
/// position. [`SendInput`](km::SendInput) reports *success* even when
/// Windows drops the input at delivery (UIPI: an elevated or
/// input-isolated window — Task Manager, an on-screen keyboard, a UAC
/// prompt — filters injected input silently), so its return value cannot
/// detect a freeze. Only the cursor itself tells the truth: if it stops
/// arriving at the requested position, input is blocked. At ~250 moves/s
/// this verifies ~20x per second — a couple of ms of polling amortized
/// over the batch.
const VERIFY_EVERY: u32 = 12;

/// How far the real cursor may be from the requested position and still
/// count as arrived (polling jitter, rounding in the 0..65535 mapping).
const VERIFY_TOLERANCE: i32 = 3;

/// The client-side injector over the local Windows desktop.
pub struct Win32Injector {
    clipboard: Clipboard,
    /// True while we are hiding the local cursor (between `enter` and
    /// `leave`). Lets `leave` only show the cursor if we hid it.
    cursor_hidden: bool,
    /// When the OS last refused to move the cursor to where we asked
    /// (see [`Win32Injector::verify_cursor`]). `None` while input flows.
    /// Windows drops injected input while an elevated or input-isolated
    /// window (UAC prompt, on-screen keyboard, Task Manager, an admin
    /// tool) is foreground — the cursor freezes and the user would be
    /// trapped, so this feeds [`Injector::input_blocked`] and the server
    /// brings control home.
    blocked_since: Option<Instant>,
    /// Moves injected since the last position verification.
    moves_since_verify: u32,
    /// Keys (HID usages) injected as down and not yet released. `leave`
    /// releases everything still held: the server may never deliver the
    /// matching ups (the user crossed back mid-hold), and the OS must
    /// not be left thinking a key is stuck down.
    keys_down: HashSet<u32>,
    /// Buttons injected as down and not yet released (same contract as
    /// [`Win32Injector::keys_down`]).
    buttons_down: HashSet<u8>,
}

impl Win32Injector {
    pub fn new() -> Self {
        Self {
            clipboard: Clipboard::new(),
            cursor_hidden: false,
            blocked_since: None,
            moves_since_verify: 0,
            keys_down: HashSet::new(),
            buttons_down: HashSet::new(),
        }
    }

    /// Inject one event, tracking whether the OS accepted it.
    ///
    /// [`SendInput`](km::SendInput) returns the number of events it
    /// inserted; 0 means the foreground window (or the session) rejected
    /// injected input — on Windows that happens with elevated or
    /// input-isolated windows (UIPI), e.g. an on-screen keyboard or an
    /// admin tool. Any accepted event clears the tracker, and since
    /// cursor moves arrive at up to ~250/s while the user moves, input
    /// is detected as flowing again almost the instant the blocking
    /// window stops being foreground.
    fn send_input(&mut self, input: &km::INPUT) {
        // SAFETY: callers build a well-formed INPUT for SendInput.
        let accepted =
            unsafe { km::SendInput(1, input, std::mem::size_of::<km::INPUT>() as i32) } == 1;
        let now = Instant::now();
        if accepted {
            self.blocked_since = None;
        } else {
            self.blocked_since.get_or_insert(now);
        }
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

    /// Confirm the real cursor arrived where the last injected move asked
    /// it to be.
    ///
    /// SendInput succeeds even when UIPI will drop the event, so the only
    /// reliable signal is the cursor itself. Poll the real position for a
    /// few milliseconds (the OS may land the cursor a moment after the
    /// call); if it never arrives, the injector is blocked and the core
    /// loop will tell the server to bring control home.
    fn verify_cursor(&mut self, x: i32, y: i32) {
        let deadline = Instant::now() + Duration::from_millis(4);
        let mut arrived = false;
        loop {
            let mut pt = POINT { x: 0, y: 0 };
            // SAFETY: GetCursorPos writes one POINT into a valid buffer.
            let got = unsafe { wm::GetCursorPos(&mut pt) } != 0;
            if !got {
                break; // no cursor to move — treat as blocked
            }
            if (pt.x - x).abs() <= VERIFY_TOLERANCE && (pt.y - y).abs() <= VERIFY_TOLERANCE {
                arrived = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let now = Instant::now();
        if arrived {
            self.blocked_since = None;
        } else {
            self.blocked_since.get_or_insert(now);
        }
    }
}

/// Map a physical pixel coordinate to SendInput's 16-bit absolute space
/// (0..65535 across the primary monitor), clamped to the screen.
fn normalize(v: i32, limit: i32) -> i32 {
    let limit = limit.max(1);
    let v = v.clamp(0, limit - 1);
    ((v as u64 * 65535) / (limit - 1) as u64) as i32
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
        // Moves go through SendInput as *absolute* input, not SetCursorPos.
        // Mixing SetCursorPos (a direct cursor placement, outside the
        // input queue) with SendInput button events makes drags and
        // double-clicks unreliable: the OS derives those from coherent
        // sequences of queued input. Routing every event — down, moves,
        // up — through the same queue makes the client behave like a
        // real device. Absolute SendInput coordinates are 0..65535 over
        // the primary monitor's pixel space.
        let (w, h) = Self::screen_size();
        let nx = normalize(x, w);
        let ny = normalize(y, h);
        // SAFETY: well-formed absolute-move INPUT event.
        let mut input: km::INPUT = unsafe { std::mem::zeroed() };
        input.r#type = km::INPUT_MOUSE;
        input.Anonymous.mi.dwFlags = km::MOUSEEVENTF_MOVE | km::MOUSEEVENTF_ABSOLUTE;
        input.Anonymous.mi.dx = nx;
        input.Anonymous.mi.dy = ny;
        self.send_input(&input);
        // Verify every Nth move (see [`VERIFY_EVERY`]). SendInput's return
        // value cannot detect UIPI-dropped input, so we check the real
        // cursor position on a cadence instead.
        self.moves_since_verify += 1;
        if self.moves_since_verify >= VERIFY_EVERY {
            self.moves_since_verify = 0;
            self.verify_cursor(x, y);
        }
    }

    fn button(&mut self, button: u8, pressed: bool) {
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A release for a button we did not press
        // is dropped (its press happened on the server's machine).
        if pressed {
            self.buttons_down.insert(button);
        } else if !self.buttons_down.remove(&button) {
            return;
        }
        let Some((flag, xbutton)) = buttons::sendinput_flags(button, pressed) else { return };
        // SAFETY: building a well-formed INPUT union and passing it to
        // SendInput; the union's active field matches INPUT_MOUSE.
        let mut input: km::INPUT = unsafe { std::mem::zeroed() };
        input.r#type = km::INPUT_MOUSE;
        input.Anonymous.mi.dwFlags = flag;
        if let Some(data) = xbutton {
            input.Anonymous.mi.mouseData = data;
        }
        self.send_input(&input);
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        let Some(flag) = buttons::wheel_flag(dx, dy) else { return };
        // SAFETY: well-formed wheel event; data carries the notch delta
        // in WHEEL_DELTA units.
        let mut input: km::INPUT = unsafe { std::mem::zeroed() };
        input.r#type = km::INPUT_MOUSE;
        input.Anonymous.mi.dwFlags = flag;
        input.Anonymous.mi.mouseData = buttons::wheel_data(dx, dy);
        self.send_input(&input);
    }

    fn key(&mut self, kind: KeyKind, key: u32) {
        // Canonical HID usage -> set-1 scancode (with the E0 extended
        // flag). Unknown usages are dropped: a wrong key would be worse
        // than no key.
        let Some((scan, extended)) = crate::keys::scancode_from_hid(key) else { return };
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A repeat for a key we did not press (it
        // was held across the boundary, pressed on the server) must not
        // start a press here.
        match kind {
            KeyKind::Down => {
                self.keys_down.insert(key);
            }
            KeyKind::Up => {
                self.keys_down.remove(&key);
            }
            KeyKind::Repeat => {
                if !self.keys_down.contains(&key) {
                    return;
                }
            }
        }
        let is_press = matches!(kind, KeyKind::Down | KeyKind::Repeat);
        // SAFETY: well-formed KEYBDINPUT with scan-code mode; the
        // layout-independent physical key is what travels.
        let mut input: km::INPUT = unsafe { std::mem::zeroed() };
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
        self.send_input(&input);
    }

    fn input_blocked(&mut self) -> bool {
        self.blocked_since.is_some_and(|t| t.elapsed() >= BLOCK_LINGER)
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
        // Control left this machine: release every key and button we
        // injected and never saw released — the user may have crossed
        // back mid-hold, so the matching ups will never arrive. Without
        // this the OS would keep the key held (auto-repeating into the
        // foreground app) until the same key was pressed and released
        // again.
        let keys: Vec<u32> = self.keys_down.drain().collect();
        for key in keys {
            self.key(KeyKind::Up, key);
        }
        let buttons: Vec<u8> = self.buttons_down.drain().collect();
        for button in buttons {
            self.button(button, false);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_pixels_to_16bit_space() {
        // 1920x1080 screen: the corners and midpoints land where
        // SendInput expects them.
        assert_eq!(normalize(0, 1920), 0);
        assert_eq!(normalize(1919, 1920), 65535);
        assert_eq!(normalize(960, 1920), 32768);
        assert_eq!(normalize(540, 1080), 32768);
        // Out of range clamps, never wraps.
        assert_eq!(normalize(-50, 1920), 0);
        assert_eq!(normalize(5000, 1920), 65535);
        assert_eq!(normalize(0, 1), 0); // degenerate screen
    }
}