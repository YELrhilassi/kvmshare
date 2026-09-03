//! Mapping between X11 button numbers and the protocol's canonical ids.
//!
//! X11 button numbers (from the X protocol): 1 = left, 2 = middle,
//! 3 = right, 4-7 = wheel (up/down/left/right), 8/9 = back/forward.
//! The protocol's canonical ids live in `kvmshare_protocol::id::buttons`.
//! Wheel buttons never travel as buttons — they become [`Message::MouseWheel`].

use kvmshare_protocol::id::buttons;

/// What an X11 button press means, in protocol terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XButton {
    /// A real button, with its canonical id.
    Button(u8),
    /// A wheel notch, as a (dx, dy) delta in notches.
    Wheel(i32, i32),
    /// Unknown/unmapped button — ignore.
    Ignore,
}

/// X11 button number → protocol meaning (capture side).
pub fn from_x11(button: u32) -> XButton {
    match button {
        1 => XButton::Button(buttons::LEFT),
        2 => XButton::Button(buttons::MIDDLE),
        3 => XButton::Button(buttons::RIGHT),
        4 => XButton::Wheel(0, 1),  // up
        5 => XButton::Wheel(0, -1), // down
        6 => XButton::Wheel(-1, 0), // left
        7 => XButton::Wheel(1, 0),  // right
        8 => XButton::Button(buttons::EXTRA_1),
        9 => XButton::Button(buttons::EXTRA_2),
        _ => XButton::Ignore,
    }
}

/// Canonical button id → X11 button number (injection side).
pub fn to_x11(button: u8) -> Option<u8> {
    match button {
        buttons::LEFT => Some(1),
        buttons::MIDDLE => Some(2),
        buttons::RIGHT => Some(3),
        buttons::EXTRA_1 => Some(8),
        buttons::EXTRA_2 => Some(9),
        _ => None,
    }
}

/// Wheel delta → X11 wheel button (injection side).
pub fn wheel_to_x11(dx: i32, dy: i32) -> Option<u8> {
    if dy > 0 {
        Some(4)
    } else if dy < 0 {
        Some(5)
    } else if dx > 0 {
        Some(7)
    } else if dx < 0 {
        Some(6)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_buttons() {
        for x11 in [1u32, 2, 3, 8, 9] {
            let XButton::Button(canon) = from_x11(x11) else { panic!("not a button") };
            assert_eq!(to_x11(canon), Some(x11 as u8));
        }
    }

    #[test]
    fn wheel_mapping_is_consistent() {
        assert_eq!(from_x11(4), XButton::Wheel(0, 1));
        assert_eq!(wheel_to_x11(0, 1), Some(4));
        assert_eq!(from_x11(6), XButton::Wheel(-1, 0));
        assert_eq!(wheel_to_x11(-1, 0), Some(6));
        assert_eq!(from_x11(99), XButton::Ignore);
    }
}