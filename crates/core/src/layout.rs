//! The virtual desktop layout.
//!
//! Screens live on one big 2D plane (virtual coordinates). The server's
//! own screen is the "local" screen; clients are positioned around it by
//! the user in the GUI. The only job of this module is to answer two
//! questions:
//!
//! 1. When the cursor is at `(x, y)`, which screen is it on?
//! 2. If the cursor leaves screen A through edge `dir`, which screen is
//!    next, and where exactly does it enter?
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

    /// Find the neighbor of `from_id` across edge `dir`, together with the
    /// position (in the **neighbor's local coordinates**) where the cursor
    /// enters it.
    ///
    /// The entry point keeps the cursor's offset along the shared edge and
    /// clamps it into the neighbor's span.
    pub fn neighbor(&self, from_id: u8, dir: Direction, at_x: i32, at_y: i32) -> Option<(u8, i32, i32)> {
        let from = self.find(from_id)?;
        let candidate = self.screens.iter().find(|s| s.id != from_id && adjacent(from, s, dir))?;

        let (local_x, local_y) = match dir {
            Direction::Left => (candidate.rect.w - 1, clamp(at_y - candidate.rect.y, 0, candidate.rect.h - 1)),
            Direction::Right => (0, clamp(at_y - candidate.rect.y, 0, candidate.rect.h - 1)),
            Direction::Top => (clamp(at_x - candidate.rect.x, 0, candidate.rect.w - 1), candidate.rect.h - 1),
            Direction::Bottom => (clamp(at_x - candidate.rect.x, 0, candidate.rect.w - 1), 0),
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
}

/// Maximum slack (px) between two screen edges for them to still count as
/// connected.
///
/// GUI-built layouts routinely land a couple of pixels off exact contact
/// (drag rounding, scaled canvas coordinates, aspect-ratio offsets — the
/// real layouts that broke this feature had 2 px and 4 px gaps). An
/// unreachable screen edge because of a tiny gap is far worse than
/// tolerating a small one: the cursor would hit a *dead edge* and refuse
/// to cross in either direction.
const EDGE_TOLERANCE: i32 = 16;

/// Are `a` and `b` adjacent across `dir`, within [`EDGE_TOLERANCE`]?
///
/// The facing edges must be near each other (touching, overlapping, or
/// within the tolerance) **and** the perpendicular spans must overlap
/// (with the same slack) — that is what makes the two screens share an
/// edge region the cursor can travel along.
fn adjacent(a: &Screen, b: &Screen, dir: Direction) -> bool {
    let tol = EDGE_TOLERANCE;
    match dir {
        Direction::Left => {
            near(a.rect.left(), b.rect.right(), tol) && spans_overlap(a.rect.top(), a.rect.bottom(), b.rect.top(), b.rect.bottom(), tol)
        }
        Direction::Right => {
            near(a.rect.right(), b.rect.left(), tol) && spans_overlap(a.rect.top(), a.rect.bottom(), b.rect.top(), b.rect.bottom(), tol)
        }
        Direction::Top => {
            near(a.rect.top(), b.rect.bottom(), tol) && spans_overlap(a.rect.left(), a.rect.right(), b.rect.left(), b.rect.right(), tol)
        }
        Direction::Bottom => {
            near(a.rect.bottom(), b.rect.top(), tol) && spans_overlap(a.rect.left(), a.rect.right(), b.rect.left(), b.rect.right(), tol)
        }
    }
}

/// Are two facing edges within `tol` px of each other (touching counts)?
fn near(edge_a: i32, edge_b: i32, tol: i32) -> bool {
    (edge_a - edge_b).abs() <= tol
}

/// Do two 1-D spans `[a_lo, a_hi)` and `[b_lo, b_hi)` overlap, allowing
/// `tol` px of slack at the ends? Two screens whose spans nearly meet
/// (small perpendicular offset) still share an edge region to travel
/// along; the entry clamp pulls the cursor inside.
fn spans_overlap(a_lo: i32, a_hi: i32, b_lo: i32, b_hi: i32, tol: i32) -> bool {
    a_lo - tol < b_hi && b_lo - tol < a_hi
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmshare_protocol::message::Rect;

    fn screen(id: u8, x: i32, y: i32, w: i32, h: i32) -> Screen {
        Screen { id, name: id.to_string(), rect: Rect { x, y, w, h } }
    }

    /// Classic deskflow setup: pc (server) on the right, hp to its left.
    fn two_screen_layout() -> Layout {
        Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 0, 1920, 1080)])
    }

    #[test]
    fn screen_at_picks_the_right_screen() {
        let l = two_screen_layout();
        assert_eq!(l.screen_at(100, 100).unwrap().id, 0);
        assert_eq!(l.screen_at(-100, 100).unwrap().id, 1);
        assert!(l.screen_at(9999, 9999).is_none());
    }

    #[test]
    fn left_neighbor_is_hp() {
        let l = two_screen_layout();
        let (id, x, y) = l.neighbor(0, Direction::Left, 0, 300).unwrap();
        assert_eq!(id, 1);
        assert_eq!(x, 1919); // hp's right edge
        assert_eq!(y, 300); // vertical offset preserved
    }

    #[test]
    fn right_neighbor_is_pc() {
        let l = two_screen_layout();
        let (id, x, y) = l.neighbor(1, Direction::Right, 0, 400).unwrap();
        assert_eq!(id, 0);
        assert_eq!(x, 0); // pc's left edge
        assert_eq!(y, 400);
    }

    #[test]
    fn no_neighbor_on_outer_edge() {
        let l = two_screen_layout();
        assert!(l.neighbor(1, Direction::Left, 0, 0).is_none());
        assert!(l.neighbor(0, Direction::Right, 0, 0).is_none());
    }

    #[test]
    fn entry_y_is_clamped_into_neighbor() {
        let l = two_screen_layout();
        let (_, _, y) = l.neighbor(0, Direction::Left, 0, 99999).unwrap();
        assert_eq!(y, 1079);
    }

    #[test]
    fn small_gap_still_connects() {
        // Exactly the geometry that broke real layouts: pc sits 2 px
        // right of where hp's edge ends (and 4 px higher). The screens
        // must still connect across both directions.
        let l = Layout::new(vec![screen(0, 2, 0, 1920, 1080), screen(1, -1920, -4, 1920, 1080)]);
        let (id, x, y) = l.neighbor(0, Direction::Left, 2, 300).unwrap();
        assert_eq!(id, 1);
        assert_eq!(x, 1919); // hp's right edge
        assert_eq!(y, 304); // hp is 4 px higher, so pc row 300 maps to hp row 304
        // And back: hp -> pc (virtual row 300 maps straight across).
        let (id, x, y) = l.neighbor(1, Direction::Right, -1, 300).unwrap();
        assert_eq!(id, 0);
        assert_eq!(x, 0); // pc's left edge
        assert_eq!(y, 300);
    }

    #[test]
    fn big_gap_stays_disconnected() {
        // A real gap (280 px) must not connect — dead edges stay dead
        // only when the user actually left a large hole in the layout.
        let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -2200, 0, 1920, 1080)]);
        assert!(l.neighbor(0, Direction::Left, 0, 300).is_none());
    }

    #[test]
    fn partial_overlap_still_connects() {
        // hp sits higher than pc and only overlaps the top half.
        let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, -500, 1920, 1000)]);
        assert!(l.neighbor(0, Direction::Left, 0, 200).is_some());
        // No overlap at all -> not adjacent.
        let l2 = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, -1920, 1200, 1920, 1080)]);
        assert!(l2.neighbor(0, Direction::Left, 0, 200).is_none());
    }

    #[test]
    fn stacked_screens_connect_vertically() {
        let l = Layout::new(vec![screen(0, 0, 0, 1920, 1080), screen(1, 0, -1080, 1920, 1080)]);
        let (id, x, y) = l.neighbor(0, Direction::Top, 500, 0).unwrap();
        assert_eq!(id, 1);
        assert_eq!(x, 500);
        assert_eq!(y, 1079);
    }

    #[test]
    fn exit_direction_detection() {
        let l = two_screen_layout();
        assert_eq!(l.exit_direction(0, -1, 500), Some(Direction::Left));
        assert_eq!(l.exit_direction(0, 1920, 500), Some(Direction::Right));
        assert_eq!(l.exit_direction(0, 500, -1), Some(Direction::Top));
        assert_eq!(l.exit_direction(0, 500, 1080), Some(Direction::Bottom));
        assert_eq!(l.exit_direction(0, 500, 500), None);
    }
}