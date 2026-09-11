//! The virtual desktop layout.
//!
//! Screens live on one big 2D plane (virtual coordinates). The server's
//! own screen is the "local" screen; clients are positioned around it by
//! the user in the GUI. The only job of this module is to answer three
//! questions:
//!
//! 1. When the cursor is at `(x, y)`, which screen is it on?
//! 2. If the cursor leaves screen A through edge `dir`, which screen is
//!    next, and where exactly does it enter?
//! 3. Is anything about the arrangement ambiguous or surprising? (See
//!    [`Layout::issues`] — the GUI surfaces the answers.)
//!
//! Everything else builds on those answers.

use kvmshare_protocol::message::Screen;

/// The four screen edges. "Top" means moving *up* (decreasing y).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Top,
    Bottom,
}

/// A full desktop layout.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub screens: Vec<Screen>,
}

impl Layout {
    /// Build a layout from wire-format screens.
    pub fn new(screens: Vec<Screen>) -> Self {
        Self { screens }
    }

    /// Return a copy with layout noise removed: near-adjacent screens
    /// (within [`EDGE_TOLERANCE`]) are snapped into exact contact and
    /// their perpendicular spans aligned, so the boundary math is
    /// pixel-exact. GUI-built layouts routinely drift a few pixels off
    /// contact (drag rounding, scaled canvas coordinates); a 2 px gap or
    /// a 4 px vertical offset turns a crossing at y=500 into an entry at
    /// y=504 — a visible seam glitch that reads as "the boundary is
    /// off". The server applies this to every layout it adopts (config
    /// load and hot reload), so the user never has to fight pixel
    /// alignment in the GUI.
    ///
    /// Gaps the user placed deliberately are *not* closed: only pairs
    /// already within [`EDGE_TOLERANCE`] snap, and crossing across a
    /// larger gap works anyway (see [`Layout::neighbor`]) — snapping is
    /// cosmetic, not functional.
    pub fn normalized(&self) -> Layout {
        let mut screens = self.screens.clone();
        // Closing one gap can bring another pair into snapping range, so
        // iterate to a fixed point. Bounded by the screen count; a pass
        // that changes nothing stops early.
        for _ in 0..8 {
            let mut changed = false;
            for i in 0..screens.len() {
                let a = screens[i].rect;
                for j in 0..screens.len() {
                    if i == j {
                        continue;
                    }
                    let b = screens[j].rect;
                    let mut nb = b;
                    // Side-by-side: snap the facing edges together and
                    // align the shared edge's perpendicular span.
                    if near(a.right(), b.left(), EDGE_TOLERANCE)
                        && spans_overlap(a.top(), a.bottom(), b.top(), b.bottom(), EDGE_TOLERANCE)
                    {
                        nb.x = a.right();
                        if (nb.y - a.y).abs() <= EDGE_TOLERANCE {
                            nb.y = a.y;
                        }
                    } else if near(b.right(), a.left(), EDGE_TOLERANCE)
                        && spans_overlap(a.top(), a.bottom(), b.top(), b.bottom(), EDGE_TOLERANCE)
                    {
                        nb.x = a.left() - b.w;
                        if (nb.y - a.y).abs() <= EDGE_TOLERANCE {
                            nb.y = a.y;
                        }
                    }
                    // Stacked: same for top/bottom neighbors.
                    if near(a.bottom(), b.top(), EDGE_TOLERANCE)
                        && spans_overlap(a.left(), a.right(), b.left(), b.right(), EDGE_TOLERANCE)
                    {
                        nb.y = a.bottom();
                        if (nb.x - a.x).abs() <= EDGE_TOLERANCE {
                            nb.x = a.x;
                        }
                    } else if near(b.bottom(), a.top(), EDGE_TOLERANCE)
                        && spans_overlap(a.left(), a.right(), b.left(), b.right(), EDGE_TOLERANCE)
                    {
                        nb.y = a.top() - b.h;
                        if (nb.x - a.x).abs() <= EDGE_TOLERANCE {
                            nb.x = a.x;
                        }
                    }
                    if nb != b {
                        screens[j].rect = nb;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        Layout::new(screens)
    }

    pub fn find(&self, id: u8) -> Option<&Screen> {
        self.screens.iter().find(|s| s.id == id)
    }

    /// The screen containing the virtual point, if any.
    pub fn screen_at(&self, x: i32, y: i32) -> Option<&Screen> {
        self.screens.iter().find(|s| s.rect.contains(x, y))
    }

    /// Find the destination when leaving screen `from_id` through edge
    /// `dir`, with the entry position in the **destination's local
    /// coordinates**.
    ///
    /// The model is a **ray cast**, not edge contact: the cursor fires a
    /// ray from its exit point through that edge, and the first screen
    /// the ray reaches is the destination. A deliberate gap between
    /// screens is crossed like contact — the ray flies over it — so a
    /// layout with spacing (or with screens of different sizes at
    /// different offsets) behaves exactly like a tightly packed one. The
    /// old edge-contact rule turned any gap past a few pixels into a
    /// dead edge that silently refused to cross; with a ray, *every*
    /// edge with any screen beyond it is alive.
    ///
    /// Destination choice: among the screens lying in `dir`'s half-plane
    /// (for `Left`: every screen whose right edge is at or left of this
    /// screen's left edge, with a small tolerance so a few pixels of
    /// accidental overlap still connects), the one whose span across the
    /// ray comes closest to the exit point. Inside the span (the common
    /// case) the exit position carries straight across; outside it, the
    /// entry clamps to the nearest corner of the span — the destination
    /// exists in that direction, so landing at its nearest point is what
    /// the placement means.
    pub fn neighbor(&self, from_id: u8, dir: Direction, at_x: i32, at_y: i32) -> Option<(u8, i32, i32)> {
        let from = self.find(from_id)?;
        // `exit_perp`: the coordinate across `dir`'s span (the ray's
        // lateral position); the ray's own axis starts at the facing
        // edge and travels in `dir`.
        let exit_perp = match dir {
            Direction::Left | Direction::Right => at_y,
            Direction::Top | Direction::Bottom => at_x,
        };

        // Candidates: screens strictly in `dir`'s half-plane past this
        // screen's facing edge (`EDGE_TOLERANCE` of slack absorbs tiny
        // overlaps). Score = perpendicular distance from the exit point
        // to the candidate's span (0 when the exit point is inside it),
        // then parallel distance (nearest screen wins a tie).
        let mut best: Option<(i32, i32, &Screen)> = None;
        for s in &self.screens {
            if s.id == from_id {
                continue;
            }
            let (s_lo, s_hi, s_edge, past) = match dir {
                Direction::Left => (s.rect.top(), s.rect.bottom(), s.rect.right(), s.rect.right() <= from.rect.left() + EDGE_TOLERANCE),
                Direction::Right => (s.rect.top(), s.rect.bottom(), s.rect.left(), s.rect.left() >= from.rect.right() - EDGE_TOLERANCE),
                Direction::Top => (s.rect.left(), s.rect.right(), s.rect.bottom(), s.rect.bottom() <= from.rect.top() + EDGE_TOLERANCE),
                Direction::Bottom => (s.rect.left(), s.rect.right(), s.rect.top(), s.rect.top() >= from.rect.bottom() - EDGE_TOLERANCE),
            };
            if !past {
                continue;
            }
            let perp = if exit_perp < s_lo {
                s_lo - exit_perp
            } else if exit_perp >= s_hi {
                exit_perp - (s_hi - 1).max(s_lo)
            } else {
                0
            };
            let parallel = match dir {
                Direction::Left => (from.rect.left() - s_edge).max(0),
                Direction::Right => (s_edge - from.rect.right()).max(0),
                Direction::Top => (from.rect.top() - s_edge).max(0),
                Direction::Bottom => (s_edge - from.rect.bottom()).max(0),
            };
            if best.is_none() || (perp, parallel) < (best.unwrap().0, best.unwrap().1) {
                best = Some((perp, parallel, s));
            }
        }
        let (_, _, candidate) = best?;

        // Carry the exit position across; clamp into the destination's
        // span (covers offsets, partial overlaps and corner destinations
        // alike). `.max(0)` keeps a degenerate zero-size rect sane.
        let (local_x, local_y) = match dir {
            Direction::Left => (
                (candidate.rect.w - 1).max(0),
                clamp(at_y - candidate.rect.y, 0, (candidate.rect.h - 1).max(0)),
            ),
            Direction::Right => (
                0,
                clamp(at_y - candidate.rect.y, 0, (candidate.rect.h - 1).max(0)),
            ),
            Direction::Top => (
                clamp(at_x - candidate.rect.x, 0, (candidate.rect.w - 1).max(0)),
                (candidate.rect.h - 1).max(0),
            ),
            Direction::Bottom => (
                clamp(at_x - candidate.rect.x, 0, (candidate.rect.w - 1).max(0)),
                0,
            ),
        };
        Some((candidate.id, local_x, local_y))
    }

    /// Which direction does the cursor leave `from_id`'s rect when moving
    /// to `(x, y)` (virtual)? `None` if `(x, y)` is still inside.
    pub fn exit_direction(&self, from_id: u8, x: i32, y: i32) -> Option<Direction> {
        let s = self.find(from_id)?;
        if x < s.rect.left() {
            Some(Direction::Left)
        } else if x >= s.rect.right() {
            Some(Direction::Right)
        } else if y < s.rect.top() {
            Some(Direction::Top)
        } else if y >= s.rect.bottom() {
            Some(Direction::Bottom)
        } else {
            None
        }
    }

    /// Everything about the arrangement the user should know before
    /// relying on it. Returns human-readable, *actionable* warnings —
    /// the GUI lists them under the canvas so problems never surface as
    /// silent misbehavior. Empty = the layout is unambiguous.
    pub fn issues(&self) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0..self.screens.len() {
            let a = &self.screens[i];
            if a.rect.w <= 0 || a.rect.h <= 0 {
                out.push(format!("screen {:?} has no size — give it its real resolution", a.name));
                continue;
            }
            for b in &self.screens[i + 1..] {
                let overlap_x = a.rect.left() < b.rect.right() && b.rect.left() < a.rect.right();
                let overlap_y = a.rect.top() < b.rect.bottom() && b.rect.top() < a.rect.bottom();
                if overlap_x && overlap_y {
                    out.push(format!(
                        "screens {:?} and {:?} overlap — the cursor can only be on one; move them apart",
                        a.name, b.name
                    ));
                }
            }
        }
        out
    }
}

/// Maximum slack (px) between two screen edges for snap/normalize logic,
/// and for the ray cast's small overlap tolerance.
const EDGE_TOLERANCE: i32 = 16;

/// Are two facing edges within `tol` px of each other (touching counts)?
fn near(edge_a: i32, edge_b: i32, tol: i32) -> bool {
    (edge_a - edge_b).abs() <= tol
}

/// Do two 1-D spans `[a_lo, a_hi)` and `[b_lo, b_hi)` overlap, allowing
/// `tol` px of slack at the ends? Used by normalization only — crossing
/// uses the ray cast and no longer needs span overlap.
fn spans_overlap(a_lo: i32, a_hi: i32, b_lo: i32, b_hi: i32, tol: i32) -> bool {
    a_lo - tol < b_hi && b_lo - tol < a_hi
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

#[cfg(test)]
mod tests;
