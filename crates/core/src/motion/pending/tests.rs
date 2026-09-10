use super::*;

#[test]
fn keeps_fractional_remainder() {
    let mut m = PendingMotion::default();
    // Sub-pixel motion is held until it crosses a full pixel, with
    // no overshoot in either direction: -0.9px stays put...
    m.push(-0.3, 0.0);
    m.push(-0.3, 0.0);
    m.push(-0.3, 0.0);
    assert_eq!(m.take_whole(), None);
    assert!((m.fx + 0.9).abs() < 1e-9);
    // ...until it crosses -1, and then it emits exactly -1 (a floor
    // here would have emitted -1 already at -0.9 and biased the
    // cursor leftward under jitter).
    m.push(-0.3, 0.0);
    assert_eq!(m.take_whole(), Some((-1, 0)));
    assert!((m.fx + 0.2).abs() < 1e-9);
    // Nothing more to take until more motion accrues.
    assert_eq!(m.take_whole(), None);
    // Fast motion passes through whole (the leftover -0.2 from the
    // slow drift above offsets the 12: 11.8 -> 11).
    m.push(12.0, 5.9);
    assert_eq!(m.take_whole(), Some((11, 5)));
    assert!((m.fx - 0.8).abs() < 1e-9);
    assert!((m.fy - 0.9).abs() < 1e-9);
}

#[test]
fn has_pending_sees_only_whole_pixels() {
    let mut m = PendingMotion::default();
    assert!(!m.has_pending());
    m.push(0.4, 0.0);
    assert!(!m.has_pending(), "sub-pixel motion is not pending");
    m.push(0.7, 0.0);
    assert!(m.has_pending(), "a whole pixel is pending");
    assert_eq!(m.take_whole(), Some((1, 0)));
    assert!(!m.has_pending());
}