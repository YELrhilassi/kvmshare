//! Local input capture via Win32 **Raw Input**.
//!
//! A hidden message-only window registers for raw mouse and keyboard
//! input (`RIDEV_INPUTSINK | RIDEV_DEVNOTIFY`), so events arrive even
//! when the desktop's foreground window is something else. The message
//! loop lives on a background thread and forwards what matters as
//! protocol [`Message`]s.
//!
//! Raw input has the same two properties the X11 backend gets from XI2
//! raw events:
//!
//! * **No warp feedback** — programmatic `SetCursorPos` (park/warp)
//!   does not generate raw input, so the hidden local cursor can roam
//!   without feeding phantom motion into the session.
//! * **Hardware deltas** — relative motion arrives as the device's own
//!   `lLastX`/`lLastY`, already in whole pixels.
//!
//! Keyboard auto-repeat is *not* suppressed by raw input (unlike XI2
//! raw events), so presses are deduplicated per key: only true
//! down/up transitions are forwarded, preventing stuck keys on the
//! receiving machine.
//!
//! ## Position beacons
//!
//! Raw input carries only *deltas* — the session's boundary model needs
//! the **real** cursor position too (see `core::session`: a crossing is
//! armed by a beacon placing the visible cursor on a screen wall, and raw
//! deltas alone must never fire one). The capture therefore polls
//! `GetCursorPos` on a timer ([`BEACON_MS`]) and forwards each *changed*
//! position as a [`Message::MouseMoveAbs`] beacon — mirroring the X11
//! backend exactly, where ordinary XI motion events stop arriving once
//! the pointer is pinned at a wall (so the last beacon before the pin is
//! the one that arms it).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::Input as ri;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_log::{log_error, log_info};
use kvmshare_protocol::message::{KeyKind, Message};

use super::buttons;

/// Usage page / usages for the devices we capture.
const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_KEYBOARD: u16 = 0x06;

/// Window class name for the hidden capture window.
const CAPTURE_CLASS: &[u16] = &[
    'K' as u16, 'V' as u16, 'M' as u16, 'S' as u16, 'H' as u16, 'A' as u16, 'R' as u16, 'E' as u16, 0,
];

/// How often the position beacon polls `GetCursorPos` (ms). Same order
/// as the X11 capture's coalesced beacons (~6 ms + poll slack): tight
/// enough that edge crossings arm promptly, sparse enough to stay
/// negligible. Beacons are only forwarded when the position *changed*, so
/// an idle desktop sends nothing.
const BEACON_MS: usize = 8;
/// The timer id for the position beacon poll.
const BEACON_TIMER: usize = 1;

/// Tracks the last absolute mouse position so absolute-mode deltas can
/// be computed (raw input can report either mode depending on the
/// device/driver).
#[derive(Default)]
struct AbsoluteTracker {
    last_x: Option<i32>,
    last_y: Option<i32>,
}

/// Tracks pressed keyboard keys so auto-repeat is deduplicated.
#[derive(Default)]
struct KeyTracker {
    down: std::collections::HashSet<(u16, bool)>,
}

/// Captures local input on a background thread.
struct InputCapture {
    tx: Sender<Message>,
    abs: AbsoluteTracker,
    keys: KeyTracker,
    /// The last beaconed cursor position (`None` = none yet). Only
    /// changes are forwarded, so a still (or pinned) cursor goes quiet.
    beacon: Option<(i32, i32)>,
    /// Heartbeat for the server supervisor (see
    /// [`core::server::Liveness`]). Bumped every capture-loop iteration.
    capture_tick: Arc<AtomicU64>,
}

/// Register the hidden capture window for raw mouse + keyboard input.
/// Returns the window handle.
fn register_raw_input(hwnd: HWND) -> Result<(), String> {
    let devices = [
        ri::RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_GENERIC_DESKTOP,
            usUsage: USAGE_MOUSE,
            dwFlags: ri::RIDEV_INPUTSINK | ri::RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
        ri::RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_GENERIC_DESKTOP,
            usUsage: USAGE_KEYBOARD,
            dwFlags: ri::RIDEV_INPUTSINK | ri::RIDEV_DEVNOTIFY,
            hwndTarget: hwnd,
        },
    ];
    // SAFETY: the devices array is owned and valid for the call; the
    // handles point at our own window.
    let ok = unsafe {
        ri::RegisterRawInputDevices(devices.as_ptr(), devices.len() as u32, std::mem::size_of::<ri::RAWINPUTDEVICE>() as u32)
    };
    if ok == 0 {
        return Err("RegisterRawInputDevices failed".into());
    }
    Ok(())
}

/// Create the hidden message-only capture window. `DefWindowProcW` is the
/// procedure; `WM_INPUT` is handled directly in the message loop so no
/// shared state is needed in the proc.
fn create_capture_window() -> Result<HWND, String> {
    // SAFETY: trivial kernel32 module lookup for the class registration.
    let hinstance = unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null()) };
    let class = wm::WNDCLASSW {
        style: 0,
        lpfnWndProc: Some(wm::DefWindowProcW),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: CAPTURE_CLASS.as_ptr(),
    };
    // SAFETY: the class is fully initialized above.
    let atom = unsafe { wm::RegisterClassW(&class) };
    if atom == 0 {
        return Err("RegisterClassW failed".into());
    }
    // SAFETY: creating a message-only window (parent HWND_MESSAGE).
    let hwnd = unsafe {
        wm::CreateWindowExW(
            0,
            CAPTURE_CLASS.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            wm::HWND_MESSAGE,
            std::ptr::null_mut(),
            hinstance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    // Start the position-beacon poller (see the module docs).
    // SAFETY: hwnd is valid and owned by this thread's message loop.
    if unsafe { wm::SetTimer(hwnd, BEACON_TIMER as usize, BEACON_MS as u32, None) } == 0 {
        return Err("SetTimer failed".into());
    }
    Ok(hwnd)
}

/// Open raw input capture and start the background message loop.
///
/// Returns the channel the server's main loop reads local input from.
pub fn start() -> Result<(Receiver<Message>, Arc<AtomicU64>), String> {
    let hwnd = create_capture_window()?;
    register_raw_input(hwnd)?;

    log_info!("input capture started (Raw Input)");
    let (tx, rx) = mpsc::channel();
    let capture_tick = Arc::new(AtomicU64::new(0));
    let capture = InputCapture {
        tx,
        abs: AbsoluteTracker::default(),
        keys: KeyTracker::default(),
        beacon: None,
        capture_tick: capture_tick.clone(),
    };
    thread::spawn(move || {
        if let Err(e) = capture.run_forever() {
            log_error!("input capture stopped: {e}");
        }
    });
    Ok((rx, capture_tick))
}

impl InputCapture {
    /// The capture loop: block on the message queue and forward what
    /// matters. Runs forever; returns only on a fatal error.
    fn run_forever(mut self) -> Result<(), String> {
        // SAFETY: msg is a valid out-parameter; GetMessageW blocks until
        // a message arrives and returns 0 only on WM_QUIT (never posted
        // here), -1 on error.
        unsafe {
            let mut msg = std::mem::zeroed::<wm::MSG>();
            loop {
                self.capture_tick.store(
                    std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0),
                    Ordering::Relaxed,
                );
                let ret = wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if ret == 0 || ret == -1 {
                    return Ok(());
                }
                if msg.message == wm::WM_INPUT {
                    self.on_raw_input(msg.lParam as ri::HRAWINPUT);
                } else if msg.message == wm::WM_TIMER && msg.wParam as usize == BEACON_TIMER {
                    self.on_timer();
                } else {
                    wm::TranslateMessage(&msg);
                    wm::DispatchMessageW(&msg);
                }
            }
        }
    }

    /// Parse one `WM_INPUT` payload and forward protocol messages.
    fn on_raw_input(&mut self, handle: ri::HRAWINPUT) {
        // First call with a null buffer returns the required size.
        // SAFETY: standard two-call GetRawInputData pattern.
        unsafe {
            let mut size: u32 = 0;
            ri::GetRawInputData(handle, ri::RID_INPUT, std::ptr::null_mut(), &mut size, std::mem::size_of::<ri::RAWINPUTHEADER>() as u32);
            if size == 0 {
                return;
            }
            let mut buf = vec![0u8; size as usize];
            let got = ri::GetRawInputData(handle, ri::RID_INPUT, buf.as_mut_ptr() as *mut _, &mut size, std::mem::size_of::<ri::RAWINPUTHEADER>() as u32);
            if got == u32::MAX {
                return;
            }
            let raw = &*(buf.as_ptr() as *const ri::RAWINPUT);
            match raw.header.dwType {
                ri::RIM_TYPEMOUSE => self.on_mouse(&raw.data.mouse),
                ri::RIM_TYPEKEYBOARD => self.on_keyboard(&raw.data.keyboard),
                _ => {}
            }
        }
    }

    /// One beacon poll: forward the real cursor position when it moved
    /// since the last poll. See the module docs for why the session needs
    /// these and why changes only.
    fn on_timer(&mut self) {
        let mut pt = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos writes one POINT into a valid buffer.
        let got = unsafe { wm::GetCursorPos(&mut pt) } != 0;
        if !got {
            return;
        }
        let pos = (pt.x, pt.y);
        if self.beacon == Some(pos) {
            return; // still (or pinned): quiet, exactly like X11 at a wall
        }
        self.beacon = Some(pos);
        self.send(Message::MouseMoveAbs { x: pos.0, y: pos.1 });
    }

    fn on_mouse(&mut self, mouse: &ri::RAWMOUSE) {
        // SAFETY: `mouse` points into a validated RAWINPUT from the
        // kernel; the union fields we read are the ones that were
        // populated for a RIM_TYPEMOUSE record (usButtonFlags /
        // usButtonData are one arm of the button union).
        let (button_flags, button_data) = unsafe { (mouse.Anonymous.Anonymous.usButtonFlags, mouse.Anonymous.Anonymous.usButtonData) };

        // Motion: relative deltas arrive directly; absolute coordinates
        // are normalized 0..65535 across the (virtual) screen and are
        // converted to deltas against the previous position.
        if mouse.usFlags & ri::MOUSE_MOVE_ABSOLUTE != 0 {
            let (w, h) = if mouse.usFlags & ri::MOUSE_VIRTUAL_DESKTOP != 0 {
                // SAFETY: virtual-screen metrics.
                unsafe {
                    (
                        wm::GetSystemMetrics(wm::SM_CXVIRTUALSCREEN).max(1),
                        wm::GetSystemMetrics(wm::SM_CYVIRTUALSCREEN).max(1),
                    )
                }
            } else {
                // SAFETY: primary-screen metrics.
                unsafe { (wm::GetSystemMetrics(wm::SM_CXSCREEN).max(1), wm::GetSystemMetrics(wm::SM_CYSCREEN).max(1)) }
            };
            let x = (mouse.lLastX as i64 * w as i64) / 65535;
            let y = (mouse.lLastY as i64 * h as i64) / 65535;
            if let (Some(px), Some(py)) = (self.abs.last_x, self.abs.last_y) {
                let (dx, dy) = (x as i32 - px, y as i32 - py);
                if dx != 0 || dy != 0 {
                    self.send(Message::MouseMoveRel { dx, dy });
                }
            }
            self.abs.last_x = Some(x as i32);
            self.abs.last_y = Some(y as i32);
        } else if mouse.lLastX != 0 || mouse.lLastY != 0 {
            self.send(Message::MouseMoveRel { dx: mouse.lLastX, dy: mouse.lLastY });
        }

        // Buttons and wheel from the same event.
        for (canon, pressed) in buttons::from_raw_flags(button_flags) {
            self.send(Message::MouseButton { button: canon, pressed });
        }
        if button_flags & wm::RI_MOUSE_WHEEL as u16 != 0 {
            self.send(Message::MouseWheel { dx: 0, dy: wheel_notches(button_data) });
        }
        if button_flags & wm::RI_MOUSE_HWHEEL as u16 != 0 {
            self.send(Message::MouseWheel { dx: wheel_notches(button_data), dy: 0 });
        }
    }

    fn on_keyboard(&mut self, kb: &ri::RAWKEYBOARD) {
        // E1-prefixed sequences (Pause) report a make code that collides
        // with an unrelated key (Pause = E1 1D 45 would look like Num
        // Lock's 0x45), so they are dropped rather than mis-forwarded.
        if kb.Flags & wm::RI_KEY_E1 as u16 != 0 {
            return;
        }
        // Key identity: (make code, extended flag).
        let extended = kb.Flags & wm::RI_KEY_E0 as u16 != 0;
        let is_down = matches!(kb.Message, wm::WM_KEYDOWN | wm::WM_SYSKEYDOWN);
        let is_up = matches!(kb.Message, wm::WM_KEYUP | wm::WM_SYSKEYUP);
        if !is_down && !is_up {
            return;
        }
        // Deduplicate auto-repeat: only forward true transitions.
        let id = (kb.MakeCode, extended);
        let was_down = self.keys.down.contains(&id);
        if is_down && !was_down {
            self.keys.down.insert(id);
            self.forward_key(kb.MakeCode, extended, KeyKind::Down);
        } else if is_up && was_down {
            self.keys.down.remove(&id);
            self.forward_key(kb.MakeCode, extended, KeyKind::Up);
        }
    }

    /// Map a (make code, extended) pair to a canonical HID usage and
    /// forward the key event. Unknown keys are dropped rather than sent
    /// with a wrong identity.
    fn forward_key(&self, make: u16, extended: bool, kind: KeyKind) {
        if let Some(key) = crate::keys::hid_from_scancode(make, extended) {
            self.send(Message::Key { kind, key });
        }
    }

    fn send(&self, msg: Message) {
        // The channel is unbounded; the server main loop drains it at its
        // own pace. If the receiver is gone (shutdown), drop the message.
        let _ = self.tx.send(msg);
    }
}

/// Convert a raw wheel `usButtonData` to protocol notch count. Windows
/// reports multiples of `WHEEL_DELTA` (120); the protocol uses 1 notch
/// per unit, matching the X11 backend.
fn wheel_notches(data: u16) -> i32 {
    (data as i16 as i32) / 120
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
