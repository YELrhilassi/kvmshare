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
