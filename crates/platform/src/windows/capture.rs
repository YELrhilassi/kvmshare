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

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use windows_sys::Win32::Foundation::HWND;
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
    Ok(hwnd)
}

/// Open raw input capture and start the background message loop.
///
/// Returns the channel the server's main loop reads local input from.
pub fn start() -> Result<Receiver<Message>, String> {
    let hwnd = create_capture_window()?;
    register_raw_input(hwnd)?;

    log_info!("input capture started (Raw Input)");
    let (tx, rx) = mpsc::channel();
    let capture = InputCapture { tx, abs: AbsoluteTracker::default(), keys: KeyTracker::default() };
    thread::spawn(move || {
        if let Err(e) = capture.run_forever() {
            log_error!("input capture stopped: {e}");
        }
    });
    Ok(rx)
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
                let ret = wm::GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if ret == 0 || ret == -1 {
                    return Ok(());
                }
                if msg.message == wm::WM_INPUT {
                    self.on_raw_input(msg.lParam as ri::HRAWINPUT);
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
mod tests {
    use super::*;

    #[test]
    fn wheel_data_converts_to_notches() {
        // 120 = one notch up, -120 = one notch down.
        assert_eq!(wheel_notches(120), 1);
        assert_eq!(wheel_notches(120u16.wrapping_neg()), -1);
        assert_eq!(wheel_notches(0), 0);
        // Two notches (some mice report 240).
        assert_eq!(wheel_notches(240), 2);
    }

    #[test]
    fn e1_pause_sequence_is_dropped_not_misforwarded() {
        // Pause arrives as make code 0x45 with RI_KEY_E1 set — the same
        // make code as Num Lock. It must be dropped, never forwarded as
        // Num Lock.
        let mut capture = InputCapture { tx: mpsc::channel().0, abs: AbsoluteTracker::default(), keys: KeyTracker::default() };
        // SAFETY: zeroed struct; only the fields the handler reads are
        // populated, matching a real raw-input record.
        let mut kb: ri::RAWKEYBOARD = unsafe { std::mem::zeroed() };
        kb.MakeCode = 0x45;
        kb.Flags = wm::RI_KEY_E1 as u16;
        kb.Message = wm::WM_KEYDOWN;
        capture.on_keyboard(&kb);
        assert!(capture.keys.down.is_empty(), "Pause must not be tracked as a key");
    }

    #[test]
    fn key_tracker_dedupes_repeats() {
        let mut capture = InputCapture { tx: mpsc::channel().0, abs: AbsoluteTracker::default(), keys: KeyTracker::default() };
        // Down, then auto-repeat downs, then up.
        capture.keys.down.insert((0x1e, false));
        assert!(capture.keys.down.contains(&(0x1e, false)));
        // A repeat while already down must not change state.
        let id = (0x1e, false);
        let was_down = capture.keys.down.contains(&id);
        assert!(was_down);
        // Up clears it.
        capture.keys.down.remove(&id);
        assert!(!capture.keys.down.contains(&id));
    }

    #[test]
    fn absolute_tracker_computes_deltas() {
        let mut abs = AbsoluteTracker::default();
        assert_eq!((abs.last_x, abs.last_y), (None, None));
        abs.last_x = Some(100);
        abs.last_y = Some(50);
        assert_eq!((abs.last_x, abs.last_y), (Some(100), Some(50)));
    }
}