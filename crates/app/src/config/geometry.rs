//! Layout geometry: turning the config into the wire layout, and
//! correcting the local screen's size to the platform's real display.

use kvmshare_core::layout::Layout;
use kvmshare_protocol::message::{Rect, Screen};

use super::Config;

impl Config {
    /// Build the wire layout: id 0 is the server, then 1..n in order.
    pub fn to_layout(&self) -> Layout {
        let screens = self
            .screens
            .iter()
            .enumerate()
            .map(|(id, s)| Screen {
                id: id as u8,
                name: s.name.clone(),
                rect: Rect {
                    x: s.x,
                    y: s.y,
                    w: s.width as i32,
                    h: s.height as i32,
                },
            })
            .collect();
        // Normalize: snap near-adjacent screens into exact contact and
        // align their perpendicular spans, so crossings land pixel-exact
        // regardless of how the GUI's canvas positioned the screens.
        Layout::new(screens).normalized()
    }

    /// Correct the server's own screen (the first entry, id 0) to the
    /// platform's real display geometry, and shift screens that were
    /// placed against the old (wrong) right/bottom edges by the edge
    /// delta so they stay adjacent.
    ///
    /// The local screen's size is not a user choice — it is this
    /// machine's display. A stale config (an old default, or the display
    /// changed since) leaves the boundary walls far from the real cursor
    /// space: the capture beacons the *physical* cursor position, and a
    /// wall the real cursor can never reach can never be armed — so the
    /// cursor cannot cross to the neighbor at all. Correcting only the
    /// size is not enough: a neighbor placed against the old right edge
    /// would then sit in a gap. Screens wholly right of the old right
    /// edge (or below the old bottom edge) shift by exactly the edge
    /// delta — their spacing is preserved, they just follow the edge
    /// they were built against. Screens left of / above the local screen
    /// anchor to the unchanged left/top edge and never move.
    ///
    /// Returns whether the config changed. No-op when the platform
    /// cannot report a display or the size already matches. Called at
    /// every server start; after the first correction the config is
    /// right, so it stops changing.
    pub fn correct_local_screen(&mut self) -> bool {
        match kvmshare_platform::primary_display() {
            Some(info) => self.correct_local_screen_to(info.width.max(1), info.height.max(1)),
            None => false,
        }
    }

    /// The pure part of [`Config::correct_local_screen`], testable
    /// without a platform.
    fn correct_local_screen_to(&mut self, w: u32, h: u32) -> bool {
        let Some(local) = self.screens.first_mut() else { return false };
        if local.width == w && local.height == h {
            return false;
        }
        let (ow, oh) = (local.width, local.height);
        local.width = w;
        local.height = h;
        let (dx, dy) = (w as i32 - ow as i32, h as i32 - oh as i32);
        for s in self.screens.iter_mut().skip(1) {
            if s.x >= ow as i32 {
                s.x += dx;
            }
            if s.y >= oh as i32 {
                s.y += dy;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NetworkConfig, ScreenConfig};

    // A stale local screen size (old default, display changed) is the
    // classic "cursor cannot cross" bug: the capture beacons the real
    // cursor position, and a layout wall the cursor can never reach is
    // never armed. The correction resizes the local screen to reality
    // and shifts neighbors placed against the old edges so they stay
    // adjacent.
    #[test]
    fn correct_local_screen_resizes_and_keeps_neighbors_adjacent() {
        let mut cfg = Config {
            port: 24800,
            screens: vec![
                ScreenConfig { name: "HP".into(), width: 1920, height: 1080, x: 0, y: 0, scale: 1.0 },
                ScreenConfig { name: "pc".into(), width: 3840, height: 1080, x: 1920, y: 0, scale: 1.0 },
                ScreenConfig { name: "left".into(), width: 1920, height: 1080, x: -1920, y: 0, scale: 1.0 },
                ScreenConfig { name: "below".into(), width: 1920, height: 1080, x: 0, y: 1080, scale: 1.0 },
            ],
            network: NetworkConfig::default(),
        };

        assert!(cfg.correct_local_screen_to(1024, 768), "a stale size must be corrected");
        // Local screen now matches reality.
        assert_eq!((cfg.screens[0].width, cfg.screens[0].height), (1024, 768));
        // The right neighbor was adjacent to the old right edge (1920);
        // it follows the corrected edge (1024) — still adjacent, gap
        // preserved.
        assert_eq!(cfg.screens[1].x, 1024);
        // The left neighbor anchors to the unchanged left edge: unmoved.
        assert_eq!(cfg.screens[2].x, -1920);
        // The screen below the old bottom edge (1080) follows the new
        // bottom edge (768).
        assert_eq!(cfg.screens[3].y, 768);

        // A second pass is a no-op: the config is already correct.
        assert!(!cfg.correct_local_screen_to(1024, 768), "a correct config must not change");
        assert_eq!(cfg.screens[1].x, 1024);
    }

    // The correction must never move a screen that sits inside the old
    // local rect (an overlapping config) — only screens placed against
    // the old edges follow them.
    #[test]
    fn correct_local_screen_leaves_overlapping_screens_alone() {
        let mut cfg = Config {
            port: 24800,
            screens: vec![
                ScreenConfig { name: "HP".into(), width: 1920, height: 1080, x: 0, y: 0, scale: 1.0 },
                ScreenConfig { name: "overlap".into(), width: 800, height: 600, x: 1500, y: 200, scale: 1.0 },
            ],
            network: NetworkConfig::default(),
        };
        assert!(cfg.correct_local_screen_to(1024, 768));
        assert_eq!(cfg.screens[1].x, 1500, "an overlapping screen must not shift");
        assert_eq!(cfg.screens[1].y, 200);
    }
}