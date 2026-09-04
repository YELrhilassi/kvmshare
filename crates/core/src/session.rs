//! The session: one cursor, one layout, and the rules for moving between
//! screens.
//!
//! The session is pure logic. It owns the *virtual* cursor position and
//! answers "what should happen next?" by returning [`Action`]s. The caller
//! (the server or a test harness) executes the actions.
//!
//! ## Coordinate model
//!
//! * **Virtual coordinates** span the whole desktop layout. The cursor
//!   always has a virtual position, regardless of which screen it is on.
//! * **Local coordinates** are per-screen (`x - screen.rect.x`).
//! * On every switch the virtual cursor is **snapped to the entry point**
//!   of the destination screen, so absolute positions and `Enter` always
//!   agree — no drift, no off-by-one accumulation.
//! * While the cursor is on a remote screen, the *physical* cursor on the
//!   server machine is hidden **in place** — exactly where it crossed the
//!   shared edge — and it never moves again until control returns. Moving
//!   a hidden cursor across the desktop fires hover/enter effects in
//!   every local window it crosses (and moving it *before* hiding made it
//!   visibly dash to the screen center on every crossing), so pc elements
//!   would react while the user is working on a client. The virtual
//!   cursor is driven entirely by XI2 *raw* motion (see
//!   `kvmshare-platform`), which does not depend on the physical cursor's
//!   position at all — so a static park loses nothing.

use std::time::{Duration, Instant};

use kvmshare_protocol::message::{Message, Rect};

use crate::layout::{Direction, Layout};
use crate::Mode;

/// Something the caller must do in response to an input event.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send this message to the active client.
    Send(Message),
    /// Switch the cursor to client `to`, entering at its local coords
    /// `(x, y)`. The caller must:
    /// 1. send `Leave` to the previous active client,
    /// 2. send `Enter` + `MouseMoveAbs` to the new one,
    /// 3. hide the local cursor **in place** — it is already exactly at
    ///    the shared edge where it crossed, and moving it (even hidden)
    ///    would sweep hover/enter effects across local windows.
    SwitchTo { to: u8, x: i32, y: i32 },
    /// Switch back to the local screen, entering at its local coords.
    SwitchToLocal { x: i32, y: i32 },
    /// Nothing to do.
    Nothing,
}

/// The cursor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    /// Virtual position.
    x: i32,
    y: i32,
    mode: Mode,
}

/// How long outward deltas must keep pushing against a screen edge — with
/// no beacon correcting them — before the switch fires anyway. The normal
/// crossing is confirmed instantly by the beacon (the real cursor parked
/// at the edge); this is only the fallback for a stalled beacon stream
/// (the OS pinned the pointer at the edge, so motion events — and with
/// them beacons — stop). 150 ms is far shorter than an accidental hover
/// but long enough that a beacon lag can never fire a crossing on its
/// own.
const EDGE_PUSH_FALLBACK: Duration = Duration::from_millis(150);

/// How recent an outward push must be for a beacon that parks the real
/// cursor at the shared edge to complete the crossing immediately.
/// Crossing on the *next* delta after the park beacon adds one motion
/// frame of dead time at the exact moment a crossing should feel
/// seamless — but a beacon alone (the user merely resting at the edge)
/// must never cross, so only a push within this window counts as intent.
/// Far shorter than a hover, long enough to cover the beacon lag between
/// the last delta and the park confirmation.
const EDGE_PUSH_FRESH: Duration = Duration::from_millis(60);

/// How old a client cursor-position beacon may be and still be treated
/// as the real cursor's location. Beacons arrive every few ms while the
/// client is controlled; anything older than this means the stream
/// stalled (a wedged client, a network drop) and the virtual position
/// must take over for edge decisions.
const REMOTE_BEACON_FRESH: Duration = Duration::from_millis(120);

/// The real cursor position on a remote screen, as reported by the
/// client itself (client-local pixels + arrival time).
struct RemoteBeacon {
    at: Instant,
}

/// The switching brain.
pub struct Session {
    layout: Layout,
    cursor: Cursor,
    /// The local screen's rectangle in virtual coordinates.
    local: Rect,
    /// While local: which edge the *real* (beacon-reported) cursor is
    /// parked on, if any. A crossing fires only when outward motion
    /// follows a beacon that confirms the real cursor is at the edge.
    at_edge: Option<Direction>,
    /// While local: when outward deltas first pushed against an edge the
    /// beacon had not confirmed. If they keep pushing for
    /// [`EDGE_PUSH_FALLBACK`] with no beacon correcting them (pinned
    /// cursor, stalled beacon stream), the crossing fires — see
    /// [`Session::handle_local_motion`]. Cleared whenever a beacon shows
    /// the real cursor back inside the screen.
    edge_pushing_since: Option<Instant>,
    /// While remote: the most recent real cursor position the active
    /// client reported (its screen-local pixels) and when it arrived.
    /// The client's OS applies its own pointer acceleration to the
    /// relative motion we forward, so its real cursor is the ground
    /// truth for where the shared cursor sits — exactly the role the
    /// local capture's position beacons play on the server screen.
    remote_beacon: Option<RemoteBeacon>,
    /// While remote: which edge of the active client's screen its real
    /// cursor is pinned on, per the latest beacon. A crossing fires only
    /// when outward motion follows a beacon pinned at the shared edge.
    remote_at_edge: Option<Direction>,
    /// While remote: when outward deltas first pushed against a screen
    /// edge the beacon had not confirmed (see
    /// [`Session::handle_remote_motion`]; mirrors
    /// [`Session::edge_pushing_since`] for the local screen).
    remote_pushing_since: Option<Instant>,
    /// The last relative motion forwarded while remote, so a beacon that
    /// parks the cursor at an edge can tell whether the user is still
    /// pushing outward.
    last_remote_delta: Instant,
}

impl Session {
    pub fn new(layout: Layout, local_id: u8) -> Self {
        let local = layout.find(local_id).expect("local screen must be in layout").rect;
        let (cx, cy) = local.center();
        Self {
            cursor: Cursor { x: cx, y: cy, mode: Mode::Local },
            layout,
            local,
            at_edge: None,
            edge_pushing_since: None,
            remote_beacon: None,
            remote_at_edge: None,
            remote_pushing_since: None,
            last_remote_delta: Instant::now(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.cursor.mode
    }

    /// The current layout (read-only view).
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The layout screen id for a client that introduced itself with
    /// `name`, if the layout has a remote screen with that name.
    pub fn assign_screen_id(&self, name: &str) -> Option<u8> {
        self.layout.screens.iter().find(|s| s.id != 0 && s.name == name).map(|s| s.id)
    }

    /// A client reported a new screen shape: resize its rect in the
    /// layout (position stays). Used to keep edge math correct after
    /// resolution/scale changes.
    pub fn update_screen_info(&mut self, id: u8, info: kvmshare_protocol::message::ScreenInfo) {
        if let Some(s) = self.layout.screens.iter_mut().find(|s| s.id == id) {
            let w = (info.width as f32 / info.scale.max(0.1)) as i32;
            let h = (info.height as f32 / info.scale.max(0.1)) as i32;
            s.rect.w = w;
            s.rect.h = h;
        }
    }

    /// Adopt a new layout at runtime (config hot-reload).
    ///
    /// If the cursor was on a remote client it is brought home first
    /// (`SwitchToLocal` at the new local center); if it was already local
    /// the virtual cursor is re-anchored to the local center without
    /// moving the physical one. A layout whose local screen (id 0) is
    /// missing is rejected and leaves the session untouched.
    pub fn swap_layout(&mut self, layout: Layout) -> Vec<Action> {
        let local = match layout.find(0) {
            Some(s) => s.rect,
            None => return vec![],
        };
        let was_remote = self.cursor.mode != Mode::Local;
        let (cx, cy) = local.center();
        self.layout = layout;
        self.local = local;
        self.cursor = Cursor { x: cx, y: cy, mode: Mode::Local };
        self.at_edge = None;
        self.edge_pushing_since = None;
        self.remote_beacon = None;
        self.remote_at_edge = None;
        self.remote_pushing_since = None;
        if was_remote {
            vec![Action::SwitchToLocal { x: cx, y: cy }]
        } else {
            vec![]
        }
    }

    /// Process a *local* input event (the user's physical mouse/keyboard
    /// on the server machine). Returns everything the caller must do.
    pub fn on_local_event(&mut self, msg: Message) -> Vec<Action> {
        match msg {
            Message::MouseMoveRel { dx, dy } => self.on_local_motion(dx, dy),
            Message::MouseMoveAbs { x, y } => {
                // Position beacon from the capture (real, post-acceleration
                // pointer position in screen pixels). While on the local
                // screen, re-anchor the virtual cursor to it: raw deltas
                // are pre-acceleration, so without this the virtual and
                // visible cursors drift apart and boundaries become
                // unreliable. In Remote mode the physical cursor is
                // hidden, parked and warped, so its position is
                // meaningless — raw deltas rule there (they drive the
                // client's visible cursor 1:1 and never drift).
                if matches!(self.cursor.mode, Mode::Local) {
                    let vx = self.local.x + x;
                    let vy = self.local.y + y;
                    self.cursor.x = vx;
                    self.cursor.y = vy;
                    // The beacon is the truth about where the real cursor
                    // is. Remember whether it sits exactly on a screen
                    // edge — only then can outward motion mean "cross".
                    self.at_edge = edge_direction(&self.local, vx, vy);
                    match self.at_edge {
                        Some(dir) => {
                            // The real cursor just parked on an edge
                            // while the user is mid-push: cross now, on
                            // the park itself, instead of waiting for the
                            // next delta. A beacon alone — the user
                            // resting at the edge — never crosses.
                            let pushing = self
                                .edge_pushing_since
                                .is_some_and(|t| t.elapsed() < EDGE_PUSH_FRESH);
                            if pushing {
                                return self.switch_out(dir);
                            }
                        }
                        None => {
                            // The real cursor is back inside: any edge-
                            // push attempt was a transient overshoot, and
                            // it is over.
                            self.edge_pushing_since = None;
                        }
                    }
                }
                vec![]
            }
            Message::MouseButton { button, pressed } => {
                self.forward_while_remote(Message::MouseButton { button, pressed })
            }
            Message::MouseWheel { dx, dy } => self.forward_while_remote(Message::MouseWheel { dx, dy }),
            Message::Key { kind, key } => self.forward_while_remote(Message::Key { kind, key }),
            // The user pressed the escape key (Scroll Lock) while the
            // cursor was on a client: bring control home, no matter what
            // the client is doing. This is the universal "unstick" — it
            // works even when the client's machine cannot inject input
            // (an elevated window, a wedged session, a dead client).
            Message::Escape => vec![self.force_local()],
            _ => vec![],
        }
    }

    /// The user asked (via the escape key, or because the active client
    /// reported blocked input) to return control home right now,
    /// regardless of where the virtual cursor is. Reuses the home-entry
    /// logic so the session, client Leave and engine state all stay
    /// consistent.
    pub fn force_local(&mut self) -> Action {
        let (x, y) = self.local.center();
        self.enter_screen(0, x, y);
        Action::SwitchToLocal { x, y }
    }

    fn forward_while_remote(&mut self, msg: Message) -> Vec<Action> {
        if matches!(self.cursor.mode, Mode::Remote(_)) {
            vec![Action::Send(msg)]
        } else {
            vec![]
        }
    }

    /// Relative motion of the physical cursor while the virtual cursor is
    /// anywhere. Returns the actions that keep the virtual cursor on the
    /// right screen.
    fn on_local_motion(&mut self, dx: i32, dy: i32) -> Vec<Action> {
        // Track the virtual position regardless of mode.
        self.cursor.x += dx;
        self.cursor.y += dy;

        match self.cursor.mode {
            Mode::Local => self.handle_local_motion(),
            Mode::Remote(id) => self.handle_remote_motion(id, dx, dy),
        }
    }

    /// A client reported where its *real* cursor is (client-local
    /// pixels). Runs from the client's connection thread while this
    /// client is the active one; the session mutex serializes it with the
    /// main loop's local-input processing.
    ///
    /// Only *state* is updated here — the actual crossing is fired by the
    /// next outward delta on the main loop (≤ a motion frame later), so
    /// no action needs executing from this thread. The state mirrors the
    /// local-screen beacon: which edge the real cursor is pinned on, and
    /// whether it has come back inside (which cancels any edge-push
    /// attempt — an interior beacon means the deltas were an overshoot).
    pub fn on_remote_beacon(&mut self, id: u8, x: i32, y: i32) -> Vec<Action> {
        if !matches!(self.cursor.mode, Mode::Remote(cur) if cur == id) {
            return vec![];
        }
        let Some(screen) = self.layout.find(id) else { return vec![] };
        self.remote_beacon = Some(RemoteBeacon { at: Instant::now() });
        let vx = screen.rect.x + x;
        let vy = screen.rect.y + y;
        self.remote_at_edge = edge_direction(&screen.rect, vx, vy);
        if self.remote_at_edge.is_none() {
            self.remote_pushing_since = None;
        }
        vec![]
    }

    /// Cursor is on the local screen and moving. If it leaves toward a
    /// neighbor, switch.
    fn handle_local_motion(&mut self) -> Vec<Action> {
        let dir = match self.layout.exit_direction(0, self.cursor.x, self.cursor.y) {
            Some(d) => d,
            None => {
                // Still inside the local screen: any earlier edge-push
                // attempt was a transient overshoot and is over.
                self.edge_pushing_since = None;
                return vec![];
            }
        };
        // The virtual cursor left the local rect through `dir`. Raw
        // deltas run *ahead* of the real cursor (they are
        // pre-acceleration and the beacon that corrects them can lag
        // under load), so switching on deltas alone is what made the
        // cursor jump to a neighbor while merely approaching its edge.
        // Cross only when the real cursor is confirmed parked on this
        // edge (a beacon put it there and outward motion follows) — or,
        // as a fallback for a beacon stream that stalls on a pinned
        // cursor, after sustained outward pushing.
        let local = self.local;
        self.clamp_to(&local);
        let confirmed = self.at_edge == Some(dir);
        let sustained = self.edge_pushing_since.is_some_and(|t| t.elapsed() >= EDGE_PUSH_FALLBACK);
        if !confirmed && !sustained {
            self.edge_pushing_since.get_or_insert(Instant::now());
            return vec![];
        }
        self.switch_out(dir)
    }

    /// Leave the local screen through `dir`: switch to the neighbor in
    /// that direction. Resets the edge/push state and snaps the virtual
    /// cursor to the neighbor's entry point. Returns nothing on a dead
    /// edge (the cursor stays clamped).
    fn switch_out(&mut self, dir: Direction) -> Vec<Action> {
        match self.layout.neighbor(0, dir, self.cursor.x, self.cursor.y) {
            Some((id, x, y)) => {
                self.at_edge = None;
                self.edge_pushing_since = None;
                self.enter_screen(id, x, y);
                vec![Action::SwitchTo { to: id, x, y }]
            }
            None => vec![], // dead edge: stay clamped
        }
    }

    /// Cursor is on a remote screen and the physical mouse keeps moving.
    ///
    /// Motion is forwarded *relative*: the client's OS applies its own
    /// pointer acceleration, so the shared cursor feels exactly like a
    /// physical mouse on that machine — this is what makes fast movement
    /// match hand speed (raw pre-acceleration counts forwarded as
    /// absolute positions made the client cursor crawl at speed). The
    /// client reports its real cursor position back as beacons
    /// ([`Session::on_remote_beacon`]) and those — not the raw virtual
    /// position — drive edge crossings, because after acceleration the
    /// raw deltas no longer equal real travel. Without fresh beacons
    /// (stalled stream, old client) the virtual position is the
    /// fallback. The hidden local cursor never moves while we are away.
    fn handle_remote_motion(&mut self, id: u8, dx: i32, dy: i32) -> Vec<Action> {
        self.last_remote_delta = Instant::now();
        let mut actions = Vec::with_capacity(2);
        // Forward the raw motion so the client's pointer transform turns
        // it into real travel.
        actions.push(Action::Send(Message::MouseMoveRel { dx, dy }));

        let rect = match self.layout.find(id) {
            Some(s) => s.rect,
            None => return actions, // layout changed under us
        };
        let (local_x, local_y) = self.cursor.local_pos(&rect);
        if local_x >= 0 && local_x < rect.w && local_y >= 0 && local_y < rect.h {
            return actions; // still inside
        }

        // The virtual cursor left this screen's rect. Which way?
        let dir = match self.layout.exit_direction(id, self.cursor.x, self.cursor.y) {
            Some(d) => d,
            None => {
                self.clamp_to(&rect);
                return actions;
            }
        };
        self.clamp_to(&rect);

        let beacon_fresh = self
            .remote_beacon
            .as_ref()
            .is_some_and(|b| b.at.elapsed() < REMOTE_BEACON_FRESH);
        let pinned_here = self.remote_at_edge == Some(dir);
        // While the beacon stream is fresh the *real* cursor is the
        // authority: cross only when it is genuinely pinned on this edge
        // (the client's OS parked it there) and the user keeps pushing.
        // Deltas alone must not cross — after acceleration they run far
        // ahead of the real cursor (the same trap as the local screen).
        let confirmed = beacon_fresh && pinned_here;
        // Fallback for a stalled beacon stream: sustained outward
        // pushing on a virtual-exit eventually crosses (the OS has the
        // real cursor pinned against the wall, so raw deltas keep
        // flowing only while the user pushes).
        let sustained = !beacon_fresh
            && self
                .remote_pushing_since
                .is_some_and(|t| t.elapsed() >= EDGE_PUSH_FALLBACK);
        if !confirmed && !sustained {
            self.remote_pushing_since.get_or_insert(Instant::now());
            return actions;
        }
        self.cross_from_remote(id, dir)
    }

    /// Switch away from the remote screen `id` through `dir` (back home
    /// or on to another client). Resets the remote beacon/push state and
    /// snaps the virtual cursor to the destination's entry point.
    fn cross_from_remote(&mut self, id: u8, dir: Direction) -> Vec<Action> {
        self.remote_beacon = None;
        self.remote_at_edge = None;
        self.remote_pushing_since = None;
        match self.layout.neighbor(id, dir, self.cursor.x, self.cursor.y) {
            Some((next, x, y)) => {
                self.enter_screen(next, x, y);
                if next == 0 {
                    vec![Action::SwitchToLocal { x, y }]
                } else {
                    vec![Action::SwitchTo { to: next, x, y }]
                }
            }
            None => vec![], // outer edge of the desktop: stay
        }
    }

    /// Snap the virtual cursor to the entry point of screen `id` (local
    /// coords `x, y`) and set the mode.
    fn enter_screen(&mut self, id: u8, x: i32, y: i32) {
        let s = self.layout.find(id).expect("entry screen must exist");
        self.cursor.x = s.rect.x + x;
        self.cursor.y = s.rect.y + y;
        self.cursor.mode = if id == 0 { Mode::Local } else { Mode::Remote(id) };
        self.at_edge = None;
        self.edge_pushing_since = None;
        self.remote_beacon = None;
        self.remote_at_edge = None;
        self.remote_pushing_since = None;
    }

    /// Clamp the virtual cursor inside `rect` (in virtual coords).
    fn clamp_to(&mut self, rect: &Rect) {
        self.cursor.x = self.cursor.x.clamp(rect.left(), rect.right() - 1);
        self.cursor.y = self.cursor.y.clamp(rect.top(), rect.bottom() - 1);
    }

    /// A client with the given id disconnected while active: drop back to
    /// the local screen.
    pub fn on_client_disconnected(&mut self, id: u8) -> Action {
        if self.cursor.mode == Mode::Remote(id) {
            self.force_local()
        } else {
            Action::Nothing
        }
    }

    #[cfg(test)]
    pub fn cursor_pos(&self) -> (i32, i32) {
        (self.cursor.x, self.cursor.y)
    }
}

impl Cursor {
    /// Convert a virtual position to `rect`'s local coordinates.
    fn local_pos(&self, rect: &Rect) -> (i32, i32) {
        (self.x - rect.x, self.y - rect.y)
    }
}

/// Which screen edge a position sits exactly on, if any. The OS pins the
/// pointer at the outer pixel column/row (`left`/`right - 1`), so those
/// are the edges. Precedence mirrors [`Layout::exit_direction`]
/// (left, right, top, bottom) so a corner is treated consistently.
fn edge_direction(rect: &Rect, x: i32, y: i32) -> Option<Direction> {
    if x <= rect.left() {
        Some(Direction::Left)
    } else if x >= rect.right() - 1 {
        Some(Direction::Right)
    } else if y <= rect.top() {
        Some(Direction::Top)
    } else if y >= rect.bottom() - 1 {
        Some(Direction::Bottom)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
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
    /// really happens: the beacon parks the real cursor at the shared
    /// edge, then an outward push confirms the intent.
    fn cross_to_hp(s: &mut Session) {
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
    }

    #[test]
    fn assign_screen_id_matches_by_name() {
        let s = two_screens();
        assert_eq!(s.assign_screen_id("hp"), Some(1));
        assert_eq!(s.assign_screen_id("pc"), None); // the server's own screen
        assert_eq!(s.assign_screen_id("nope"), None);
    }

    #[test]
    fn update_screen_info_resizes_rect() {
        let mut s = two_screens();
        s.update_screen_info(1, kvmshare_protocol::message::ScreenInfo { width: 2560, height: 1440, scale: 1.0 });
        let hp = s.layout().find(1).unwrap();
        assert_eq!((hp.rect.w, hp.rect.h), (2560, 1440));
        // Position is untouched.
        assert_eq!(hp.rect.x, -1920);
    }

    #[test]
    fn local_motion_inside_does_nothing() {
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 10 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn crossing_left_edge_switches_to_hp() {
        let mut s = two_screens();
        // The real cursor parks at the left edge (beacon), then an
        // outward push crosses.
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y }] => {
                assert_eq!(*to, 1);
                assert_eq!(*x, 1919); // hp's right edge, local coords
                assert_eq!(*y, 540);
            }
            other => panic!("expected SwitchTo, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
        // Virtual position was snapped to hp's entry point (-1, 540).
        assert_eq!(s.cursor_pos(), (-1, 540));
    }

    #[test]
    fn remote_motion_forwards_relative_deltas() {
        // Motion on a client is forwarded *relative* — the client's OS
        // applies its own pointer acceleration, which is what makes the
        // shared cursor feel native (raw deltas replayed as absolute
        // positions made it crawl at speed).
        let mut s = two_screens();
        cross_to_hp(&mut s); // switch to hp
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -10, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn crossing_back_returns_to_local() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // to hp, virtual (-1, 540) = hp's right edge
        // The client's real cursor sits where control placed it: pinned
        // on hp's right edge (the shared edge with pc).
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        // A push right while the real cursor is pinned on that edge
        // crosses home.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 0, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn crossing_home_requires_the_real_cursor_at_the_edge() {
        // Deltas alone must never cross back home: after acceleration the
        // raw deltas run far ahead of the client's real cursor. While the
        // real cursor is still interior, an outward overshoot only
        // clamps.
        let mut s = two_screens();
        cross_to_hp(&mut s);
        assert_eq!(s.on_remote_beacon(1, 900, 540), vec![]); // real cursor mid-screen
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 5, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "overshoot while interior must not cross");
        // Only once the client reports its real cursor pinned on the
        // shared edge does a push cross.
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 0, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn buttons_forward_only_when_remote() {
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseButton { button: 0, pressed: true }), vec![]);
        cross_to_hp(&mut s);
        let actions = s.on_local_event(Message::MouseButton { button: 0, pressed: true });
        assert_eq!(actions, vec![Action::Send(Message::MouseButton { button: 0, pressed: true })]);
    }

    #[test]
    fn outer_edge_clamps() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // on hp, virtual (-1,540)
        // Push far left past hp's left edge (an outer edge of the
        // desktop — no neighbor there). The motion is forwarded but the
        // virtual cursor clamps and nothing crosses.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -3000, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -3000, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1));
        // Even with the real cursor pinned on that outer edge and a
        // fresh push, there is nowhere to go: stay on hp.
        assert_eq!(s.on_remote_beacon(1, 0, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn escape_returns_home_even_when_remote() {
        let mut s = two_screens();
        // Escape while local: re-anchors to the local center (the
        // capture only emits Escape while remote, but it must not corrupt
        // state if it fires locally).
        let actions = s.on_local_event(Message::Escape);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 960, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);

        // The real case: stuck on a client (even one that stopped
        // responding) — the escape key brings control home regardless.
        cross_to_hp(&mut s);
        assert_eq!(s.mode(), Mode::Remote(1));
        let actions = s.on_local_event(Message::Escape);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 960, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
        assert_eq!(s.cursor_pos(), (960, 540));
    }

    #[test]
    fn disconnect_returns_home() {
        let mut s = two_screens();
        cross_to_hp(&mut s);
        assert_eq!(s.on_client_disconnected(1), Action::SwitchToLocal { x: 960, y: 540 });
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn key_events_forward_when_remote() {
        let mut s = two_screens();
        cross_to_hp(&mut s);
        let actions = s.on_local_event(Message::Key { kind: KeyKind::Down, key: 0x14 });
        assert_eq!(actions, vec![Action::Send(Message::Key { kind: KeyKind::Down, key: 0x14 })]);
    }

    #[test]
    fn remote_to_remote_switch_keeps_park() {
        let layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
            Screen { id: 2, name: "mac".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
        ]);
        let mut s = Session::new(layout, 0);
        // pc -> hp
        cross_to_hp(&mut s);
        // Swoop across hp to its left edge (raw deltas overshoot the
        // rect; no beacon has confirmed the real cursor there yet).
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -2000, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "overshoot alone must not switch");
        // The client reports its real cursor pinned on hp's left edge,
        // then the push continues: switch on to mac.
        assert_eq!(s.on_remote_beacon(1, 0, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y }] => {
                assert_eq!(*to, 2);
                assert_eq!(*x, 1919); // mac's right edge
                assert_eq!(*y, 540);
            }
            other => panic!("expected [SwitchTo], got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(2));
    }

    #[test]
    fn remote_roam_only_forwards_relative_deltas() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // on hp
        // Roam around hp. The session must only ever emit relative
        // motion for the client — never an absolute position (the
        // hidden local cursor never moves while we are away, so no warp
        // or recenter can sweep hover/enter effects across local
        // windows).
        for dx in [-1000, -900, 1900, -1000] {
            let actions = s.on_local_event(Message::MouseMoveRel { dx, dy: 0 });
            for a in &actions {
                assert!(
                    matches!(a, Action::Send(Message::MouseMoveRel { .. })),
                    "remote motion must only forward relative deltas, got {a:?}"
                );
            }
        }
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn swap_layout_while_local_stays_put() {
        let mut s = two_screens();
        assert_eq!(s.mode(), Mode::Local);

        // Same geometry, only names change: nothing to do, cursor stays.
        let new_layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
        ]);
        let actions = s.swap_layout(new_layout);
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Local);
        assert_eq!(s.layout().screens[1].rect.x, -3840);
    }

    #[test]
    fn swap_layout_while_remote_comes_home() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // now on hp
        assert_eq!(s.mode(), Mode::Remote(1));

        // New layout without hp at all (and a different local size).
        let new_layout = Layout::new(vec![Screen {
            id: 0,
            name: "pc".into(),
            rect: Rect { x: 0, y: 0, w: 2560, h: 1440 },
        }]);
        let actions = s.swap_layout(new_layout);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 1280, y: 720 }]);
        assert_eq!(s.mode(), Mode::Local);
        assert_eq!(s.cursor_pos(), (1280, 720));
    }

    #[test]
    fn beacon_resyncs_the_virtual_cursor_to_the_real_position() {
        let mut s = two_screens();
        // The virtual cursor drifted far from the real one (say the
        // server started while the mouse sat near the right edge): a
        // beacon snaps it to the real position, and motion then moves
        // from there.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 1500, y: 540 }), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 0 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.cursor_pos(), (1510, 540)); // 1500 (real) + delta
        // Leftward motion from mid-screen stays local too.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -700, dy: 0 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.cursor_pos(), (810, 540));
    }

    #[test]
    fn beacon_crosses_when_pinned_at_the_wall() {
        let mut s = two_screens();
        // The OS clamps the real pointer at the left screen edge: the
        // last beacon says x=0, and the raw deltas of the continued
        // outward push arrive anyway, so the virtual cursor crosses the
        // boundary — the switch fires exactly when the cursor is visibly
        // jammed against the edge.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y, .. }] => {
                assert_eq!(*to, 1);
                assert_eq!(*x, 1919); // hp's right edge
                assert_eq!(*y, 540);
            }
            other => panic!("expected SwitchTo, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn deltas_alone_never_jump_to_a_neighbor() {
        // The bug this guards against: raw deltas run ahead of the real
        // cursor (they are pre-acceleration and the beacon can lag under
        // load), so a fast approach near the edge used to overshoot the
        // boundary and "jump" to the client without intent. Deltas alone
        // — even far past the edge — must never switch while the real
        // cursor is still inside.
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local, "overshoot alone must not switch");
        // The virtual cursor was clamped at the edge; a beacon showing
        // the real cursor back inside confirms it was an overshoot.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 300, y: 540 }), vec![]);
        assert_eq!(s.cursor_pos(), (300, 540));
        // Continuing from there stays local.
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn beacon_park_crosses_immediately_during_a_fresh_push() {
        let mut s = two_screens();
        // A fast approach overshoots the boundary via raw deltas (no
        // beacon has confirmed anything yet — the cursor may still be
        // mid-screen).
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
        // The beacon then reports the real cursor parked on the shared
        // edge while the user is still pushing: the crossing must fire
        // on the park itself — not on a later delta — so a fast crossing
        // has no dead frame at the boundary.
        let actions = s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y, .. }] => {
                assert_eq!(*to, 1);
                assert_eq!(*x, 1919); // hp's right edge, local coords
                assert_eq!(*y, 540);
            }
            other => panic!("expected immediate SwitchTo on park-during-push, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn resting_at_the_edge_never_crosses_without_a_fresh_push() {
        let mut s = two_screens();
        // A flick ends with the cursor pushed against the left edge, then
        // the user stops: the outward deltas are no longer fresh.
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
        // A beacon shows the real cursor parked on the edge, but nothing
        // has pushed outward recently: resting there must not cross.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
        // A fresh push while parked crosses immediately (confirmed by the
        // beacon).
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, .. }] => assert_eq!(*to, 1),
            other => panic!("expected SwitchTo after a fresh push at the parked edge, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn sustained_push_crosses_when_the_beacon_stream_stalls() {
        // A stalled beacon stream: the OS has pinned the pointer at the
        // edge, motion events (and with them beacons) stop, and only raw
        // deltas keep flowing. Without a beacon the first push cannot be
        // confirmed — but sustained pushing must still cross, or the
        // cursor would stick at the edge forever.
        let mut s = two_screens();
        // The push reaches the edge and stays there (unconfirmed, no
        // switch yet).
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local, "first unconfirmed push must not switch");
        // Wait out the fallback window, then keep pushing.
        std::thread::sleep(EDGE_PUSH_FALLBACK + Duration::from_millis(20));
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        match actions.as_slice() {
            [Action::SwitchTo { to, .. }] => assert_eq!(*to, 1),
            other => panic!("expected SwitchTo after sustained push, got {other:?}"),
        }
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn local_abs_beacon_is_ignored_while_remote() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // on hp, virtual (-1, 540)
        // A *local* capture beacon while remote is the hidden parked
        // cursor (meaningless): it must not resync the virtual position.
        let actions = s.on_local_event(Message::MouseMoveAbs { x: 50, y: 60 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.cursor_pos(), (-1, 540)); // untouched
        // Crossing home is driven by the client's own beacon, not the
        // local one.
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 0, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn remote_beacon_from_wrong_client_is_ignored() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // active client is 1
        assert_eq!(s.on_remote_beacon(2, 0, 0), vec![]); // not the active one
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn sustained_remote_push_crosses_when_beacons_stall() {
        // A client whose beacon stream stalls (wedged, network drop):
        // outward pushing must still bring control home after the
        // fallback window, or the cursor would be stuck on the client
        // forever.
        let mut s = two_screens();
        cross_to_hp(&mut s); // virtual (-1, 540), hp's right edge
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 }), vec![Action::Send(Message::MouseMoveRel { dx: 1, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "no beacon yet: one push must not cross");
        std::thread::sleep(REMOTE_BEACON_FRESH + EDGE_PUSH_FALLBACK + Duration::from_millis(20));
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 0, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn swap_layout_rejects_missing_local_screen() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // on hp
        let bad = Layout::new(vec![Screen {
            id: 1,
            name: "hp".into(),
            rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 },
        }]);
        let actions = s.swap_layout(bad);
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Remote(1), "bad layout must not disturb the session");
        assert_eq!(s.layout().screens.len(), 2);
    }
}