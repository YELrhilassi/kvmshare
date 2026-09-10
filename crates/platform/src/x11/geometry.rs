//! The **visible desktop**: the union of the active outputs (RandR),
//! expressed as an origin in root-window pixels and a size in visible
//! pixels.
//!
//! The X root window is not the desktop the user sees: it spans every
//! output *ever* configured, so a disconnected monitor leaves a slab of
//! black virtual space (a laptop with an external 1920×1080 monitor
//! whose root is 3840 wide is the field case). Reporting the root as the
//! screen makes the cursor drivable into invisible space, places entry
//! points where nothing is visible, and makes the reported geometry lie
//! about where the true screen corners are. The visible desktop is the
//! truth: it is the bounding box of the connected, enabled outputs.
//!
//! Both X11 roles use it:
//!
//! * the **client** reports the visible size as its screen and steers
//!   the cursor in visible-local pixels (`local = root − origin`);
//! * the **server** reports the visible size as its screen, forwards
//!   position beacons in visible-local pixels, and warps through the
//!   same translation.
//!
//! When RandR is unavailable or reports nothing usable (a headless
//! session, an ancient server), the fallback is the whole root window
//! with a zero origin — today's behavior, still correct where there is
//! only one output.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// The visible desktop on one X screen: where it sits in root pixels
/// and how big it is (visible pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisibleDesktop {
    /// The visible region's top-left corner in root-window pixels.
    pub origin: (i32, i32),
    /// The visible region's size, in visible pixels.
    pub size: (u32, u32),
}

impl VisibleDesktop {
    /// The whole root window with a zero origin — the fallback when no
    /// RandR layout is available (headless, ancient server, query
    /// failure). One output at the origin is indistinguishable from the
    /// true layout, which is exactly why the fallback is safe.
    pub fn whole_root(width: u32, height: u32) -> Self {
        Self {
            origin: (0, 0),
            size: (width, height),
        }
    }

    /// Translate visible-local pixels into root-window pixels.
    pub fn to_root(&self, x: i32, y: i32) -> (i32, i32) {
        (x + self.origin.0, y + self.origin.1)
    }

    /// Translate root-window pixels into visible-local pixels.
    pub fn from_root(&self, x: i32, y: i32) -> (i32, i32) {
        (x - self.origin.0, y - self.origin.1)
    }
}

/// Query the visible desktop (union of connected, enabled outputs) on
/// `conn`'s screen `screen_num`. `None` when RandR is missing or the
/// layout cannot be read — the caller falls back to the whole root.
pub fn visible_desktop(conn: &RustConnection, screen_num: usize) -> Option<VisibleDesktop> {
    let setup = conn.setup();
    let root = setup.roots.get(screen_num)?.root;
    if conn
        .extension_information(randr::X11_EXTENSION_NAME)
        .ok()?
        .is_none()
    {
        return None;
    }
    let resources = conn
        .randr_get_screen_resources_current(root)
        .ok()?
        .reply()
        .ok()?;
    let mut min = (i32::MAX, i32::MAX);
    let mut max = (i32::MIN, i32::MIN);
    let mut found = false;
    for output in resources.outputs {
        let info = match conn
            .randr_get_output_info(output, x11rb::CURRENT_TIME)
            .ok()?
            .reply()
            .ok()
        {
            Some(info) => info,
            None => continue,
        };
        // Only outputs that are physically connected and have an active
        // mode (a CRTC assigned) are visible. A connected-but-off output
        // (the laptop panel on an external-monitor desktop) is exactly
        // the phantom space the visible desktop must exclude.
        if info.connection != randr::Connection::CONNECTED || info.crtc == x11rb::NONE {
            continue;
        }
        let Some(crtc) = conn
            .randr_get_crtc_info(info.crtc, x11rb::CURRENT_TIME)
            .ok()?
            .reply()
            .ok()
        else {
            continue;
        };
        let (x, y) = (crtc.x as i32, crtc.y as i32);
        let (w, h) = (crtc.width as i32, crtc.height as i32);
        if w <= 0 || h <= 0 {
            continue;
        }
        min.0 = min.0.min(x);
        min.1 = min.1.min(y);
        max.0 = max.0.max(x + w);
        max.1 = max.1.max(y + h);
        found = true;
    }
    if !found {
        return None;
    }
    Some(VisibleDesktop {
        origin: min,
        size: ((max.0 - min.0) as u32, (max.1 - min.1) as u32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_root_has_zero_origin() {
        let v = VisibleDesktop::whole_root(3840, 1080);
        assert_eq!(v.origin, (0, 0));
        assert_eq!(v.size, (3840, 1080));
        assert_eq!(v.to_root(48, 100), (48, 100));
        assert_eq!(v.from_root(48, 100), (48, 100));
    }

    #[test]
    fn translation_shifts_by_the_origin() {
        // A monitor whose visible area sits at (1920, 200) in the root
        // (a laptop panel right of the primary, for instance).
        let v = VisibleDesktop {
            origin: (1920, 200),
            size: (1920, 1080),
        };
        // Visible-local (0, 0) is the *true* visible corner, which lives
        // at the origin in root pixels.
        assert_eq!(v.to_root(0, 0), (1920, 200));
        assert_eq!(v.to_root(48, 869), (1968, 1069));
        // And the inverse maps a root position back into local pixels.
        assert_eq!(v.from_root(1968, 1069), (48, 869));
        assert_eq!(v.from_root(1920, 200), (0, 0));
    }

    #[test]
    fn query_on_live_display_matches_root_or_shrinks() {
        // Best-effort live check: whatever RandR reports must be
        // consistent with the root — the visible region can never be
        // larger than the root window, and its translation must land
        // back inside it.
        let Ok((conn, screen_num)) = RustConnection::connect(None) else {
            eprintln!("skipping: no X server available");
            return;
        };
        let root = &conn.setup().roots[screen_num];
        let Some(v) = visible_desktop(&conn, screen_num) else {
            eprintln!("skipping: no RandR layout");
            return;
        };
        assert!(v.size.0 <= root.width_in_pixels as u32);
        assert!(v.size.1 <= root.height_in_pixels as u32);
        assert!(v.origin.0 >= 0 && v.origin.1 >= 0);
        let (rx, ry) = v.to_root(v.size.0 as i32 - 1, v.size.1 as i32 - 1);
        assert!(rx < root.width_in_pixels as i32);
        assert!(ry < root.height_in_pixels as i32);
        assert_eq!(v.from_root(v.origin.0, v.origin.1), (0, 0));
    }
}
