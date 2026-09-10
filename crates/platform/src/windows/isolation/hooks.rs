//! The low-level hook procedures: swallow hardware-origin mouse and
//! keyboard events while [`ISOLATE`] is set; everything injected (our
//! forwarded stream, legitimate local software) passes untouched.

use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use super::{ISOLATE, LLKHF_INJECTED, LLMHF_INJECTED};

/// Low-level mouse hook: swallow hardware-origin input while this
/// machine's cursor is controlled remotely; pass everything else through
/// untouched.
///
/// # Safety
///
/// `lparam` points at a `MSLLHOOKSTRUCT` owned by the system for the
/// duration of the call — the standard low-level-hook contract.
pub(super) unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && ISOLATE.load(Ordering::SeqCst) {
        // SAFETY: lparam is a valid MSLLHOOKSTRUCT pointer while the
        // hook procedure runs (documented contract of WH_MOUSE_LL).
        let info = unsafe { &*(lparam as *const wm::MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            // Nonzero return: the event is not delivered. The hardware
            // cursor never moves, no click lands, no wheel turns.
            return 1;
        }
    }
    wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

/// Low-level keyboard hook: the same contract as [`mouse_proc`] for
/// keys — hardware keys are silenced while controlled, injected keys
/// (the forwarded stream) pass.
///
/// # Safety
///
/// `lparam` points at a `KBDLLHOOKSTRUCT` owned by the system for the
/// duration of the call.
pub(super) unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && ISOLATE.load(Ordering::SeqCst) {
        // SAFETY: lparam is a valid KBDLLHOOKSTRUCT pointer while the
        // hook procedure runs (documented contract of WH_KEYBOARD_LL).
        let info = unsafe { &*(lparam as *const wm::KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED == 0 {
            return 1;
        }
    }
    wm::CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}
