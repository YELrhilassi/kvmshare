use super::*;

#[test]
fn measures_px_per_count_from_delta_and_beacon_windows() {
    let mut g = GainTracker::new();
    assert_eq!(g.gain(), 1.0, "neutral until measured");
    // The first beacon anchors the window (nothing to measure
    // against yet); 100 counts of raw motion, real cursor travelled
    // 50 px: 0.5 px/count (a slow-speed libinput curve).
    g.on_beacon(0, 0);
    for _ in 0..25 {
        g.on_delta(4, 0);
    }
    let gain = g.on_beacon(50, 0);
    assert!((gain - 0.5).abs() < 0.01, "window sets the gain, got {gain}");
    assert_eq!(g.gain(), gain);
}

#[test]
fn skips_idle_and_jump_windows() {
    let mut g = GainTracker::new();
    g.on_beacon(0, 0); // anchor
    // Idle: almost no counts, no travel — the gain must not move
    // from 1.0 (a parked cursor would otherwise read as 0 px/count).
    g.on_delta(1, 0);
    assert_eq!(g.on_beacon(1, 0), 1.0);
    // A jump (a crossing park/warp: real travel far beyond the
    // counts) must be excluded, not averaged in.
    g.on_delta(5, 0);
    assert_eq!(g.on_beacon(400, 0), 1.0, "jump windows are skipped");
}

#[test]
fn smooths_toward_the_true_transform() {
    let mut g = GainTracker::new();
    // Ten windows of 100 counts / 60 px travel each: the estimate
    // must converge near 0.6 and stay there (not drift per window).
    let mut target = 0;
    for _ in 0..10 {
        for _ in 0..25 {
            g.on_delta(4, 0);
        }
        target += 60; // the real cursor advances 60 px per window
        g.on_beacon(target, 0);
    }
    assert!((g.gain() - 0.6).abs() < 0.1, "converged near 0.6, got {}", g.gain());
}

#[test]
fn gain_is_frozen_while_no_beacons_flow() {
    // While the cursor is on a client the local cursor is parked:
    // deltas keep arriving but no beacons close a window — the gain
    // must stay exactly where it was.
    let mut g = GainTracker::new();
    for _ in 0..25 {
        g.on_delta(4, 0);
    }
    g.on_beacon(50, 0);
    let frozen = g.gain();
    for _ in 0..100 {
        g.on_delta(9, 0); // remote-era motion, no beacons
    }
    assert_eq!(g.gain(), frozen);
}
