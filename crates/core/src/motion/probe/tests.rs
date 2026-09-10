use super::*;

#[test]
fn probe_reports_requested_vs_actual_per_window() {
    let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
    let mut out = Vec::new();
    p.enter((100, 200));
    p.requested(10, 0);
    p.requested(20, 5);
    // Window elapsed, real cursor moved only 6 px for the 30
    // requested counts: a 0.2 px/count transform (the gel signature).
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((106, 201), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert_eq!(out.len(), 1);
    let (req_x, req_y, act_x, act_y, exp_x, exp_y, real_x, real_y) = out[0];
    assert_eq!((req_x, req_y), (30, 5), "requested counts for the window");
    assert_eq!((act_x, act_y), (6, 1), "actual travel in the window");
    assert_eq!((exp_x, exp_y), (30, 5), "expected = anchor + requested");
    assert_eq!((real_x, real_y), (106, 201), "real position");
}

#[test]
fn probe_lazy_anchors_when_never_entered() {
    // A server-side probe is never explicitly entered: its first due
    // sample must silently anchor and the next one measure.
    let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
    let mut out = Vec::new();
    // Counts before the first sample are discarded, not measured.
    p.requested(999, 999);
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((50, 50), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert!(out.is_empty(), "first sample only anchors");
    assert!(p.active());
    p.requested(100, 0);
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((80, 50), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert_eq!(out.len(), 1);
    let (req_x, req_y, act_x, act_y, ..) = out[0];
    assert_eq!((req_x, req_y), (100, 0), "only post-anchor counts measured");
    assert_eq!((act_x, act_y), (30, 0));
}

#[test]
fn probe_is_silent_without_motion_or_when_inactive() {
    let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
    let mut out = Vec::new();
    // Inactive: never logs.
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((5, 5), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert!(out.is_empty());
    // Active but idle: window with no motion logs nothing.
    p.enter((0, 0));
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((0, 0), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert!(out.is_empty());
    // Leaving closes the session.
    p.leave();
    p.requested(1, 1);
    std::thread::sleep(std::time::Duration::from_millis(15));
    p.sample((1, 1), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
    assert!(out.is_empty());
}
