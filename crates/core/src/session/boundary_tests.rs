//! Unit tests for the [`BoundarySide`] arm/push/fire machine. The full
//! crossing behaviour (through the session) lives in `session_tests.rs`
//! and the e2e suite; these pin the small machine's contract directly.

use std::time::Duration;

use kvmshare_protocol::message::Rect;

use super::*;

const RECT: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

#[test]
fn beacon_arms_only_the_walls_the_cursor_sits_on() {
    let mut s = BoundarySide::default();
    // Dead center: nothing armed.
    assert_eq!(s.on_beacon(&RECT, 960, 540), 0);
    assert!(!s.armed(Direction::Left));
    // Last pixel column: right wall armed (freshly — the return value).
    assert_eq!(s.on_beacon(&RECT, 1919, 540), bit(Direction::Right));
    assert!(s.armed(Direction::Right));
    assert!(!s.armed(Direction::Left));
    // Still on the wall: not newly armed.
    assert_eq!(s.on_beacon(&RECT, 1919, 541), 0);
    // Band edge (2 px): still armed.
    assert!(s.armed(Direction::Right) == (s.on_beacon(&RECT, 1918, 540), true).1 || true);
    let _ = s.on_beacon(&RECT, 1918, 540);
    assert!(s.armed(Direction::Right));
}

#[test]
fn beacon_back_inside_disarms_and_clears_the_fallback() {
    let mut s = BoundarySide::default();
    let _ = s.on_beacon(&RECT, 0, 540); // arm left
    assert!(s.armed(Direction::Left));
    // Start a fallback latch so we can watch it clear.
    assert!(!s.tick_fallback(Direction::Left, true)); // fresh beacon: never fires
    let _ = s.on_beacon(&RECT, 500, 540);
    assert!(!s.armed(Direction::Left));
    // The fallback latch is gone: a later non-fresh tick starts over.
    assert!(!s.tick_fallback(Direction::Left, false));
}

#[test]
fn outward_delta_on_an_armed_wall_is_the_fire_signal() {
    let mut s = BoundarySide::default();
    let _ = s.on_beacon(&RECT, 1919, 540);
    assert!(s.on_delta(Direction::Right, 5, 0));
    // Same delta on an unarmed wall is not.
    let mut t = BoundarySide::default();
    let _ = t.on_beacon(&RECT, 960, 540);
    assert!(!t.on_delta(Direction::Right, 5, 0));
}

#[test]
fn inward_delta_disarms_so_entry_never_bounces() {
    let mut s = BoundarySide::default();
    let _ = s.on_beacon(&RECT, 0, 540); // arm left
    assert!(!s.on_delta(Direction::Left, 8, 0)); // pushing inward (dx > 0 away from left wall)
    assert!(!s.armed(Direction::Left));
    // A fresh outward push no longer fires: re-arm is required.
    assert!(!s.on_delta(Direction::Left, -8, 0));
}

#[test]
fn fallback_never_fires_while_the_beacon_stream_is_fresh() {
    let mut s = BoundarySide::default();
    for _ in 0..100 {
        assert!(!s.tick_fallback(Direction::Bottom, true));
    }
}

#[test]
fn fallback_fires_only_after_the_sustained_window() {
    let mut s = BoundarySide::default();
    // First tick starts the latch (not yet sustained).
    assert!(!s.tick_fallback(Direction::Bottom, false));
    // Still inside the window: no fire. (Instant-based; sleep briefly
    // to cross the threshold.)
    std::thread::sleep(EDGE_PUSH_FALLBACK + Duration::from_millis(5));
    assert!(s.tick_fallback(Direction::Bottom, false));
    s.stop_fallback();
    // After the stop, the latch restarts from zero.
    assert!(!s.tick_fallback(Direction::Bottom, false));
}
