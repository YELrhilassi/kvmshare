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
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_all_buttons() {
        for (canon, down, up) in [
            (buttons::LEFT, km::MOUSEEVENTF_LEFTDOWN, km::MOUSEEVENTF_LEFTUP),
            (buttons::RIGHT, km::MOUSEEVENTF_RIGHTDOWN, km::MOUSEEVENTF_RIGHTUP),
            (buttons::MIDDLE, km::MOUSEEVENTF_MIDDLEDOWN, km::MOUSEEVENTF_MIDDLEUP),
            (buttons::EXTRA_1, km::MOUSEEVENTF_XDOWN, km::MOUSEEVENTF_XUP),
            (buttons::EXTRA_2, km::MOUSEEVENTF_XDOWN, km::MOUSEEVENTF_XUP),
        ] {
            assert_eq!(sendinput_flags(canon, true), Some((down, if down == km::MOUSEEVENTF_XDOWN { Some(if canon == buttons::EXTRA_1 { wm::XBUTTON1 as u32 } else { wm::XBUTTON2 as u32 }) } else { None })));
            assert_eq!(sendinput_flags(canon, false), Some((up, None)));
        }
        assert_eq!(sendinput_flags(99, true), None);
    }

    #[test]
    fn raw_flags_decode_transitions() {
        // Both down and up of left in one event → two transitions, in order.
        let evts = from_raw_flags((wm::RI_MOUSE_LEFT_BUTTON_DOWN | wm::RI_MOUSE_LEFT_BUTTON_UP) as u16);
        assert_eq!(evts, vec![(buttons::LEFT, true), (buttons::LEFT, false)]);

        // Right down + middle up simultaneously.
        let evts = from_raw_flags((wm::RI_MOUSE_RIGHT_BUTTON_DOWN | wm::RI_MOUSE_MIDDLE_BUTTON_UP) as u16);
        assert_eq!(evts, vec![(buttons::RIGHT, true), (buttons::MIDDLE, false)]);

        assert!(from_raw_flags(0).is_empty());
        // Wheel bits are not buttons.
        assert!(from_raw_flags(wm::RI_MOUSE_WHEEL as u16).is_empty());
    }

    #[test]
    fn wheel_mapping_is_consistent() {
        assert_eq!(wheel_flag(0, 1), Some(km::MOUSEEVENTF_WHEEL));
        assert_eq!(wheel_flag(1, 0), Some(km::MOUSEEVENTF_HWHEEL));
        assert_eq!(wheel_flag(0, 0), None);
        assert_eq!(wheel_data(0, -3), (3 * wm::WHEEL_DELTA) as u32);
        assert_eq!(wheel_data(2, 0), (2 * wm::WHEEL_DELTA) as u32);
    }
}