//! Session tests, split by concern: layout/admission, the crossing
//! state machine, and the remote-stream handlers. Shared fixtures live
//! here and are pulled in by the sibling modules via `use super::*`.

use std::time::Duration;

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
    s.on_client_connected(1); // hp must be online to be a destination
    s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
    s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    assert_eq!(s.mode(), Mode::Remote(1));
}

/// Assert the only action is a switch to hp at its right edge.
fn assert_switch_to_hp(actions: &[Action], y: i32) {
    match actions {
        [Action::SwitchTo { to, x, y: ay }] => {
            assert_eq!(*to, 1);
            assert_eq!(*x, 1919 - ENTRY_INSET); // hp's right edge, inset from the seam
            assert_eq!(*ay, y);
        }
        other => panic!("expected SwitchTo to hp, got {other:?}"),
    }
}

mod boundary_tests;
mod remote_tests;
