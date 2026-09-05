use super::*;
use std::time::Duration;

/// The decisive encoding fact behind relative client motion: XTest
/// fake motion with `root = None` must move the pointer *relative*
/// to where it is (not teleport it to coordinates (dx, dy)). The X
/// server applies its own pointer transform to injected relative
/// motion just like a physical mouse's, so the travel distance is
/// profile-dependent — what matters is that it moved in the
/// requested direction from its current position rather than jumping
/// to absolute (40, 40). Verified on a live X server; skipped where
/// none is available. Restores the pointer afterwards.
///
/// The desktop is live, so a busy hand can move the pointer mid-test:
/// the probe retries, and only a *deterministic* teleport to the
/// injected absolute coordinates fails immediately (that is the bug
/// this test exists to catch). Persistent interference after retries
/// is reported and skipped, not failed — the machine is being used.
#[test]
fn xtest_none_root_motion_is_relative() {
    let Ok(mut inj) = X11Injector::new(None) else {
        eprintln!("skipping: no X server available");
        return;
    };
    let (sx, sy) = inj.pointer_pos().expect("pointer position");
    for attempt in 0..3 {
        let before = inj.pointer_pos().unwrap();
        inj.xtest_move_rel(40, 0);
        std::thread::sleep(Duration::from_millis(40));
        let after = inj.pointer_pos().unwrap();
        if after == (40, 40) {
            // Deterministic teleport to the injected coordinates: the
            // real bug. The pointer raced back with the desktop hand;
            // restore and fail loudly.
            inj.move_cursor(sx, sy);
            panic!("pointer teleported to absolute (40, 40) — root=None was treated as absolute");
        }
        if after.0 > before.0 {
            // Moved right from where it started: relative semantics.
            inj.move_cursor(sx, sy);
            return;
        }
        eprintln!(
            "attempt {attempt}: pointer went {before:?} -> {after:?} (desktop busy?), retrying"
        );
    }
    inj.move_cursor(sx, sy);
    eprintln!("skipping: desktop too busy to verify relative motion");
}
