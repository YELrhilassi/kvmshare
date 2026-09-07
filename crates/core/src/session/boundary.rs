//! The boundary model: what "at the wall" means and how a crossing is
//! armed and fired.
//!
//! A screen edge is a **wall**: the OS pins a real cursor there and no
//! amount of outward motion moves it off-screen. Two cursor streams feed
//! the session:
//!
//! * **Raw deltas** — what the device reported. They are instantaneous
//!   but *pre-acceleration*: they run ahead of the visible cursor, so
//!   they can never decide a crossing alone.
//! * **Real-position beacons** — where the *visible* cursor actually is.
//!   They lag a few milliseconds but are the ground truth.
//!
//! A crossing needs **both, together**:
//!
//! 1. **Arm** — a beacon places the real cursor within [`EDGE_BAND`] px
//!    of a screen edge ("at the wall"). The OS has *committed* the
//!    cursor to that boundary; the user is standing on the seam.
//! 2. **Fire** — an outward push (raw deltas toward that edge) while
//!    armed. At the wall, outward deltas are unambiguous intent.
//!
//! This module owns that state machine's vocabulary — the band width,
//! the freshness windows, the direction bits and the push latches.

use std::time::{Duration, Instant};

use kvmshare_protocol::message::Rect;

use crate::layout::Direction;

/// Wall-band width (px). A real cursor within [`EDGE_BAND`] px of a
/// screen edge counts as "at the wall". Two pixels absorb pointer
/// position quantization (a pinned cursor reports the very last pixel on
/// both X11 and Windows; one pixel of slack covers off-by-ones) without
/// making the band wide enough to steal aim from UI elements near the
/// edge. The band is *not* what makes a crossing fire — the push is — it
/// only makes the "at the wall" test robust.
pub const EDGE_BAND: i32 = 2;

/// How recent an outward push must be for a beacon that *parks* the real
/// cursor on a wall to complete the crossing on the park itself.
/// Crossing on the next delta would otherwise add one motion frame of
/// dead time at the exact moment a crossing should feel seamless — and a
/// sweep that ends precisely at the wall (the flick-and-stop case) would
/// not cross until the user pushed again, which feels sticky. A beacon
/// alone — the user resting at the edge — never crosses: the push within
/// this window is what marks intent. Far shorter than a hover, long
/// enough to cover the beacon lag between the last delta and the park
/// confirmation even under load.
pub const EDGE_PUSH_FRESH: Duration = Duration::from_millis(40);

/// How long outward deltas must keep pushing against a screen edge —
/// with the *virtual* cursor outside the rect and no beacon correcting
/// it — before the switch fires anyway. This is the rescue path for a
/// stalled beacon stream (the OS pinned the pointer at the edge, motion
/// events — and with them beacons — stop, and only raw deltas keep
/// flowing; or a peer that cannot report its real cursor at all). The
/// virtual cursor must actually be *outside* the rect, so an interior
/// real cursor can never trip it. Far shorter than an accidental hover,
/// long enough that a beacon lag can never fire a crossing on its own.
pub const EDGE_PUSH_FALLBACK: Duration = Duration::from_millis(150);

/// How far past a shared edge the cursor is placed on entry (px).
/// Placing it exactly ON the edge makes the destination's very first
/// beacon report a park at the entry wall with the crossing push still
/// fresh — an immediate bounce back across the seam, and with the cursor
/// pinned on the wall any continued push re-fires it (the boundary
/// oscillation seen in the field: crossings ping-ponged within
/// milliseconds, hammering the grab/release machinery on both machines
/// and occasionally leaving one input-dead). Insetting the entry point
/// gives the continued motion room to read as travel into the screen:
/// the boundary re-arms only after the cursor has actually moved
/// [`ENTRY_INSET`] px away from the seam, so a resting or jittering
/// cursor can never bounce while a real push-through still works. It
/// also stops the reverse bounce — coming home to a cursor sitting
/// exactly on the wall.
pub const ENTRY_INSET: i32 = 48;

/// How old a client cursor-position beacon may be and still be treated
/// as the real cursor's location. Beacons arrive every few ms while the
/// client is controlled; anything older than this means the stream
/// stalled (a wedged client, a network drop) and the wall-arm expires —
/// outward pushes then need the sustained fallback instead, so a stale
/// "at the wall" report can never fire a crossing the user did not push
/// for.
pub const REMOTE_BEACON_FRESH: Duration = Duration::from_millis(120);

// Direction bits for the "which walls is the real cursor on" mask. A
// corner can set two bits at once; the push direction picks which one
// fires. `pub(super)` so the session's tests can assert `wall_bits`
// output directly.
pub(super) const BIT_LEFT: u8 = 1 << 0;
pub(super) const BIT_RIGHT: u8 = 1 << 1;
pub(super) const BIT_TOP: u8 = 1 << 2;
pub(super) const BIT_BOTTOM: u8 = 1 << 3;

/// The bit for `dir` in the wall mask.
pub fn bit(dir: Direction) -> u8 {
    match dir {
        Direction::Left => BIT_LEFT,
        Direction::Right => BIT_RIGHT,
        Direction::Top => BIT_TOP,
        Direction::Bottom => BIT_BOTTOM,
    }
}

/// The four directions, in a stable order (left/right before top/bottom,
/// mirroring `Layout::exit_direction` so corners resolve consistently).
pub const DIRS: [Direction; 4] = [Direction::Left, Direction::Right, Direction::Top, Direction::Bottom];

/// Does this delta push *outward* through `dir` (toward that edge)?
pub fn pushes_outward(dir: Direction, dx: i32, dy: i32) -> bool {
    match dir {
        Direction::Left => dx < 0,
        Direction::Right => dx > 0,
        Direction::Top => dy < 0,
        Direction::Bottom => dy > 0,
    }
}

/// Does this delta move *away* from `dir`'s wall (back into the screen)?
pub fn pulls_inward(dir: Direction, dx: i32, dy: i32) -> bool {
    match dir {
        Direction::Left => dx > 0,
        Direction::Right => dx < 0,
        Direction::Top => dy > 0,
        Direction::Bottom => dy < 0,
    }
}

/// Which walls a *local* position sits on, if any. `x`/`y` are local
/// pixels inside `rect` (or up to a hair past it — beacon lag). The OS
/// pins the pointer at the outer pixel column/row (`0` / `w - 1`), so
/// those are the walls; [`EDGE_BAND`] gives the test slack. Degenerate
/// screens smaller than two bands arm nothing.
pub(super) fn wall_bits(rect: &Rect, x: i32, y: i32) -> u8 {
    let mut bits = 0;
    if rect.w > 2 * EDGE_BAND {
        if x <= EDGE_BAND - 1 {
            bits |= BIT_LEFT;
        }
        if x >= rect.w - EDGE_BAND {
            bits |= BIT_RIGHT;
        }
    }
    if rect.h > 2 * EDGE_BAND {
        if y <= EDGE_BAND - 1 {
            bits |= BIT_TOP;
        }
        if y >= rect.h - EDGE_BAND {
            bits |= BIT_BOTTOM;
        }
    }
    bits
}

/// One outward push: which edge it pushed through and when. Used to give
/// a beacon that parks the cursor on a wall the "is the user mid-sweep?"
/// answer ([`EDGE_PUSH_FRESH`]).
#[derive(Debug, Clone, Copy)]
pub struct Push {
    pub dir: Direction,
    pub at: Instant,
}

/// A stalled-stream fallback in progress: which edge the virtual cursor
/// is pushed out through and when that started.
#[derive(Debug, Clone, Copy)]
pub struct Pushing {
    pub dir: Direction,
    pub since: Instant,
}