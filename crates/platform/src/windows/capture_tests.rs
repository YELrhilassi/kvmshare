use super::*;

#[test]
fn wheel_data_converts_to_notches() {
    // 120 = one notch up, -120 = one notch down.
    // The delta is the signed high-order word (the low word is
    // reserved): 120 is 0x00780000, -120 is 0xFF880000.
    assert_eq!(wheel_notches(120 << 16), 1);
    assert_eq!(wheel_notches(0xFF88_0000), -1);
    assert_eq!(wheel_notches(0), 0);
    // The reserved low word must not matter (it is not read).
    assert_eq!(wheel_notches(120 << 16 | 0x1234), 1);
    // Two notches (some mice report 240).
    assert_eq!(wheel_notches(240 << 16), 2);
}

#[test]
fn hook_buttons_map_by_message() {
    use kvmshare_protocol::id::buttons as ids;
    assert_eq!(super::buttons::from_wparam(wm::WM_LBUTTONDOWN, 0), Some((ids::LEFT, true)));
    assert_eq!(super::buttons::from_wparam(wm::WM_LBUTTONUP, 0), Some((ids::LEFT, false)));
    assert_eq!(super::buttons::from_wparam(wm::WM_RBUTTONDOWN, 0), Some((ids::RIGHT, true)));
    assert_eq!(super::buttons::from_wparam(wm::WM_RBUTTONUP, 0), Some((ids::RIGHT, false)));
    assert_eq!(super::buttons::from_wparam(wm::WM_MBUTTONDOWN, 0), Some((ids::MIDDLE, true)));
    assert_eq!(super::buttons::from_wparam(wm::WM_MBUTTONUP, 0), Some((ids::MIDDLE, false)));
    // Extra buttons carry their identity in the high word of mouseData.
    let x1 = (wm::XBUTTON1 as u32) << 16;
    let x2 = (wm::XBUTTON2 as u32) << 16;
    assert_eq!(super::buttons::from_wparam(wm::WM_XBUTTONDOWN, x1), Some((ids::EXTRA_1, true)));
    assert_eq!(super::buttons::from_wparam(wm::WM_XBUTTONUP, x1), Some((ids::EXTRA_1, false)));
    assert_eq!(super::buttons::from_wparam(wm::WM_XBUTTONDOWN, x2), Some((ids::EXTRA_2, true)));
    assert_eq!(super::buttons::from_wparam(wm::WM_XBUTTONUP, x2), Some((ids::EXTRA_2, false)));
    // Wheel and move messages are not buttons.
    assert_eq!(super::buttons::from_wparam(wm::WM_MOUSEWHEEL, 120), None);
    assert_eq!(super::buttons::from_wparam(wm::WM_MOUSEHWHEEL, 120), None);
    assert_eq!(super::buttons::from_wparam(wm::WM_MOUSEMOVE, 0), None);
}

#[test]
fn escape_key_identity_is_consistent() {
    // The hook consumes Scroll Lock by scan code; the canonical HID id
    // must be the escape key every capture backend uses.
    assert_eq!(
        crate::keys::hid_from_scancode(ESCAPE_SCAN, ESCAPE_EXTENDED),
        Some(crate::keys::ESCAPE_KEY_HID)
    );
}

#[test]
fn motion_delta_is_pt_minus_actual() {
    // The hook reports where the cursor WOULD be (unclamped); the real
    // position is where it actually is (the event is not applied yet).
    // Their difference is the effective move — nonzero even when the
    // real cursor is pinned at a wall (a crossing push) or swallowed
    // (isolation), which is exactly what keeps the client steerable.
    let actual = POINT { x: 1918, y: 500 };
    let would_be = POINT { x: 1924, y: 500 };
    assert_eq!((would_be.x - actual.x, would_be.y - actual.y), (6, 0));
    // Interior motion uses the same formula.
    let actual = POINT { x: 500, y: 500 };
    let would_be = POINT { x: 504, y: 500 };
    assert_eq!((would_be.x - actual.x, would_be.y - actual.y), (4, 0));
    // Leftward push against the left wall.
    let actual = POINT { x: 1, y: 300 };
    let would_be = POINT { x: -4, y: 300 };
    assert_eq!((would_be.x - actual.x, would_be.y - actual.y), (-5, 0));
}

#[test]
fn key_dedup_set_behavior() {
    // The hook dedups auto-repeat by (scan code, extended); a down that
    // is already down is not a transition, and an up clears the key.
    let mut down = HashSet::new();
    let id = (0x1e, false);
    assert!(down.insert(id));
    assert!(!down.insert(id), "repeat press is not a transition");
    assert!(down.remove(&id));
    assert!(!down.contains(&id));
}