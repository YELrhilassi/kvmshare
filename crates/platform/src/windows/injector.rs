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
//!
//! Blocked-input detection (elevated/input-isolated windows such as Task
//! Manager or the on-screen keyboard silently drop injected input) never
//! runs in the move hot path. Moves are injected unconditionally and the
//! client loop asks [`Injector::input_blocked`] on its own slow cadence;
//! that one call does a single non-blocking cursor-position check
//! against the latest injected target — SendInput returns success even
//! when UIPI drops the event, so the real cursor is the only truthful
//! signal.

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
/// check failing, so this trips ~0.4 s in.
const BLOCK_LINGER: Duration = Duration::from_millis(400);

/// How far the real cursor may be from the requested position and still
/// count as arrived (polling jitter, rounding in the 0..65535 mapping).
const VERIFY_TOLERANCE: i32 = 3;

/// The client-side injector over the local Windows desktop.
pub struct Win32Injector {
    clipboard: Clipboard,
    /// True while we are hiding the local cursor (between `enter` and
    /// `leave`). Lets `leave` only show the cursor if we hid it.
    cursor_hidden: bool,
    /// The last position this injector asked the OS to place the cursor
    /// at. [`Win32Injector::check_cursor_position`] compares the real
    /// cursor against it. Moves are *unconditional* in the hot path, so
    /// the target is remembered here instead of being read back from the
    /// OS mid-stream.
    last_target: (i32, i32),
    /// When the cursor first failed to arrive where we asked (or
    /// SendInput rejected an event outright). `None` while input flows.
    /// A blocking window keeps every check failing, so once this has
    /// lingered past [`BLOCK_LINGER`] the core loop tells the server to
    /// bring control home.
    miss_since: Option<Instant>,
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
            last_target: (0, 0),
            miss_since: None,
            keys_down: HashSet::new(),
            buttons_down: HashSet::new(),
        }
    }

    /// Inject one event.
    ///
    /// [`SendInput`](km::SendInput) returning 0 means the OS rejected
    /// injected input outright (rare); its return value cannot detect
    /// UIPI-dropped delivery, so the cursor-position check in
    /// [`Win32Injector::input_blocked`] is the real signal. A rejection
    /// is treated as an immediate miss so recovery is not blocked on the
    /// next position check.
    fn send_input(&mut self, input: &km::INPUT) {
        // SAFETY: callers build a well-formed INPUT for SendInput.
        let accepted =
            unsafe { km::SendInput(1, input, std::mem::size_of::<km::INPUT>() as i32) } == 1;
        if !accepted {
            self.miss_since.get_or_insert(Instant::now());
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

    /// One *non-blocking* check of whether the real cursor is where the
    /// last injected move asked it to be, updating [`Win32Injector::miss_since`].
    ///
    /// Runs only from [`Win32Injector::input_blocked`], on the client
    /// loop's block cadence — never from the move hot path, where
    /// polling or waiting would stall the whole input pipeline.
    fn check_cursor_position(&mut self) {
        let (x, y) = self.last_target;
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos writes one POINT into a valid buffer.
        let got = unsafe { wm::GetCursorPos(&mut pt) } != 0;
        let arrived = got && (pt.x - x).abs() <= VERIFY_TOLERANCE && (pt.y - y).abs() <= VERIFY_TOLERANCE;
        if arrived {
            self.miss_since = None;
        } else {
            self.miss_since.get_or_insert(Instant::now());
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
        // The move hot path stays pure: no polling, no waiting. Blocked
        // detection happens on the block-check cadence in
        // `input_blocked`.
        self.last_target = (x, y);
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
        // One cheap, non-blocking position check against the latest
        // injected target, then test the linger. SendInput returns
        // success even when UIPI drops the input at delivery
        // (elevated/input-isolated windows), so only the cursor itself
        // tells the truth. Runs here — on the client loop's block
        // cadence, never in the move path.
        self.check_cursor_position();
        self.miss_since.is_some_and(|t| t.elapsed() >= BLOCK_LINGER)
    }

    fn enter(&mut self) {
        // Hide our own cursor so the server's stream is the only visible
        // one — the classic KVM "leftover cursor" fix, same as X11.
        // Control starts clean: whatever happened before `enter`, the
        // cursor has now been placed by the server, so a stale miss must
        // not immediately yank control home.
        self.miss_since = None;
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
        self.miss_since = None;
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
