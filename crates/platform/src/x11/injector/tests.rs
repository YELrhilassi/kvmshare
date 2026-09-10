use super::*;

/// The decisive encoding fact behind the absolute client motion model:
/// `move_cursor` must place the pointer *exactly* at the requested
/// root-window coordinates, and `move_rel` must accumulate from the
/// last placed position rather than teleporting to (dx, dy).
///
/// The earlier design used relative XTest fake motion (`root = None`),
/// which real X servers silently treat as *absolute* coordinates — a
/// "relative" move of (30,20) teleports the pointer to (30,20), the
/// origin corner. Verified on live hardware: after warping to (500,500),
/// a "relative" fake motion of (30,20) landed the pointer at (30,20).
/// The absolute model has no such failure mode — a warp is exact.
///
/// The desktop is live, so a busy hand can move the pointer mid-test:
/// `move_rel` asserts the commanded position only, never the OS-visible
/// cursor (which a live hand can legitimately move).
#[test]
fn move_cursor_is_exact_absolute_placement() {
    let Ok(mut inj) = X11Injector::new(None) else {
        eprintln!("skipping: no X server available");
        return;
    };
    // Any interior point (clamped to the screen if it is small).
    let (w, h) = inj.bounds;
    let target = ((w / 2) as i32, (h / 2) as i32);
    inj.move_cursor(target.0, target.1);
    assert_eq!(inj.pos, target, "command tracks the exact placement");
}

/// `move_rel` accumulates into the commanded position: a stream of
/// deltas lands on their sum, not on each (dx, dy) — the collapse the
/// relative-XTest bug produced.
#[test]
fn move_rel_accumulates_from_current_position() {
    let Ok(mut inj) = X11Injector::new(None) else {
        eprintln!("skipping: no X server available");
        return;
    };
    // Anchor the command at an interior point first (the injector now
    // starts at the real cursor position, which a live hand could have
    // left anywhere — the accumulation must be measured from a known
    // interior start, clear of the clamps).
    let (w, h) = inj.bounds;
    inj.move_cursor((w / 2) as i32, (h / 2) as i32);
    let start = inj.pos;
    inj.move_rel(40, 0);
    assert_eq!(
        inj.pos,
        (start.0 + 40, start.1),
        "first delta advances the command"
    );
    inj.move_rel(-10, 25);
    assert_eq!(inj.pos, (start.0 + 30, start.1 + 25), "deltas accumulate");
}

/// Screen bounds clamp the commanded position: an off-screen command
/// never runs past the visible edge (reversing at the edge must move
/// immediately, not retrace an overshoot).
#[test]
fn move_rel_clamps_to_screen_bounds() {
    let Ok(mut inj) = X11Injector::new(None) else {
        eprintln!("skipping: no X server available");
        return;
    };
    let (w, h) = inj.bounds;
    inj.pos = ((w as i32) - 2, (h as i32) - 2);
    inj.move_rel(50, 50);
    assert_eq!(
        inj.pos,
        ((w as i32) - 1, (h as i32) - 1),
        "clamped to the last pixel"
    );
    inj.move_rel(-10000, -10000);
    assert_eq!(inj.pos, (0, 0), "clamped to the origin");
}
