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

/// Raw-input button flags → (canonical button, pressed) events.
/// A single raw event may contain several transitions; each is reported
/// separately. `RI_MOUSE_WHEEL`/`RI_MOUSE_HWHEEL` are handled by the
/// caller (they carry data, not a button state).
pub fn from_raw_flags(flags: u16) -> Vec<(u8, bool)> {
    let mut out = Vec::with_capacity(2);
    let mut push = |down: u16, up: u16, canon: u8| {
        if flags & down != 0 {
            out.push((canon, true));
        }
        if flags & up != 0 {
            out.push((canon, false));
        }
    };
    push(wm::RI_MOUSE_LEFT_BUTTON_DOWN as u16, wm::RI_MOUSE_LEFT_BUTTON_UP as u16, buttons::LEFT);
    push(wm::RI_MOUSE_RIGHT_BUTTON_DOWN as u16, wm::RI_MOUSE_RIGHT_BUTTON_UP as u16, buttons::RIGHT);
    push(wm::RI_MOUSE_MIDDLE_BUTTON_DOWN as u16, wm::RI_MOUSE_MIDDLE_BUTTON_UP as u16, buttons::MIDDLE);
    push(wm::RI_MOUSE_BUTTON_4_DOWN as u16, wm::RI_MOUSE_BUTTON_4_UP as u16, buttons::EXTRA_1);
    push(wm::RI_MOUSE_BUTTON_5_DOWN as u16, wm::RI_MOUSE_BUTTON_5_UP as u16, buttons::EXTRA_2);
    out
}

#[cfg(test)]
#[path = "buttons_tests.rs"]
mod tests;
