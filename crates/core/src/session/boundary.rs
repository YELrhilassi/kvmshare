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
//! Both sides of the session — the server's own screen and each remote
//! screen — run the *same* machine, [`BoundarySide`]. The session owns
//! two instances and a per-side "what happens on a confirmed crossing"
//! callback; nothing about the logic is written twice.

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
/// exactly on the wall. Sized just past the wall band plus comfortable
/// slack: large enough that a resting or jittering cursor can never
/// bounce, small enough that entry feels like landing on the seam (a
/// 48 px inset read as "the cursor landed further from the edge than
/// where I crossed").
pub const ENTRY_INSET: i32 = 24;

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
// fires.
const BIT_LEFT: u8 = 1 << 0;
const BIT_RIGHT: u8 = 1 << 1;
const BIT_TOP: u8 = 1 << 2;
const BIT_BOTTOM: u8 = 1 << 3;

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

/// One side of the session's boundary state: everything the arm/push/
/// fire machine needs for **one screen** the cursor can be on. The
/// session keeps two instances — [`Session::local_side`] for the
/// server's own screen and [`Session::remote_side`] for the client
/// screen the cursor is on — and feeds them through the same handlers,
/// so the crossing logic exists exactly once.
///
/// The fields are an implementation unit; nothing outside `session`
/// reads them.
#[derive(Default)]
pub(super) struct BoundarySide {
    /// Walls the *real* cursor currently sits on (a beacon armed them).
    pub(super) at_wall: u8,
    /// The most recent outward push through each edge (used to give a
    /// park beacon the "is the user mid-sweep?" answer).
    pub(super) last_out: Option<Push>,
    /// A stalled-stream fallback in progress.
    pub(super) pushing: Option<Pushing>,
    /// When the last real-position beacon arrived (`None` = none yet).
    /// Only meaningful on the remote side (locally the capture streams
    /// beacons continuously); the machine consults it for freshness.
    pub(super) beacon_at: Option<Instant>,
}

impl BoundarySide {
    /// Reset every latch: the cursor has just arrived somewhere new (or
    /// the layout changed), and any arm from before is meaningless.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// A beacon reported the real cursor at local `(x, y)` inside
    /// `rect`. Re-arms the wall bits; returns the directions whose wall
    /// was **newly** armed while the user was mid-push (the park-fire
    /// candidates, freshest-intent first logic lives with the caller).
    pub(super) fn on_beacon(&mut self, rect: &Rect, x: i32, y: i32) -> u8 {
        self.beacon_at = Some(Instant::now());
        let bits = wall_bits(rect, x, y);
        let newly = bits & !self.at_wall;
        self.at_wall = bits;
        if bits == 0 {
            // The real cursor is back inside: any wall-arm or push
            // attempt was a transient overshoot, and it is over.
            self.pushing = None;
        }
        newly
    }

    /// Is the wall for `dir` armed right now?
    pub(super) fn armed(&self, dir: Direction) -> bool {
        self.at_wall & bit(dir) != 0
    }

    /// Is there a *fresh* outward push through `dir` (the user was
    /// mid-sweep within [`EDGE_PUSH_FRESH`])?
    pub(super) fn push_fresh(&self, dir: Direction) -> bool {
        self.last_out.is_some_and(|p| p.dir == dir && p.at.elapsed() < EDGE_PUSH_FRESH)
    }

    /// Record one delta's push/disarm effect on the wall-arm state.
    /// Returns `true` when the delta is an outward push through an
    /// **armed** wall — the immediate-fire case.
    pub(super) fn on_delta(&mut self, dir: Direction, dx: i32, dy: i32) -> bool {
        if pushes_outward(dir, dx, dy) {
            self.last_out = Some(Push { dir, at: Instant::now() });
            self.armed(dir)
        } else if pulls_inward(dir, dx, dy) {
            // Leaving the edge: the next push must re-arm from a beacon.
            // This is what makes the seam placement on entry never
            // bounce.
            self.at_wall &= !bit(dir);
            false
        } else {
            false
        }
    }

    /// Advance the stalled-stream fallback for `dir` (the virtual
    /// cursor is outside the rect and raw deltas keep pushing). Returns
    /// `true` exactly when the sustained push has lasted
    /// [`EDGE_PUSH_FALLBACK`] — the fire signal.
    pub(super) fn tick_fallback(&mut self, dir: Direction, fresh_beacon: bool) -> bool {
        let sustained = !fresh_beacon
            && self.pushing.is_some_and(|p| p.dir == dir && p.since.elapsed() >= EDGE_PUSH_FALLBACK);
        if !sustained {
            self.pushing = match self.pushing {
                Some(p) if p.dir == dir => self.pushing,
                _ => Some(Pushing { dir, since: Instant::now() }),
            };
        }
        sustained
    }

    /// The fallback latch is over (a fire or an inward move ended it).
    pub(super) fn stop_fallback(&mut self) {
        self.pushing = None;
    }
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

#[cfg(test)]
#[path = "boundary_tests.rs"]
mod tests;
