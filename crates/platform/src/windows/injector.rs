//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, hide the local cursor while being controlled,
//! read/write the clipboard. Implements [`Injector`] from the core crate.
//!
//! **Motion is absolute.** The motion stream carries deltas and this
//! injector accumulates them and places the cursor exactly with
//! [`SetCursorPos`] — Windows' pointer acceleration (EPP) never applies
//! to absolute placement, so the shared cursor lands precisely where the
//! server commanded: no OS-curve overshoot, no correction lag, and a
//! dropped placement self-heals. The server compensates for its *own*
//! pointer transform by scaling the counts it sends (its measured
//! px-per-count), so this cursor mirrors the server's cursor
//! pixel-for-pixel. Keys and buttons use scan codes
//! (`KEYEVENTF_SCANCODE`), the layout-independent model: the physical
//! key identity travels and the local layout produces the character.
//!
//! The cursor's commanded position is **clamped to the screen**: the OS
//! pins the visible cursor at the edge, and an unclamped accumulator
//! would keep running off-screen — the user would then have to retrace
//! the whole overshoot before the cursor moved again. Clamping keeps the
//! command where the cursor can actually be, so reversing at an edge
//! moves immediately.

use std::sync::Arc;

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::HiDpi as hidpi;
use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_core::client::Injector;
use std::collections::HashSet;

use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use super::buttons;
use super::isolation::NativeIsolation;

/// The client-side injector over the local Windows desktop.
pub struct Win32Injector {
    /// True while we are hiding the local cursor (between `enter` and
    /// `leave`). Lets `leave` only show the cursor if we hid it.
    cursor_hidden: bool,
    /// The absolute cursor position this injector is steering. Motion is
    /// **absolute**: every `move_rel` delta accumulates here and the
    /// cursor is placed exactly, so Windows' pointer acceleration never
    /// applies to the shared cursor (see [`Win32Injector::move_rel`]).
    pos: (i32, i32),
    /// Keys (HID usages) injected as down and not yet released. `leave`
    /// releases everything still held: the server may never deliver the
    /// matching ups (the user crossed back mid-hold), and the OS must
    /// not be left thinking a key is stuck down.
    keys_down: HashSet<u32>,
    /// Buttons injected as down and not yet released (same contract as
    /// [`Win32Injector::keys_down`]).
    buttons_down: HashSet<u8>,
    /// Silences this machine's own hardware (touchpad, keyboard, mouse)
    /// while its cursor is controlled remotely — the Windows equivalent
    /// of the server's device grab. Process-wide singleton, shared by
    /// every injector (see [`isolation`]).
    isolation: Arc<NativeIsolation>,
}

impl Win32Injector {
    pub fn new() -> Self {
        Self {
            cursor_hidden: false,
            pos: (0, 0),
            keys_down: HashSet::new(),
            buttons_down: HashSet::new(),
            isolation: NativeIsolation::global(),
        }
    }

    /// Restore the machine to its users: undo the hardware silence and
    /// show the cursor again. Runs on whatever thread is dropping the
    /// injector — which is the session thread that hid the cursor — so
    /// the `ShowCursor` count balances exactly. Idempotent; safe to call
    /// when the machine was never isolated.
    fn restore_machine(&mut self) {
        self.isolation.set_isolating(false);
        if self.cursor_hidden {
            // SAFETY: balances the ShowCursor(0) in `enter`.
            unsafe {
                wm::ShowCursor(1);
            }
            self.cursor_hidden = false;
        }
    }

    /// Inject one event. A rejected event (`SendInput` returns 0) is
    /// logged; the cursor itself is steered by `SetCursorPos`, which is
    /// not subject to the input-isolation rules that can swallow
    /// `SendInput` events — so a rejection here never freezes the shared
    /// cursor.
    fn send_input(&mut self, input: &km::INPUT) {
        // SAFETY: callers build a well-formed INPUT for SendInput.
        let accepted =
            unsafe { km::SendInput(1, input, std::mem::size_of::<km::INPUT>() as i32) } == 1;
        if !accepted {
            log_win32_reject();
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

    /// Clamp a screen position to the physical desktop (primary
    /// monitor — the same bounds `screen_info` reports). The OS pins the
    /// visible cursor here, so the commanded position must never leave
    /// it. Degenerate zero-size bounds clamp to (0, 0) instead of
    /// panicking.
    fn clamp(x: i32, y: i32) -> (i32, i32) {
        let (w, h) = Self::screen_size();
        (x.clamp(0, (w - 1).max(0)), y.clamp(0, (h - 1).max(0)))
    }

    /// The real cursor position, in screen pixels.
    fn cursor_pos() -> Option<(i32, i32)> {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos writes one POINT into a valid buffer.
        let got = unsafe { wm::GetCursorPos(&mut pt) } != 0;
        got.then_some((pt.x, pt.y))
    }
}

/// The primary display's geometry the way the injector reports it
/// (physical pixels + DPI scale). A free function so the server can ask
/// for the same numbers before any injector exists — it builds its
/// default layout from this on first run.
pub(super) fn system_screen_info() -> ScreenInfo {
    let (w, h) = Win32Injector::screen_size();
    // SAFETY: GetDpiForSystem is available on Windows 10 1607+;
    // older systems fail and we fall back to 96 (scale 1.0).
    let scale = (unsafe { hidpi::GetDpiForSystem() } as f64 / 96.0) as f32;
    ScreenInfo { width: w.max(0) as u32, height: h.max(0) as u32, scale }
}

impl Injector for Win32Injector {
    fn screen_info(&mut self) -> ScreenInfo {
        system_screen_info()
    }

    fn move_cursor(&mut self, x: i32, y: i32) {
        // Absolute placement (entry points, explicit positioning, and —
        // through the client's tick loop — the whole motion stream).
        // Direct `SetCursorPos`: exact, and immune to the input-isolation
        // drops that `SendInput` is subject to (an elevated foreground
        // window silently eats injected events — the failure mode that
        // froze the shared cursor — while SetCursorPos keeps working).
        let (x, y) = Self::clamp(x, y);
        self.pos = (x, y);
        // SAFETY: SetCursorPos takes screen pixels; hp's cursor follows.
        unsafe {
            wm::SetCursorPos(x, y);
        }
    }

    fn move_rel(&mut self, dx: i32, dy: i32) {
        // Absolute motion: accumulate the delta and place the cursor
        // exactly. `SetCursorPos` bypasses Windows' pointer acceleration
        // (EPP never applies to absolute placement), so the shared cursor
        // lands precisely where the server commanded — no overshoot from
        // the OS curve, no correction lag, and a dropped placement
        // self-heals (the next set lands the whole command). The server
        // compensates for its own pointer transform by scaling the
        // counts it sends, so this cursor mirrors the server's cursor
        // pixel-for-pixel.
        //
        // The command is clamped to the screen: the OS pins the visible
        // cursor at the edge, and an unclamped accumulator would keep
        // running off-screen — the user would then have to retrace the
        // whole overshoot before the cursor moved again. Clamping keeps
        // the command where the cursor can actually be, so reversing at
        // an edge moves immediately.
        let (nx, ny) = Self::clamp(self.pos.0 + dx, self.pos.1 + dy);
        self.pos = (nx, ny);
        // SAFETY: SetCursorPos takes screen pixels; the accumulated
        // position is the command.
        unsafe {
            wm::SetCursorPos(nx, ny);
        }
    }

    fn absolute_motion(&self) -> bool {
        true
    }

    fn steer_heartbeat(&mut self) {
        // Motion-tick liveness signal for the isolation watchdog: while
        // this machine is controlled remotely and the cursor is being
        // steered, the heartbeat stays fresh. If steering stops (a
        // blocked lock, a wedged thread), the watchdog releases local
        // input so this machine is never trapped.
        NativeIsolation::heartbeat();
    }

    fn emergency_release(&mut self) {
        // Called by the client's supervisor when a worker wedged while
        // this machine was being controlled. Restore the machine to its
        // users: undo the hardware silence (the isolation gate) and show
        // the cursor again. Both are idempotent and cannot fail.
        use kvmshare_log::log_warn;
        self.restore_machine();
        log_warn!("injector: local input restored after a client stall");
    }

    fn system_resumed(&mut self) -> bool {
        // Read once per session: the isolation pump marks the resume;
        // the run loop sees it, ends the session, and the reconnect
        // starts fresh and local. The clear (inside `take_resumed`)
        // keeps a new session from inheriting a stale resume.
        super::isolation::take_resumed()
    }

    fn secure_desktop_active(&mut self) -> bool {
        // Live poll, not a latched flag: the run loop asks on every
        // iteration and ends the session for exactly as long as the UAC
        // secure desktop is up. The isolation pump independently releases
        // local input the instant it appears (see isolation.rs), so the
        // person at the machine can answer the prompt either way.
        super::isolation::secure_desktop_active()
    }

    fn cursor_position(&mut self) -> (i32, i32) {
        Self::cursor_pos().unwrap_or(self.pos)
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

    fn enter(&mut self) {
        // Hide our own cursor so the server's stream is the only visible
        // one — the classic KVM "leftover cursor" fix, same as X11.
        // This machine is now driven remotely: its own hardware must not
        // fight the injected stream (see [`isolation`] for the why).
        self.isolation.set_isolating(true);
        if !self.cursor_hidden {
            // SAFETY: ShowCursor(0) decrements the display count.
            unsafe {
                wm::ShowCursor(0);
            }
            self.cursor_hidden = true;
        }
    }

    fn leave(&mut self) {
        // Control is home again: restore this machine's own hardware
        // first, so nothing native is swallowed while we clean up.
        self.isolation.set_isolating(false);
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

}

/// The Windows clipboard, as the client's standalone [`Clipboard`]
/// service. Owns its own lock in the client (`Shared::clipboard`), so a
/// clipboard call that stalls (another process holding the clipboard
/// open) can never freeze the cursor.
pub struct Win32Clipboard {
    inner: crate::windows::clipboard::Clipboard,
}

/// A session ended without `leave` (disconnect, sleep resume, wedge):
/// the machine must never be left input-dead or cursor-invisible. Runs
/// on the session thread that hid the cursor, so the `ShowCursor` count
/// balances exactly. The isolation gate is a process static, so this
/// also covers injectors that never entered.
impl Drop for Win32Injector {
    fn drop(&mut self) {
        self.restore_machine();
    }
}

impl Win32Clipboard {
    pub fn new() -> Self {
        Self { inner: crate::windows::clipboard::Clipboard::new() }
    }
}

impl kvmshare_core::client::Clipboard for Win32Clipboard {
    fn set(&mut self, mime: &str, data: &[u8]) {
        self.inner.set_text(mime, data);
    }
    fn get(&mut self) -> Option<(String, Vec<u8>)> {
        self.inner.get_text()
    }
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        self.inner.last_injected()
    }
}

/// A rejected `SendInput` event. Rare (input-isolated windows); the
/// cursor itself is unaffected because motion and placement use
/// `SetCursorPos`.
fn log_win32_reject() {
    use kvmshare_log::log_warn;
    log_warn!("SendInput rejected an event (input-isolated window?)");
}