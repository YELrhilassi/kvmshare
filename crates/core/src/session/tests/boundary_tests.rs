//! Crossing state machine: edges, arming, parks, swaps.

use super::*;

#[test]
fn assign_screen_id_matches_by_name() {
    let s = two_screens();
    assert_eq!(s.assign_screen_id("hp"), Some(1));
    assert_eq!(s.assign_screen_id("pc"), None); // the server's own screen
    assert_eq!(s.assign_screen_id("nope"), None);
}

#[test]
fn admit_known_client_does_not_change_the_layout() {
    let mut s = two_screens();
    let before = s.layout().clone();
    let (id, admitted) = s
        .admit_client("hp", kvmshare_protocol::message::ScreenInfo { width: 1920, height: 1080, scale: 1.0 })
        .unwrap();
    assert_eq!((id, admitted), (1, false));
    assert_eq!(s.layout(), &before);
}

#[test]
fn admit_unknown_client_places_it_right_of_the_desktop() {
    let mut s = two_screens(); // pc at origin, hp to the left (x=-1920)
    let (id, admitted) = s
        .admit_client("mac", kvmshare_protocol::message::ScreenInfo { width: 2560, height: 1440, scale: 1.0 })
        .unwrap();
    assert_eq!((id, admitted), (2, true));
    let mac = s.layout().find(2).unwrap();
    // Right of the rightmost edge (pc's right edge at x=1920), so it
    // can never overlap an existing screen.
    assert_eq!(mac.rect.x, 1920);
    assert_eq!((mac.rect.w, mac.rect.h), (2560, 1440));
    // Existing screens are untouched.
    assert_eq!(s.layout().find(0).unwrap().rect.x, 0);
    assert_eq!(s.layout().find(1).unwrap().rect.x, -1920);
}

#[test]
fn admit_unknown_client_keeps_its_id_across_reconnects() {
    let mut s = two_screens();
    let (id, _) = s
        .admit_client("mac", kvmshare_protocol::message::ScreenInfo { width: 2560, height: 1440, scale: 1.0 })
        .unwrap();
    // A reconnect with the same name resolves to the same screen; the
    // caller separately applies the fresh geometry report.
    let (again, admitted) = s
        .admit_client("mac", kvmshare_protocol::message::ScreenInfo { width: 1920, height: 1080, scale: 1.0 })
        .unwrap();
    assert_eq!(again, id);
    assert!(!admitted);
    assert_eq!(s.layout().find(id).unwrap().rect.w, 2560); // untouched
}

#[test]
fn admit_client_uses_reported_geometry_as_is() {
    let mut s = two_screens();
    // The layout lives in the same space the injector reports and
    // beacons from (physical pixels on Windows, root pixels on X11 —
    // the reported scale is informational), so the reported size is
    // used as-is. Dividing by scale used to shrink the layout below
    // the real cursor space, which broke boundary arms on scaled
    // displays.
    let (id, admitted) = s
        .admit_client("hi", kvmshare_protocol::message::ScreenInfo { width: 3840, height: 2160, scale: 1.5 })
        .unwrap();
    assert!(admitted);
    assert_eq!(s.layout().find(id).unwrap().rect.w, 3840);
    assert_eq!(s.layout().find(id).unwrap().rect.h, 2160);
}

#[test]
fn admit_client_refuses_the_local_screen_name() {
    let mut s = two_screens(); // the local screen is "pc"
    let got = s.admit_client("pc", kvmshare_protocol::message::ScreenInfo { width: 1920, height: 1080, scale: 1.0 });
    assert_eq!(got, None);
    // Nothing was added.
    assert_eq!(s.layout().screens.len(), 2);
}

#[test]
fn update_screen_info_resizes_rect() {
    let mut s = two_screens();
    s.update_screen_info(1, kvmshare_protocol::message::ScreenInfo { width: 2560, height: 1440, scale: 1.0 });
    let hp = s.layout().find(1).unwrap();
    assert_eq!((hp.rect.w, hp.rect.h), (2560, 1440));
    // Position is untouched.
    assert_eq!(hp.rect.x, -1920);
}

#[test]
fn local_motion_inside_does_nothing() {
    let mut s = two_screens();
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 10 }), vec![]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn crossing_left_edge_switches_to_hp() {
    let mut s = two_screens();
    s.on_client_connected(1);
    // The real cursor reaches the left wall (beacon arms it), then an
    // outward push fires the crossing.
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
    // Virtual position was snapped to hp's entry point, inset past
    // the seam (-(ENTRY_INSET + 1), 540) — never exactly on the wall.
    assert_eq!(s.cursor_pos(), (-(ENTRY_INSET + 1), 540));
}

#[test]
fn interior_beacon_never_arms_and_pushes_do_not_cross() {
    // The real cursor is mid-screen: even a hard outward push (raw
    // deltas run ahead of the visible cursor) must not cross until a
    // beacon puts the real cursor on the wall.
    let mut s = two_screens();
    s.on_local_event(Message::MouseMoveAbs { x: 500, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.mode(), Mode::Local, "interior real cursor must never cross");
    assert_eq!(s.cursor_pos(), (0, 540)); // virtual clamped at the wall
}

#[test]
fn beacon_park_mid_push_crosses_on_the_park_itself() {
    // A fast sweep: raw deltas race to the wall while the real cursor
    // is still travelling. The beacon that parks it there arrives
    // mid-push and must complete the crossing *on the park* — no
    // waiting for the next delta, no dead frame at the boundary.
    let mut s = two_screens();
    s.on_client_connected(1);
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
    assert_eq!(s.mode(), Mode::Local);
    let actions = s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn beacon_park_after_push_went_stale_only_arms() {
    // A flick ends with the cursor at the wall, then the user stops
    // and waits: the push is no longer fresh when the park beacon
    // arrives, so it must only arm — resting at the edge never
    // crosses. A later outward push fires.
    let mut s = two_screens();
    s.on_client_connected(1);
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
    std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    assert_eq!(s.mode(), Mode::Local, "resting at the wall must not cross");
    // A fresh push while parked crosses immediately (confirmed by the
    // beacon).
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn inward_motion_disarms_the_wall() {
    // The cursor parks on the left wall (armed), then the user moves
    // back inside: the wall must disarm, so a later outward push
    // cannot fire until a beacon re-arms it (this is the hysteresis
    // that keeps the seam placement from bouncing).
    let mut s = two_screens();
    s.on_client_connected(1);
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }); // arm left
    // Move away from the wall — the real cursor leaves it.
    s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 }); // inward: disarm
    // An outward push without a fresh wall beacon must not cross.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.mode(), Mode::Local);
    // The real cursor confirms it is back inside.
    s.on_local_event(Message::MouseMoveAbs { x: 100, y: 540 });
    // Let the push record go stale so the next park beacon only arms
    // (a beacon parks the wall mid-push would fire on the park).
    std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
    // The real cursor reaches the wall again: the beacon arms it, and
    // only then does an outward push cross.
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_switch_to_hp(&actions, 540);
}

#[test]
fn sliding_along_the_wall_does_not_cross() {
    // The cursor is pinned on the left wall and slides vertically
    // (aiming at something near the edge). Vertical motion is not an
    // outward push through the left wall, so it must never fire.
    let mut s = two_screens();
    s.on_client_connected(1);
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 0, dy: -200 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.mode(), Mode::Local);
    // Only a genuine outward push crosses.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_switch_to_hp(&actions, 340);
}

#[test]
fn remote_motion_forwards_relative_deltas() {
    // Motion on a client is forwarded *relative* — the client's OS
    // applies its own pointer acceleration, which is what makes the
    // shared cursor feel native (raw deltas replayed as absolute
    // positions made it crawl at speed).
    let mut s = two_screens();
    cross_to_hp(&mut s); // switch to hp
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -10, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1));
}

#[test]
fn remote_motion_is_scaled_by_the_measured_pointer_gain() {
    // The server measures its own px-per-count (e.g. libinput's 0.5
    // at slow speeds) and scales forwarded motion by it, so a client
    // that places its cursor absolutely (1:1) mirrors the server's
    // cursor exactly. Default gain 1.0 leaves motion untouched; a
    // measured gain of 0.5 halves the forwarded counts and the
    // virtual cursor advance — both, so the client's landing spot
    // and the boundary state stay consistent.
    let mut s = two_screens();
    cross_to_hp(&mut s);
    // Gain 1.0 (default / not yet measured): forwarded verbatim.
    assert_eq!(
        s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 }),
        vec![Action::Send(Message::MouseMoveRel { dx: -10, dy: 0 })]
    );
    // The server measured 0.5 px/count (its cursor travels half the
    // raw counts): forwarded motion — and the virtual advance — are
    // halved (rounded per frame).
    s.set_gain(0.5);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -5, dy: 0 })]);
    // Sub-pixel frames round; the average stays 0.5.
    let sum: i64 = (0..20)
        .map(|_| {
            match s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 }).first().unwrap() {
                Action::Send(Message::MouseMoveRel { dx, .. }) => *dx as i64,
                other => panic!("unexpected {other:?}"),
            }
        })
        .sum();
    assert!((5..=15).contains(&sum), "20 x 1 count at 0.5 gain should sum near 10, got {sum}");
    // Local motion is never scaled (beacons re-anchor the virtual
    // cursor there).
    s.force_local();
    assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 0 }), vec![]);
}

#[test]
fn crossing_back_returns_to_local() {
    let mut s = two_screens();
    cross_to_hp(&mut s); // to hp, virtual (-1, 540) = hp's right edge
    // The client's real cursor sits where control placed it: on hp's
    // right wall (the shared edge with pc). The beacon arms it.
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    // A push right while the real cursor is on that wall crosses
    // home.
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn crossing_home_requires_the_real_cursor_at_the_wall() {
    // Deltas alone must never cross back home: after acceleration the
    // raw deltas run far ahead of the client's real cursor. While the
    // real cursor is still interior, an outward overshoot only
    // forwards motion.
    let mut s = two_screens();
    cross_to_hp(&mut s);
    assert_eq!(s.on_remote_beacon(1, 900, 540), vec![]); // real cursor mid-screen
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
    assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 5, dy: 0 })]);
    assert_eq!(s.mode(), Mode::Remote(1), "overshoot while interior must not cross");
    // Only once the client reports its real cursor on the shared wall
    // does the crossing happen — and with the push still fresh it
    // fires on the park itself (no dead frame at the boundary).
    let actions = s.on_remote_beacon(1, 1919, 540);
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn entry_is_inset_past_the_seam() {
    // The cursor enters hp inset past the seam — never exactly on the
    // wall — so hp's first beacon reports an interior cursor, not a
    // park. (An entry exactly on the wall made the first beacon a
    // park with the crossing push still fresh, which bounced the
    // cursor straight back across the seam.)
    let mut s = two_screens();
    s.on_client_connected(1);
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(
        s.on_remote_beacon(1, 1919 - ENTRY_INSET, 540),
        vec![],
        "a beacon at the inset entry point is interior, not a wall park"
    );
    assert_eq!(s.mode(), Mode::Remote(1));
    // A genuine park at the shared wall still crosses home — the
    // inset only stops seam-jitter bounce, not real travel.
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: ENTRY_INSET, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
    // And coming home is inset too: the local beacon at the entry
    // point is interior and must not immediately re-cross.
    assert_eq!(
        s.on_local_event(Message::MouseMoveAbs { x: ENTRY_INSET, y: 540 }),
        vec![],
        "the local beacon at the inset point is interior, not a wall park"
    );
}
