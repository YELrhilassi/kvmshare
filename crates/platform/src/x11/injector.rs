//! The client's control over its own screen: move the cursor, inject
//! buttons/keys/wheel, read/write the clipboard. Implements [`Injector`]
//! from the core crate. The local cursor stays **visible** while being
//! controlled: it *is* the shared cursor (the server hides its own
//! while the cursor is away, so hiding the client's too would leave no
//! visible cursor anywhere).
//!
//! ## Motion model: absolute placement
//!
//! This backend places the cursor **absolutely** (`absolute_motion()`
//! returns `true`): every received delta is accumulated into a commanded
//! position and the cursor is warped exactly there, on the motion
//! thread's tick cadence. This is the same model the Windows client
//! uses, and it is the only one that works reliably here — a **relative**
//! XTest stream does not:
//!
//! The natural X11 idiom for relative motion is a fake `MotionNotify`
//! whose root is `None`, which the server is supposed to interpret as
//! deltas from the current pointer position. On real X servers this is
//! silently treated as *absolute* coordinates instead (verified on
//! hardware: after warping to (500,500), a "relative" fake motion of
//! (30,20) landed the pointer at (30,20) — the origin corner). A
//! relative-based client therefore collapses to the top-left corner of
//! the screen and its error grows without bound, while the follower
//! keeps injecting more "relative" frames that land the same place. The
//! absolute model has no such failure: a warp is exact, a dropped frame
//! self-heals (the next placement lands the whole command), and the
//! server's pointer-gain scaling (see `GainTracker`) makes the shared
//! cursor mirror the server's cursor pixel-for-pixel regardless of
//! either machine's acceleration settings.
//!
//! ## The hot path never waits on X
//!
//! A `QueryPointer` round-trip per tick — the natural way to know where
//! the cursor is — stalls every placement by the X server's reply time
//! (a busy desktop, compositor or game delays the reply, and the
//! placement behind it waits). The hot path therefore runs on locally
//! tracked state: the commanded position *is* the placement for an
//! absolute backend, and the real position is only refreshed from
//! `QueryPointer` at the beacon cadence ([`REAL_QUERY_INTERVAL`]) —
//! plenty fresh for the server's anchoring and the pin detector, and a
//! rounding error of round-trips.
//!
//! ## Visible-desktop geometry
//!
//! All coordinates are **visible-local** pixels (see [`super::geometry`]):
//! the X root window can be far larger than the desktop the user sees
//! (a disconnected monitor leaves black virtual space), and steering or
//! reporting in root pixels would drive the cursor into invisible space
//! and lie about the true screen corners. The warp translates
//! local → root; position reads translate root → local.

use std::collections::HashSet;
use std::time::Instant;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use kvmshare_core::client::Injector;
use kvmshare_log::log_warn;
use kvmshare_protocol::message::{KeyKind, ScreenInfo};

use super::buttons;
use super::geometry::{visible_desktop, VisibleDesktop};

/// How often the real cursor position is refreshed from the X server
/// (ms). The motion loop reads the position every tick; the cached value
/// keeps that read off the wire, and the refresh cadence matches the
/// cursor-beacon cadence the client already reports at — the value the
/// server anchors on is never older than one period.
const REAL_QUERY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(8);

/// The X11 clipboard, as the client's standalone [`Clipboard`] service
/// (see the core trait docs for why the clipboard is split from the
/// injector). Wraps `arboard`, the same backend the injector used.
pub struct X11Clipboard {
    clipboard: Option<arboard::Clipboard>,
    /// Last clipboard content applied from the server; the poller skips
    /// it so remote content is never echoed back.
    last_remote: Option<(String, Vec<u8>)>,
}

impl X11Clipboard {
    pub fn new(display: Option<&str>) -> Self {
        // Remote sessions (SSH, a second X server) have no real
        // clipboard; the None arm keeps the client fully functional for
        // cursor/keyboard control there.
        let clipboard = if display.is_none() { arboard::Clipboard::new().ok() } else { None };
        Self { clipboard, last_remote: None }
    }
}

impl kvmshare_core::client::Clipboard for X11Clipboard {
    fn set(&mut self, mime: &str, data: &[u8]) {
        if mime != "text/plain" {
            log_warn!("clipboard: ignoring non-text mime {mime:?}");
            return;
        }
        if let Some(cb) = &mut self.clipboard {
            if let Ok(text) = std::str::from_utf8(data) {
                if let Err(e) = cb.set_text(text.to_owned()) {
                    log_warn!("clipboard: set failed: {e}");
                }
            }
            self.last_remote = Some((mime.to_owned(), data.to_vec()));
        }
    }
    fn get(&mut self) -> Option<(String, Vec<u8>)> {
        let text = self.clipboard.as_mut()?.get_text().ok()?;
        Some(("text/plain".into(), text.into_bytes()))
    }
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)> {
        self.last_remote.clone()
    }
}

/// Core X event codes used by XTest's fake-input requests. (x11rb does
/// not export these as constants, so they live here next to their use.)
const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;

/// The client-side injector over an X display.
pub struct X11Injector {
    conn: RustConnection,
    root: xproto::Window,
    /// The visible desktop: where it sits in the root window and how big
    /// it is. All positions are visible-local pixels; the warp and the
    /// position reads translate through this (see the module docs).
    visible: VisibleDesktop,
    /// The commanded cursor position, in visible-local pixels. Absolute
    /// placement accumulates received deltas here and warps to it — see
    /// the module docs for why relative XTest motion is unusable.
    pos: (i32, i32),
    /// The last *verified* cursor position (visible-local pixels),
    /// refreshed from `QueryPointer` at most every [`REAL_QUERY_INTERVAL`].
    /// The hot path reads this cache instead of waiting on a round-trip;
    /// the beacon and the pin detector use it, never older than one
    /// refresh period.
    real: (i32, i32),
    /// When `real` was last refreshed from the X server.
    last_query: Option<Instant>,
    /// The screen's size, for clamping the commanded position (the OS
    /// pins the visible cursor at an edge; an unclamped command would
    /// run off-screen and a reversal would have to eat the whole
    /// overshoot before the cursor moved again).
    bounds: (u32, u32),
    /// Keys (HID usages) injected as down and not yet released. `leave`
    /// releases everything still held — the server may never deliver the
    /// matching ups (the user crossed back mid-hold), and the desktop
    /// must not be left with a stuck key.
    keys_down: HashSet<u32>,
    /// Buttons injected as down and not yet released (same contract as
    /// [`X11Injector::keys_down`]).
    buttons_down: HashSet<u8>,
}

impl X11Injector {
    pub fn new(display: Option<&str>) -> Result<Self, String> {
        let (conn, screen_num) = RustConnection::connect(display).map_err(|e| format!("X11 connect: {e}"))?;
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        // The visible desktop, with the whole root as the fallback (one
        // output at the origin — the common case — is identical).
        let visible = visible_desktop(&conn, screen_num)
            .unwrap_or_else(|| VisibleDesktop::whole_root(screen.width_in_pixels as u32, screen.height_in_pixels as u32));
        let bounds = visible.size;

        if conn.extension_information(xfixes::X11_EXTENSION_NAME).map_err(|e| format!("XFixes query: {e}"))?.is_none() {
            return Err("XFixes extension not available".into());
        }
        conn.xfixes_query_version(5, 0).map_err(|e| format!("XFixes version: {e}"))?;

        // Anchor both trackers at the real position: the first command
        // is measured from where the cursor actually is, not from (0,0).
        let start = Self::query_real(&conn, root, &visible).unwrap_or((0, 0));
        Ok(Self {
            conn,
            root,
            visible,
            pos: start,
            real: start,
            last_query: Some(Instant::now()),
            bounds,
            keys_down: HashSet::new(),
            buttons_down: HashSet::new(),
        })
    }

    /// One `QueryPointer` round-trip, translated into visible-local
    /// pixels. `None` when the server does not answer (rare — the caller
    /// keeps the previous value).
    fn query_real(conn: &RustConnection, root: xproto::Window, visible: &VisibleDesktop) -> Option<(i32, i32)> {
        let reply = conn.query_pointer(root).ok()?.reply().ok()?;
        Some(visible.from_root(reply.root_x as i32, reply.root_y as i32))
    }

    /// Clamp a position into the screen bounds (degenerate zero-size
    /// bounds clamp to (0,0) instead of underflowing).
    fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        let max_x = (self.bounds.0 as i64 - 1).max(0) as i32;
        let max_y = (self.bounds.1 as i64 - 1).max(0) as i32;
        (x.clamp(0, max_x), y.clamp(0, max_y))
    }

    /// Warp the pointer to visible-local pixels `(x, y)`. A direct warp
    /// is exact and has no acceleration — right for entry points and, on
    /// this backend, the whole motion stream (see the module docs). A
    /// warp to where the cursor is *verified* to already be is skipped
    /// entirely: no X traffic at all when the command and the placement
    /// agree (the steady state while the hand rests).
    ///
    /// The skip compares against the verified real position, not the
    /// commanded one: if something moved the cursor natively (a stray
    /// hand on this machine's own mouse), the verification disagrees
    /// with the command and the warp re-places it — self-healing within
    /// one refresh period. The bookkeeping always advances to the
    /// command, so a skipped warp still leaves `pos` where the next
    /// delta accumulates from.
    fn warp(&mut self, x: i32, y: i32) {
        let (x, y) = self.clamp(x, y);
        self.pos = (x, y);
        if (x, y) == self.real {
            return;
        }
        let (rx, ry) = self.visible.to_root(x, y);
        let _ = self.conn.warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, rx as i16, ry as i16);
        let _ = self.conn.flush();
        // We placed it; the periodic refresh confirms (or corrects)
        // within one [`REAL_QUERY_INTERVAL`]. Filling it in here keeps
        // the steady-state skip from re-warping to the same spot.
        self.real = (x, y);
    }

    /// The pointer's current position in visible-local pixels, via a
    /// `QueryPointer` round-trip. Only used off the hot path (startup
    /// anchoring and the periodic real-position refresh).
    fn pointer_pos(&mut self) -> Option<(i32, i32)> {
        Self::query_real(&self.conn, self.root, &self.visible)
    }
}

impl Injector for X11Injector {
    fn screen_info(&mut self) -> ScreenInfo {
        // The visible desktop, not the root window: this is the size the
        // server lays the screen out with, and the user only ever sees
        // the visible area (see [`super::geometry`]).
        ScreenInfo { width: self.bounds.0, height: self.bounds.1, scale: 1.0 }
    }

    fn move_cursor(&mut self, x: i32, y: i32) {
        // Absolute placement: a direct warp is exact and has no
        // acceleration — right for entry points.
        self.warp(x, y);
    }

    fn move_rel(&mut self, dx: i32, dy: i32) {
        // Absolute motion: accumulate the delta and warp exactly there.
        // `XWarpPointer` bypasses the desktop's pointer transform, so
        // the shared cursor lands precisely where the server commanded —
        // no overshoot from an acceleration curve, no correction lag,
        // and a dropped placement self-heals (the next warp lands the
        // whole command). The server compensates for its own pointer
        // transform by scaling the counts it sends, so this cursor
        // mirrors the server's cursor pixel-for-pixel.
        let (nx, ny) = self.clamp(self.pos.0 + dx, self.pos.1 + dy);
        self.warp(nx, ny);
    }

    fn absolute_motion(&self) -> bool {
        true
    }

    fn cursor_position(&mut self) -> (i32, i32) {
        // Refresh the verified position at most every
        // [`REAL_QUERY_INTERVAL`]; between refreshes return the cache.
        // The hot path (the motion loop's per-tick read) therefore never
        // waits on a round-trip, while the value it hands to the beacon
        // and the pin detector is never older than one refresh period.
        let now = Instant::now();
        let due = self.last_query.is_none_or(|t| now.duration_since(t) >= REAL_QUERY_INTERVAL);
        if due {
            self.last_query = Some(now);
            if let Some((x, y)) = self.pointer_pos() {
                self.real = (x, y);
            }
        }
        self.real
    }

    fn button(&mut self, button: u8, pressed: bool) {
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A release for a button we did not press
        // is dropped (its press happened on the server's machine).
        if pressed {
            self.buttons_down.insert(button);
        } else if !self.buttons_down.remove(&button) {
            return;
        }
        let Some(x11_button) = buttons::to_x11(button) else { return };
        let ty = if pressed { BUTTON_PRESS } else { BUTTON_RELEASE };
        let _ = self.conn.xtest_fake_input(ty, x11_button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        let _ = self.conn.flush();
    }

    fn wheel(&mut self, dx: i32, dy: i32) {
        // Clamp to a sane number of notches per message.
        let notches = (dx.abs() + dy.abs()).clamp(1, 10);
        let Some(button) = buttons::wheel_to_x11(dx, dy) else { return };
        for _ in 0..notches {
            let _ = self.conn.xtest_fake_input(BUTTON_PRESS, button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
            let _ = self.conn.xtest_fake_input(BUTTON_RELEASE, button, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        }
        let _ = self.conn.flush();
    }

    fn key(&mut self, kind: KeyKind, key: u32) {
        // Canonical HID usage -> evdev -> X keycode (the standard
        // `keycode = evdev + 8` mapping). Unknown usages are dropped: a
        // wrong key would be worse than no key.
        let Some(evdev) = crate::keys::evdev_from_hid(key) else { return };
        // Track down-state so `leave` can release whatever the server
        // never sent an up for. A repeat for a key we did not press (it
        // was held across the boundary, pressed on the server) must not
        // start a press here.
        match kind {
            KeyKind::Down => {
                self.keys_down.insert(key);
            }
            KeyKind::Up => {
                self.keys_down.remove(&key);
            }
            KeyKind::Repeat => {
                if !self.keys_down.contains(&key) {
                    return;
                }
            }
        }
        let keycode = evdev + 8;
        let is_press = matches!(kind, KeyKind::Down | KeyKind::Repeat);
        let ty = if is_press { KEY_PRESS } else { KEY_RELEASE };
        let _ = self.conn.xtest_fake_input(ty, keycode as u8, x11rb::CURRENT_TIME, self.root, 0, 0, 0);
        let _ = self.conn.flush();
    }

    fn enter(&mut self) {
        // The local cursor stays visible: it *is* the shared cursor now.
        // The server hides its own while the cursor is away, so hiding
        // ours too would leave no visible cursor on either screen — the
        // "leftover cursor" fix is the server-side hide, not this.
    }

    fn leave(&mut self) {
        // Control left this machine: release every key and button we
        // injected and never saw released — the user may have crossed
        // back mid-hold, so the matching ups will never arrive.
        let keys: Vec<u32> = self.keys_down.drain().collect();
        for key in keys {
            self.key(KeyKind::Up, key);
        }
        let buttons: Vec<u8> = self.buttons_down.drain().collect();
        for button in buttons {
            self.button(button, false);
        }
    }

}


#[cfg(test)]
#[path = "injector_tests.rs"]
mod tests;