//! Mapping between Windows mouse button identifiers and the protocol's
//! canonical ids (`kvmshare_protocol::id::buttons`).
//!
//! Two directions, both based on the canonical id:
//!
//! * **Injection** (`to_sendinput`) — canonical id → [`MOUSEEVENTF_*`]
//!   down/up flags used by `SendInput`. Extra buttons also need the
//!   XBUTTON id in `MOUSEINPUT.mouseData`.
//! * **Capture** (`from_raw_flags`) — the raw-input `RI_MOUSE_*` button
//!   flags (bit-packed, multiple flags can be set in one event, and both
//!   the down and up bit for the same button can appear together) →
//!   protocol events.
//!
//! Wheel events never travel as buttons — raw input reports them through
//! `RI_MOUSE_WHEEL` / `RI_MOUSE_HWHEEL` with the delta in
//! `usButtonData` (multiples of `WHEEL_DELTA`).

use windows_sys::Win32::UI::Input::KeyboardAndMouse as km;
use windows_sys::Win32::UI::WindowsAndMessaging as wm;

use kvmshare_protocol::id::buttons;

/// (down flag, up flag, optional XBUTTON data) for a canonical button.
struct Flags {
    down: km::MOUSE_EVENT_FLAGS,
    up: km::MOUSE_EVENT_FLAGS,
    xbutton: Option<u32>,
}

fn to_flags(button: u8) -> Option<Flags> {
    match button {
        buttons::LEFT => Some(Flags {
            down: km::MOUSEEVENTF_LEFTDOWN,
            up: km::MOUSEEVENTF_LEFTUP,
            xbutton: None,
        }),
        buttons::RIGHT => Some(Flags {
            down: km::MOUSEEVENTF_RIGHTDOWN,
            up: km::MOUSEEVENTF_RIGHTUP,
            xbutton: None,
        }),
        buttons::MIDDLE => Some(Flags {
            down: km::MOUSEEVENTF_MIDDLEDOWN,
            up: km::MOUSEEVENTF_MIDDLEUP,
            xbutton: None,
        }),
        buttons::EXTRA_1 => Some(Flags {
            down: km::MOUSEEVENTF_XDOWN,
            up: km::MOUSEEVENTF_XUP,
            xbutton: Some(wm::XBUTTON1 as u32),
        }),
        buttons::EXTRA_2 => Some(Flags {
            down: km::MOUSEEVENTF_XDOWN,
            up: km::MOUSEEVENTF_XUP,
            xbutton: Some(wm::XBUTTON2 as u32),
        }),
        _ => None,
    }
}

/// The down/up event flags (and optional XBUTTON data) for a canonical
/// button, for `SendInput` injection.
pub fn sendinput_flags(button: u8, pressed: bool) -> Option<(km::MOUSE_EVENT_FLAGS, Option<u32>)> {
    let f = to_flags(button)?;
    let flag = if pressed { f.down } else { f.up };
    Some((flag, f.xbutton))
}

/// The wheel event flag for a (dx, dy) delta: vertical or horizontal.
pub fn wheel_flag(dx: i32, dy: i32) -> Option<km::MOUSE_EVENT_FLAGS> {
    if dy != 0 {
        Some(km::MOUSEEVENTF_WHEEL)
    } else if dx != 0 {
        Some(km::MOUSEEVENTF_HWHEEL)
    } else {
        None
    }
}

/// The `mouseData` for a wheel delta: `WHEEL_DELTA` per notch, the same
/// convention the protocol uses (1 notch = 1).
pub fn wheel_data(dx: i32, dy: i32) -> u32 {
    let delta = if dy != 0 { dy } else { dx };
    (delta * wm::WHEEL_DELTA as i32) as u32
}

/// Low-level-hook button messages (`wParam`) → (canonical button,
/// pressed). Extra buttons carry their identity in the high word of
/// `mouse_data` (`XBUTTON1` / `XBUTTON2`). Wheel messages are handled by
/// the caller (they carry data, not a button state).
pub fn from_wparam(msg: u32, mouse_data: u32) -> Option<(u8, bool)> {
    match msg {
        wm::WM_LBUTTONDOWN => Some((buttons::LEFT, true)),
        wm::WM_LBUTTONUP => Some((buttons::LEFT, false)),
        wm::WM_RBUTTONDOWN => Some((buttons::RIGHT, true)),
        wm::WM_RBUTTONUP => Some((buttons::RIGHT, false)),
        wm::WM_MBUTTONDOWN => Some((buttons::MIDDLE, true)),
        wm::WM_MBUTTONUP => Some((buttons::MIDDLE, false)),
        wm::WM_XBUTTONDOWN | wm::WM_XBUTTONUP => {
            let pressed = msg == wm::WM_XBUTTONDOWN;
            let which = (mouse_data >> 16) & 0xFFFF;
            let button = if which == wm::XBUTTON1 as u32 { buttons::EXTRA_1 } else { buttons::EXTRA_2 };
            Some((button, pressed))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "buttons_tests.rs"]
mod tests;
