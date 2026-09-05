use super::*;
use kvmshare_protocol::message::{KeyKind, Rect, Screen};

fn two_screens() -> Session {
    let layout = Layout::new(vec![
        Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
        Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
    ]);
    Session::new(layout, 0)
}

/// Cross from the local screen onto hp (left of pc) the way it
/// really happens: a beacon arms the left wall (the real cursor
/// reached it), then an outward push fires the crossing.
fn cross_to_hp(s: &mut Session) {
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_eq!(s.mode(), Mode::Remote(1));
}

/// Assert the only action is a switch to hp at its right edge.
fn assert_switch_to_hp(actions: &[Action], y: i32) {
    match actions {
        [Action::SwitchTo { to, x, y: ay }] => {
            assert_eq!(*to, 1);
            assert_eq!(*x, 1871); // hp's right edge, inset 48 px from the seam
            assert_eq!(*ay, y);
        }
        other => panic!("expected SwitchTo to hp, got {other:?}"),
    }
}

#[test]
fn assign_screen_id_matches_by_name() {
    let s = two_screens();
    assert_eq!(s.assign_screen_id("hp"), Some(1));
    assert_eq!(s.assign_screen_id("pc"), None); // the server's own screen
    assert_eq!(s.assign_screen_id("nope"), None);
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
    // The real cursor reaches the left wall (beacon arms it), then an
    // outward push fires the crossing.
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
    // Virtual position was snapped to hp's entry point, 48 px past
    // the seam (-49, 540) — never exactly on the wall.
    assert_eq!(s.cursor_pos(), (-49, 540));
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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
}

#[test]
fn entry_is_inset_past_the_seam() {
    // The cursor enters hp 48 px past the seam — never exactly on the
    // wall — so hp's first beacon reports an interior cursor, not a
    // park. (An entry exactly on the wall made the first beacon a
    // park with the crossing push still fresh, which bounced the
    // cursor straight back across the seam.)
    let mut s = two_screens();
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(
        s.on_remote_beacon(1, 1871, 540),
        vec![],
        "a beacon at the inset entry point is interior, not a wall park"
    );
    assert_eq!(s.mode(), Mode::Remote(1));
    // A genuine park at the shared wall still crosses home — the
    // inset only stops seam-jitter bounce, not real travel.
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
    // And coming home is inset too: the local beacon at the entry
    // point is interior and must not immediately re-cross.
    assert_eq!(
        s.on_local_event(Message::MouseMoveAbs { x: 48, y: 540 }),
        vec![],
        "the local beacon at the inset point is interior, not a wall park"
    );
}

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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
            assert_eq!(*x, 1871); // mac's right edge, inset 48 px
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
    cross_to_hp(&mut s); // on hp, virtual (-49, 540)
    // A *local* capture beacon while remote is the hidden parked
    // cursor (meaningless): it must not resync the virtual position.
    let actions = s.on_local_event(Message::MouseMoveAbs { x: 50, y: 60 });
    assert_eq!(actions, vec![]);
    assert_eq!(s.cursor_pos(), (-49, 540)); // untouched
    // Crossing home is driven by the client's own beacon, not the
    // local one.
    assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
    cross_to_hp(&mut s); // virtual (-49, 540): the entry inset
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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
    assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
    assert_eq!(s.mode(), Mode::Local);
    // pc -> hp again, immediately.
    assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
    let actions = s.on_local_event(Message::MouseMoveRel { dx: -2, dy: 0 });
    assert_switch_to_hp(&actions, 540);
    assert_eq!(s.mode(), Mode::Remote(1));
}
