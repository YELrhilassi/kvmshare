//! Remote-stream handlers: motion/beacon/button/key forwarding and
//! resync, plus the boundary unit tests.

use super::*;
use super::boundary::{wall_bits, BIT_LEFT, BIT_RIGHT, BIT_TOP};

#[test]
fn remote_inward_motion_disarms_the_wall() {
    // After entering hp the cursor sits on the seam (hp's right
    // wall). Moving *into* hp disarms it, so a stray outward jitter
    // cannot bounce control straight back home.
    let mut s = two_screens();
    cross_to_hp(&mut s);
    s.on_remote_beacon(1, 1919, 540); // arm the seam wall
    s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 }); // into hp: disarm
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 1, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1), "disarmed wall must not fire");
    // The real cursor must reach the wall again; with the push still
    // fresh, the park itself completes the crossing home.
    let actions = s.on_remote_beacon(1, 1919, 540);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn remote_beacon_park_mid_push_crosses_on_the_park() {
    // The user sweeps right across hp toward home and the beacon
    // parks the real cursor on the shared wall mid-push: the crossing
    // fires on the park itself (the actions are returned to the
    // client thread to execute), no dead frame at the boundary.
    let mut s = two_screens();
    cross_to_hp(&mut s);
    // A hard outward push races the virtual cursor out of hp.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 2000, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 2000, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1));
    // The beacon parks the real cursor on the wall mid-push: cross now.
    let actions = s.on_remote_beacon(1, 1919, 540);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn buttons_forward_only_when_remote() {
    let mut s = two_screens();
    assert_eq!(s.on_local_event(Message::MouseButton { button: 0, pressed: true }), vec![]);
    cross_to_hp(&mut s);
    let actions = s.on_local_event(Message::MouseButton { button: 0, pressed: true });
    assert_eq!(actions, vec![Action::Send(Message::MouseButton { button: 0, pressed: true })]);
}

#[test]
fn outer_edge_clamps() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // on hp, virtual (-1,540)
    // Push far left past hp's left edge (an outer edge of the
    // desktop — no neighbor there). The motion is forwarded but the
    // virtual cursor clamps and nothing crosses.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -3000, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -3000, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1));
    // Even with the real cursor pinned on that outer wall and a
    // fresh push, there is nowhere to go: motion forwards, hp keeps
    // control.
    assert_eq!(s.on_remote_beacon(1, 0, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -5, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn escape_returns_home_even_when_remote() {
    let mut s = two_screens();
    // Escape while local: re-anchors to the local center (the
    // capture only emits Escape while remote, but it must not corrupt
    // state if it fires locally).
    let actions = s.on_local_event(Message::Escape);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 960, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);

    // The real case: stuck on a client (even one that stopped
    // responding) — the escape key brings control home regardless.
    cross_to_hp(&mut s);
    assert_eq!(s.mode(), Mode::Remote(1));
    let actions = s.on_local_event(Message::Escape);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 960, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
    assert_eq!(s.cursor_pos(), (960, 540));
}

#[test]
fn disconnect_returns_home() {
    let mut s = two_screens();
    cross_to_hp(&mut s);
    assert_eq!(s.on_client_disconnected(1), Action::SwitchToLocal { x: 960, y: 540 });
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn key_events_forward_when_remote() {
    let mut s = two_screens();
    cross_to_hp(&mut s);
    let actions = s.on_local_event(Message::Key { kind: KeyKind::Down, key: 0x14 });
    assert_eq!(actions, vec![Action::Send(Message::Key { kind: KeyKind::Down, key: 0x14 })]);
}

#[test]
fn remote_to_remote_switch_fires_on_the_park() {
    let layout = Layout::new(vec![
        Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
        Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
        Screen { id: 2, name: "mac".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
    ]);
    let mut s = Session::new(layout, 0);
    s.on_client_connected(2); // mac must be online to be a destination
    // pc -> hp
    cross_to_hp(&mut s);
    // Swoop across hp toward its left wall (raw deltas overshoot the
    // rect; no beacon has confirmed the real cursor there yet).
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -2000, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1), "overshoot alone must not switch");
    // The client reports its real cursor parked on hp's left wall
    // while the sweep is still pushing: switch on to mac, on the
    // park itself.
    let actions = s.on_remote_beacon(1, 0, 540);
    match actions.as_slice() {
        [Action::SwitchTo { to, x, y }] => {
            assert_eq!(*to, 2);
            assert_eq!(*x, 1919 - ENTRY_INSET); // mac's right edge, inset from the seam
            assert_eq!(*y, 540);
        }
        other => panic!("expected [SwitchTo], got {other:?}"),
    }
    assert_eq!(s.mode(), Mode::Remote(2));
}

#[test]
fn remote_roam_only_forwards_relative_deltas() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // on hp
    // Roam around hp. The session must only ever emit relative
    // motion for the client — never an absolute position (the
    // hidden local cursor never moves while we are away, so no warp
    // or recenter can sweep hover/enter effects across local
    // windows).
    for dx in [-1000, -900, 1900, -1000] {
        let actions = s.on_local_event(Message::MouseMoveRel { dx, dy: 0 });
        for a in &actions {
            assert!(
                matches!(a, Action::Send(Message::MouseMoveRel { .. })),
                "remote motion must only forward relative deltas, got {a:?}"
            );
        }
    }
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn swap_layout_while_local_stays_put() {
    let mut s = two_screens();
    assert_eq!(s.mode(), Mode::Local);

    // Same geometry, only names change: nothing to do, cursor stays.
    let new_layout = Layout::new(vec![
        Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
        Screen { id: 1, name: "hp".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
    ]);
    let actions = s.swap_layout(new_layout);
    assert_eq!(actions, vec![]);
    assert_eq!(s.mode(), Mode::Local);
    assert_eq!(s.layout().screens[1].rect.x, -3840);
}

#[test]
fn swap_layout_while_remote_comes_home() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // now on hp
    assert_eq!(s.mode(), Mode::Remote(1));

    // New layout without hp at all (and a different local size).
    let new_layout = Layout::new(vec![Screen {
        id: 0,
        name: "pc".into(),
        rect: Rect { x: 0, y: 0, w: 2560, h: 1440 },
    }]);
    let actions = s.swap_layout(new_layout);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 1280, y: 720 }]);
    assert_eq!(s.mode(), Mode::Local);
    assert_eq!(s.cursor_pos(), (1280, 720));
}

#[test]
fn beacon_resyncs_the_virtual_cursor_to_the_real_position() {
    let mut s = two_screens();
    // The virtual cursor drifted far from the real one (say the
    // server started while the mouse sat near the right edge): a
    // beacon snaps it to the real position, and motion then moves
    // from there.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 1500, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 0 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.cursor_pos(), (1510, 540)); // 1500 (real) + delta
    // Leftward motion from mid-screen stays local too.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -700, dy: 0 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.cursor_pos(), (810, 540));
}

#[test]
fn deltas_alone_never_jump_to_a_neighbor() {
    // The bug this guards against: raw deltas run ahead of the real
    // cursor (they are pre-acceleration and the beacon can lag under
    // load), so a fast approach near the edge used to overshoot the
    // boundary and "jump" to the client without intent. Deltas alone
    // — even far past the edge — must never switch while the real
    // cursor is still inside.
    let mut s = two_screens();
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
    assert_eq!(s.mode(), Mode::Local, "overshoot alone must not switch");
    // A beacon showing the real cursor back inside confirms it was an
    // overshoot.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 300, y: 540 }), vec![]);
    assert_eq!(s.cursor_pos(), (300, 540));
    // Continuing from there stays local.
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 }), vec![]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn sustained_push_crosses_when_the_beacon_stream_stalls() {
    // A stalled beacon stream: the OS has pinned the pointer at the
    // edge, position events (and with them beacons) stop, and only
    // raw deltas keep flowing. Without a beacon the first push cannot
    // be confirmed — but sustained pushing past the fallback window
    // (with the virtual cursor outside the rect) must still cross, or
    // the cursor would stick at the edge forever.
    let mut s = two_screens();
    s.on_client_connected(1);
    // The push reaches the edge and stays there (unconfirmed, no
    // switch yet).
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
    assert_eq!(s.mode(), Mode::Local, "first unconfirmed push must not switch");
    // Wait out the fallback window, then keep pushing.
    std::thread::sleep(EDGE_PUSH_FALLBACK + Duration::from_millis(20));
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn local_abs_beacon_is_ignored_while_remote() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // on hp, virtual (-(ENTRY_INSET + 1), 540)
    // A *local* capture beacon while remote is the hidden parked
    // cursor (meaningless): it must not resync the virtual position.
    let actions = s.on_local_event(Message::MouseMoveAbs { x: 50, y: 60 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.cursor_pos(), (-(ENTRY_INSET + 1), 540)); // untouched
    // Crossing home is driven by the client's own beacon, not the
    // local one.
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn remote_beacon_from_wrong_client_is_ignored() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // active client is 1
    assert_eq!(s.on_remote_beacon(2, 0, 0), vec![]); // not the active one
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn sustained_remote_push_crosses_when_beacons_stall() {
    // A client whose beacon stream stalls (wedged, network drop):
    // outward pushing must still bring control home after the
    // fallback window, or the cursor would be stuck on the client
    // forever.
    let mut s = two_screens();
    cross_to_hp(&mut s); // virtual (-(ENTRY_INSET + 1), 540): the entry inset
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 }), vec![Action::Send(Message::MouseMoveRel { dx: 1, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1), "no beacon yet: one push must not cross");
    // Keep pushing until the virtual cursor has traversed the entry
    // inset and actually leaves hp's rect — then the fallback window
    // must bring control home.
    for _ in 0..60 {
        s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    }
    std::thread::sleep(REMOTE_BEACON_FRESH + EDGE_PUSH_FALLBACK + Duration::from_millis(20));
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn remote_beacon_resyncs_the_virtual_cursor() {
    // The client's real cursor (post-acceleration) is the ground
    // truth on its screen. A beacon must re-anchor the virtual cursor
    // so the stalled-stream fallback and entry math start from
    // reality instead of raw deltas that acceleration ran ahead of.
    let mut s = two_screens();
    cross_to_hp(&mut s); // virtual (-1, 540)
    // The client reports its real cursor mid-screen (our raw deltas
    // had overshot): snap to reality.
    s.on_remote_beacon(1, 900, 200);
    assert_eq!(s.cursor_pos(), (-1020, 200));
}

#[test]
fn swap_layout_rejects_missing_local_screen() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // on hp
    let bad = Layout::new(vec![Screen {
        id: 1,
        name: "hp".into(),
        rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 },
    }]);
    let actions = s.swap_layout(bad);
    assert_eq!(actions, vec![]);
    assert_eq!(s.mode(), Mode::Remote(1), "bad layout must not disturb the session");
    assert_eq!(s.layout().screens.len(), 2);
}

#[test]
fn wall_bits_marks_the_outer_band_only() {
    let rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
    assert_eq!(wall_bits(&rect, 0, 540), BIT_LEFT);
    assert_eq!(wall_bits(&rect, 1, 540), BIT_LEFT); // band slack
    assert_eq!(wall_bits(&rect, 2, 540), 0);
    assert_eq!(wall_bits(&rect, 1918, 540), BIT_RIGHT);
    assert_eq!(wall_bits(&rect, 1919, 540), BIT_RIGHT);
    assert_eq!(wall_bits(&rect, 1919, 0), BIT_RIGHT | BIT_TOP); // corner
    assert_eq!(wall_bits(&rect, 960, 540), 0);
}

#[test]
fn crossing_roundtrip_back_and_forth_is_crisp() {
    // Rapid back-and-forth at the boundary: each direction must cross
    // on a beacon-arm plus one push — no fallback timers in the
    // common path.
    let mut s = two_screens();
    // pc -> hp
    cross_to_hp(&mut s);
    // hp -> pc
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 2, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
    // pc -> hp again, immediately.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -2, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn crossing_into_a_disconnected_client_is_a_dead_edge() {
    // The regression this guards: a crossing into a screen whose client
    // is not connected used to fire anyway — the engine armed its input
    // isolation and hid the local cursor with nothing on the other side
    // to return control to (only the escape key could bring it home,
    // and the beacon watchdog spun forever on a stream that cannot
    // exist). A configured-but-offline screen must behave like a
    // desktop edge with no neighbor.
    let mut s = two_screens();
    // Park on the left wall and push: no client on hp, no crossing.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_eq!(actions, vec![], "no crossing into an offline client");
    assert_eq!(s.mode(), Mode::Local);
    assert_eq!(s.cursor_pos(), (0, 540), "virtual cursor stays clamped at the wall");
    // Let the failed push's intent go stale, then bring the client
    // online: a fresh arm + push now crosses normally.
    std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
    s.on_client_connected(1);
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
    // It disconnects while the cursor is on it: control returns home
    // and the screen is a dead edge again.
    assert_eq!(s.on_client_disconnected(1), Action::SwitchToLocal { x: 960, y: 540 }); // forced return goes to the local center
    assert_eq!(s.mode(), Mode::Local);
    // A fresh arm + push at the wall still cannot cross into the
    // offline client.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_eq!(actions, vec![], "disconnected client stays a dead edge");
    assert_eq!(s.mode(), Mode::Local);
}
