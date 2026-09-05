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
