use super::*;

#[test]
fn now_ms_is_monotonic() {
    let a = now_ms();
    let b = now_ms();
    assert!(b >= a);
}
