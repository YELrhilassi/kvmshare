use super::*;

#[test]
fn flags_round_trip_all_buttons() {
    for (canon, down, up) in [
        (
            buttons::LEFT,
            km::MOUSEEVENTF_LEFTDOWN,
            km::MOUSEEVENTF_LEFTUP,
        ),
        (
            buttons::RIGHT,
            km::MOUSEEVENTF_RIGHTDOWN,
            km::MOUSEEVENTF_RIGHTUP,
        ),
        (
            buttons::MIDDLE,
            km::MOUSEEVENTF_MIDDLEDOWN,
            km::MOUSEEVENTF_MIDDLEUP,
        ),
        (buttons::EXTRA_1, km::MOUSEEVENTF_XDOWN, km::MOUSEEVENTF_XUP),
        (buttons::EXTRA_2, km::MOUSEEVENTF_XDOWN, km::MOUSEEVENTF_XUP),
    ] {
        // X buttons carry the XBUTTON id in `mouseData` on **both**
        // down and up: Windows needs it to know which X button the
        // event refers to (the MOUSEINPUT docs say so for both
        // MOUSEEVENTF_XDOWN and MOUSEEVENTF_XUP).
        let xbutton = match canon {
            buttons::EXTRA_1 => Some(wm::XBUTTON1 as u32),
            buttons::EXTRA_2 => Some(wm::XBUTTON2 as u32),
            _ => None,
        };
        assert_eq!(sendinput_flags(canon, true), Some((down, xbutton)));
        assert_eq!(sendinput_flags(canon, false), Some((up, xbutton)));
    }
    assert_eq!(sendinput_flags(99, true), None);
}

#[test]
fn hook_messages_decode_transitions() {
    // Down and up of each button map to the canonical ids, in the
    // message order the low-level hook delivers them.
    assert_eq!(
        from_wparam(wm::WM_LBUTTONDOWN, 0),
        Some((buttons::LEFT, true))
    );
    assert_eq!(
        from_wparam(wm::WM_LBUTTONUP, 0),
        Some((buttons::LEFT, false))
    );
    assert_eq!(
        from_wparam(wm::WM_RBUTTONDOWN, 0),
        Some((buttons::RIGHT, true))
    );
    assert_eq!(
        from_wparam(wm::WM_RBUTTONUP, 0),
        Some((buttons::RIGHT, false))
    );
    assert_eq!(
        from_wparam(wm::WM_MBUTTONDOWN, 0),
        Some((buttons::MIDDLE, true))
    );
    assert_eq!(
        from_wparam(wm::WM_MBUTTONUP, 0),
        Some((buttons::MIDDLE, false))
    );

    // Extra buttons: identity in the high word of mouseData.
    let x1 = (wm::XBUTTON1 as u32) << 16;
    let x2 = (wm::XBUTTON2 as u32) << 16;
    assert_eq!(
        from_wparam(wm::WM_XBUTTONDOWN, x1),
        Some((buttons::EXTRA_1, true))
    );
    assert_eq!(
        from_wparam(wm::WM_XBUTTONUP, x1),
        Some((buttons::EXTRA_1, false))
    );
    assert_eq!(
        from_wparam(wm::WM_XBUTTONDOWN, x2),
        Some((buttons::EXTRA_2, true))
    );
    assert_eq!(
        from_wparam(wm::WM_XBUTTONUP, x2),
        Some((buttons::EXTRA_2, false))
    );

    // Wheel and motion messages are not buttons.
    assert_eq!(from_wparam(wm::WM_MOUSEWHEEL, 120), None);
    assert_eq!(from_wparam(wm::WM_MOUSEHWHEEL, 120), None);
    assert_eq!(from_wparam(wm::WM_MOUSEMOVE, 0), None);
}

#[test]
fn wheel_mapping_is_consistent() {
    assert_eq!(wheel_flag(0, 1), Some(km::MOUSEEVENTF_WHEEL));
    assert_eq!(wheel_flag(1, 0), Some(km::MOUSEEVENTF_HWHEEL));
    assert_eq!(wheel_flag(0, 0), None);
    // `mouseData` is a **signed** delta: 3 notches down is -360 (the
    // u32 wrap is the two's-complement form Windows reads back as a
    // negative value — flipping the sign here would invert scroll
    // direction on Windows).
    assert_eq!(wheel_data(0, -3), (-3 * wm::WHEEL_DELTA as i32) as u32);
    assert_eq!(wheel_data(2, 0), (2 * wm::WHEEL_DELTA as i32) as u32);
}
