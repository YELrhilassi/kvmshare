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
//!   cursor is driven entirely by *raw* motion and real-position beacons
//!   (see `kvmshare-platform`), which do not depend on the physical
//!   cursor's position at all — so a static park loses nothing.
//!
//! ## The boundary model
//!
//! A screen edge is a **wall**: the OS pins a real cursor there and no
//! amount of outward motion moves it off-screen. Two cursor streams feed
//! the session:
//!
//! * **Raw deltas** — what the device reported. They are instantaneous
//!   but *pre-acceleration*: they run ahead of the visible cursor, so
//!   they can never decide a crossing alone (deltas alone are exactly
//!   what made the cursor jump to a neighbor while merely approaching an
//!   edge in earlier designs).
//! * **Real-position beacons** — where the *visible* cursor actually is
//!   (the server's own screen on the local side, the active client's
//!   reports on the remote side). They lag a few milliseconds but are the
//!   ground truth.
//!
//! A crossing therefore needs **both, together**:
//!
//! 1. **Arm** — a beacon places the real cursor within [`EDGE_BAND`] px
//!    of a screen edge ("at the wall"). The OS has *committed* the cursor
//!    to that boundary; the user is standing on the seam.
//! 2. **Fire** — an outward push (raw deltas toward that edge) while
//!    armed. At the wall, outward deltas are unambiguous intent: the OS
//!    cannot move the cursor any further, so the push can only mean
//!    "cross".
//!
//! Because arming comes from the *real* cursor and firing from a *push*,
//! the rule is symmetric in both directions and needs no fragile
//! point-exact edge math:
//!
//! * resting at the wall never crosses (no push);
//! * an interior cursor never crosses, no matter how fast the deltas run;
//! * a sweep that ends exactly at the wall still crosses: when a beacon
//!   parks the cursor on a wall while a push arrived within
//!   [`EDGE_PUSH_FRESH`], the crossing fires **on the park itself** —
//!   no dead frame at the boundary, no "I have to push again" stickiness;
//! * moving *away* from a wall disarms it, so the entry placement on the
//!   neighbor (which sits on the seam) can never bounce control back.
//!
//! The one fallback left is for a **stalled beacon stream** (a platform
//! whose position events stop while the pointer is pinned, a wedged
//! client, an old peer without beacon support): sustained outward pushing
//! past [`EDGE_PUSH_FALLBACK`] — with the *virtual* cursor (raw deltas)
//! actually outside the screen rect, so an interior real cursor can never
//! trip it — crosses anyway. It is the rescue path, never the common one.

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

/// Wall-band width (px). A real cursor within [`EDGE_BAND`] px of a
/// screen edge counts as "at the wall". Two pixels absorb pointer
/// position quantization (a pinned cursor reports the very last pixel on
/// both X11 and Windows; one pixel of slack covers off-by-ones) without
/// making the band wide enough to steal aim from UI elements near the
/// edge. The band is *not* what makes a crossing fire — the push is — it
/// only makes the "at the wall" test robust.
const EDGE_BAND: i32 = 2;

/// How recent an outward push must be for a beacon that *parks* the real
/// cursor on a wall to complete the crossing on the park itself. Crossing
/// on the next delta would otherwise add one motion frame of dead time at
/// the exact moment a crossing should feel seamless — and a sweep that
/// ends precisely at the wall (the flick-and-stop case) would not cross
/// until the user pushed again, which feels sticky. A beacon alone —
/// the user resting at the edge — never crosses: the push within this
/// window is what marks intent. Far shorter than a hover, long enough to
/// cover the beacon lag between the last delta and the park confirmation
/// even under load.
const EDGE_PUSH_FRESH: Duration = Duration::from_millis(40);

/// How long outward deltas must keep pushing against a screen edge —
/// with the *virtual* cursor outside the rect and no beacon correcting
/// it — before the switch fires anyway. This is the rescue path for a
/// stalled beacon stream (the OS pinned the pointer at the edge, motion
/// events — and with them beacons — stop, and only raw deltas keep
/// flowing; or a peer that cannot report its real cursor at all). The
/// virtual cursor must actually be *outside* the rect, so an interior
/// real cursor can never trip it. Far shorter than an accidental hover,
/// long enough that a beacon lag can never fire a crossing on its own.
const EDGE_PUSH_FALLBACK: Duration = Duration::from_millis(150);

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
const ENTRY_INSET: i32 = 48;

/// How old a client cursor-position beacon may be and still be treated
/// as the real cursor's location. Beacons arrive every few ms while the
/// client is controlled; anything older than this means the stream
/// stalled (a wedged client, a network drop) and the wall-arm expires —
/// outward pushes then need the sustained fallback instead, so a stale
/// "at the wall" report can never fire a crossing the user did not push
/// for.
const REMOTE_BEACON_FRESH: Duration = Duration::from_millis(120);

// Direction bits for the "which walls is the real cursor on" mask. A
// corner can set two bits at once; the push direction picks which one
// fires.
const BIT_LEFT: u8 = 1 << 0;
const BIT_RIGHT: u8 = 1 << 1;
const BIT_TOP: u8 = 1 << 2;
const BIT_BOTTOM: u8 = 1 << 3;

fn bit(dir: Direction) -> u8 {
    match dir {
        Direction::Left => BIT_LEFT,
        Direction::Right => BIT_RIGHT,
        Direction::Top => BIT_TOP,
        Direction::Bottom => BIT_BOTTOM,
    }
}

/// The four directions, in a stable order (left/right before top/bottom,
/// mirroring [`Layout::exit_direction`] so corners resolve consistently).
const DIRS: [Direction; 4] = [Direction::Left, Direction::Right, Direction::Top, Direction::Bottom];

/// Does this delta push *outward* through `dir` (toward that edge)?
fn pushes_outward(dir: Direction, dx: i32, dy: i32) -> bool {
    match dir {
        Direction::Left => dx < 0,
        Direction::Right => dx > 0,
        Direction::Top => dy < 0,
        Direction::Bottom => dy > 0,
    }
}

/// Does this delta move *away* from `dir`'s wall (back into the screen)?
fn pulls_inward(dir: Direction, dx: i32, dy: i32) -> bool {
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
fn wall_bits(rect: &Rect, x: i32, y: i32) -> u8 {
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
struct Push {
    dir: Direction,
    at: Instant,
}

/// A stalled-stream fallback in progress: which edge the virtual cursor
/// is pushed out through and when that started.
#[derive(Debug, Clone, Copy)]
struct Pushing {
    dir: Direction,
    since: Instant,
}

/// The switching brain.
pub struct Session {
    layout: Layout,
    cursor: Cursor,
    /// The local screen's rectangle in virtual coordinates.
    local: Rect,

    // Local-mode boundary state.
    //
    // `at_wall` mirrors what the *real* (beacon-reported) cursor is doing
    // on the local screen. A crossing fires when outward motion follows a
    // beacon that put the cursor on a wall — never on motion alone and
    // never on a beacon alone.
    at_wall: u8,
    /// The most recent outward push through each shared edge (used to
    /// fire on a park beacon mid-sweep).
    last_out: Option<Push>,
    /// A stalled-stream fallback in progress on the local screen.
    pushing: Option<Pushing>,

    // Remote-mode boundary state (mirrors the local side, fed by the
    // active client's real-cursor beacons).
    remote_at_wall: u8,
    /// When the last remote beacon arrived (`None` = none yet).
    remote_beacon_at: Option<Instant>,
    remote_last_out: Option<Push>,
    remote_pushing: Option<Pushing>,

    /// The server's measured pointer gain (pixels of real cursor travel
    /// per raw device count), applied to forwarded remote motion so the
    /// client's cursor travels exactly like the server's own would.
    /// Updated by the server loop from its [`GainTracker`]; defaults to
    /// 1.0 (counts map 1:1) before the first measurement.
    gain: f64,
    /// Fractional carry of the gain-scaled motion (sub-pixel remainders
    /// accumulate and flush as whole pixels, so a 0.5 px/count gain never
    /// truncates slow motion away and never biases it upward).
    gain_rem: (f64, f64),
}

impl Session {
    pub fn new(layout: Layout, local_id: u8) -> Self {
        let local = layout.find(local_id).expect("local screen must be in layout").rect;
        let (cx, cy) = local.center();
        Self {
            cursor: Cursor { x: cx, y: cy, mode: Mode::Local },
            layout,
            local,
            at_wall: 0,
            last_out: None,
            pushing: None,
            remote_at_wall: 0,
            remote_beacon_at: None,
            remote_last_out: None,
            remote_pushing: None,
            gain: 1.0,
            gain_rem: (0.0, 0.0),
        }
    }

    /// Update the pointer-gain estimate (see the field docs). Called by
    /// the server loop whenever a local motion window closes; affects
    /// only future *remote* motion.
    pub fn set_gain(&mut self, gain: f64) {
        self.gain = gain.clamp(0.25, 3.0);
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
        self.clear_boundary_state();
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
                // meaningless — raw deltas and the client's own beacons
                // rule there.
                if matches!(self.cursor.mode, Mode::Local) {
                    self.on_local_beacon(x, y)
                } else {
                    vec![]
                }
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
        // While the cursor is on a client, raw device counts are scaled
        // by the server's measured pointer gain ([`Self::set_gain`]) so
        // the virtual cursor — and the motion forwarded to the client —
        // moves exactly like the server's own visible cursor would for
        // the same hand motion. Local motion stays raw: the server's
        // real-position beacons re-anchor the virtual cursor anyway.
        let (sx, sy) = match self.cursor.mode {
            Mode::Remote(_) => {
                let g = self.gain;
                // Scaled with a fractional carry: truncation toward zero
                // (like the capture's PendingMotion) keeps slow motion
                // symmetric in both directions instead of rounding every
                // half-pixel frame up (a 0.5 gain would otherwise turn
                // 1-count frames into 1 px each — a 2x bias).
                let rx = dx as f64 * g + self.gain_rem.0;
                let ry = dy as f64 * g + self.gain_rem.1;
                let sx = rx.trunc() as i32;
                let sy = ry.trunc() as i32;
                self.gain_rem = (rx - sx as f64, ry - sy as f64);
                (sx, sy)
            }
            Mode::Local => (dx, dy),
        };

        // Track the virtual position regardless of mode.
        self.cursor.x += sx;
        self.cursor.y += sy;

        match self.cursor.mode {
            Mode::Local => self.handle_local_motion(dx, dy),
            Mode::Remote(id) => self.handle_remote_motion(id, sx, sy),
        }
    }

    /// A beacon reporting the *real* cursor position on the local screen
    /// (capture pixels). Re-anchors the virtual cursor and updates the
    /// wall-arm state; may fire a crossing when the cursor parks on a
    /// wall mid-sweep.
    fn on_local_beacon(&mut self, x: i32, y: i32) -> Vec<Action> {
        // The beacon is the truth about where the visible cursor is.
        self.cursor.x = self.local.x + x;
        self.cursor.y = self.local.y + y;
        let bits = wall_bits(&self.local, x, y);
        let newly = bits & !self.at_wall;
        self.at_wall = bits;
        if bits == 0 {
            // The real cursor is back inside: any wall-arm or push
            // attempt was a transient overshoot, and it is over.
            self.pushing = None;
            return vec![];
        }
        // A beacon that parks the cursor on a wall the user is pushing
        // against completes the crossing on the park itself — a fast
        // sweep has no dead frame at the boundary, and a sweep that ends
        // exactly at the wall still crosses. A beacon alone (resting at
        // the edge) only arms: the next outward push fires.
        for dir in DIRS {
            if newly & bit(dir) != 0
                && self
                    .last_out
                    .is_some_and(|p| p.dir == dir && p.at.elapsed() < EDGE_PUSH_FRESH)
            {
                let actions = self.switch_out(dir);
                if !actions.is_empty() {
                    return actions;
                }
                // Dead edge (no neighbor there): fall through, stay armed
                // for the other directions if any.
            }
        }
        vec![]
    }

    /// Cursor is on the local screen and moving.
    fn handle_local_motion(&mut self, dx: i32, dy: i32) -> Vec<Action> {
        let now = Instant::now();

        // Beacon-confirmed crossings. A delta pushing outward through a
        // wall the *real* cursor sits on (a beacon armed it) IS the
        // crossing intent — fire immediately: the OS has already pinned
        // the cursor at the boundary, so the push cannot mean anything
        // else. A delta moving away from a wall disarms it — the user is
        // leaving the edge, and the next push must re-arm from a beacon
        // (this is what makes the seam placement on entry never bounce).
        for dir in DIRS {
            if pushes_outward(dir, dx, dy) {
                self.last_out = Some(Push { dir, at: now });
                if self.at_wall & bit(dir) != 0 {
                    let actions = self.switch_out(dir);
                    if !actions.is_empty() {
                        return actions;
                    }
                    // Dead edge: stop trying to cross here.
                    self.at_wall &= !bit(dir);
                }
            } else if pulls_inward(dir, dx, dy) {
                self.at_wall &= !bit(dir);
            }
        }

        // Fallback for a stalled beacon stream (see the module docs). The
        // virtual cursor must have actually crossed the rect — raw deltas
        // ran past the edge — so an interior real cursor can never trip
        // it, and the outward pushing must be sustained.
        let dir = match self.layout.exit_direction(0, self.cursor.x, self.cursor.y) {
            Some(d) => d,
            None => {
                self.pushing = None;
                return vec![];
            }
        };
        let local = self.local;
        self.clamp_to(&local);
        let sustained = self.pushing.is_some_and(|p| p.dir == dir && p.since.elapsed() >= EDGE_PUSH_FALLBACK);
        if !sustained {
            self.pushing = match self.pushing {
                Some(p) if p.dir == dir => self.pushing,
                _ => Some(Pushing { dir, since: now }),
            };
            return vec![];
        }
        self.pushing = None;
        self.switch_out(dir)
    }

    /// Leave the local screen through `dir`: switch to the neighbor in
    /// that direction. Resets the boundary state and snaps the virtual
    /// cursor to the neighbor's entry point (inset past the seam — see
    /// [`ENTRY_INSET`]). Returns nothing on a dead edge (the cursor stays
    /// clamped).
    fn switch_out(&mut self, dir: Direction) -> Vec<Action> {
        match self.layout.neighbor(0, dir, self.cursor.x, self.cursor.y) {
            Some((id, x, y)) => {
                let (x, y) = self.inset_entry(id, dir, x, y);
                self.enter_screen(id, x, y);
                vec![Action::SwitchTo { to: id, x, y }]
            }
            None => vec![], // dead edge: stay
        }
    }

    /// Push an entry point [`ENTRY_INSET`] px past the crossed edge
    /// (clamped to the screen), so the cursor never lands exactly on a
    /// wall — see the const docs for why that matters.
    fn inset_entry(&self, id: u8, dir: Direction, x: i32, y: i32) -> (i32, i32) {
        let s = self.layout.find(id).expect("entry screen must exist");
        match dir {
            Direction::Left => (x.saturating_sub(ENTRY_INSET).max(0), y),
            Direction::Right => (x.saturating_add(ENTRY_INSET).min(s.rect.w - 1), y),
            Direction::Top => (x, y.saturating_sub(ENTRY_INSET).max(0)),
            Direction::Bottom => (x, y.saturating_add(ENTRY_INSET).min(s.rect.h - 1)),
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
    /// position — arm edge crossings, because after acceleration the raw
    /// deltas no longer equal real travel. The hidden local cursor never
    /// moves while we are away.
    fn handle_remote_motion(&mut self, id: u8, dx: i32, dy: i32) -> Vec<Action> {
        let now = Instant::now();
        let rect = match self.layout.find(id) {
            Some(s) => s.rect,
            None => return vec![], // layout changed under us
        };

        // Beacon-confirmed crossings, mirroring the local side. An
        // outward push through a wall the client's *real* cursor sits on
        // (a fresh beacon armed it) crosses back — immediately.
        let fresh = self
            .remote_beacon_at
            .is_some_and(|t| now.duration_since(t) < REMOTE_BEACON_FRESH);
        for dir in DIRS {
            if pushes_outward(dir, dx, dy) {
                self.remote_last_out = Some(Push { dir, at: now });
                if fresh && self.remote_at_wall & bit(dir) != 0 {
                    if self.layout.neighbor(id, dir, self.cursor.x, self.cursor.y).is_some() {
                        return self.cross_from_remote(id, dir);
                    }
                    // Dead edge (outer wall of the desktop): stop trying.
                    self.remote_at_wall &= !bit(dir);
                }
            } else if pulls_inward(dir, dx, dy) {
                self.remote_at_wall &= !bit(dir);
            }
        }

        // No confirmed crossing: forward the motion verbatim — the
        // client's pointer transform turns it into real travel.
        let actions = vec![Action::Send(Message::MouseMoveRel { dx, dy })];

        // Stalled-stream fallback (see the module docs): only when the
        // beacon stream is dead or absent — a fresh beacon means the real
        // cursor is the authority and it has not armed this wall. The
        // virtual cursor must be outside the rect (raw deltas ran past
        // the edge) so an interior real cursor can never trip it.
        let dir = match self.layout.exit_direction(id, self.cursor.x, self.cursor.y) {
            Some(d) => d,
            None => {
                self.remote_pushing = None;
                return actions;
            }
        };
        self.clamp_to(&rect);
        let sustained = !fresh
            && self
                .remote_pushing
                .is_some_and(|p| p.dir == dir && p.since.elapsed() >= EDGE_PUSH_FALLBACK);
        if !sustained {
            self.remote_pushing = match self.remote_pushing {
                Some(p) if p.dir == dir => self.remote_pushing,
                _ => Some(Pushing { dir, since: now }),
            };
            return actions;
        }
        self.remote_pushing = None;
        match self.cross_from_remote(id, dir) {
            actions if actions.is_empty() => {
                // Dead edge after all (layout changed): keep the motion.
                vec![Action::Send(Message::MouseMoveRel { dx, dy })]
            }
            actions => actions,
        }
    }

    /// A client reported where its *real* cursor is (client-local
    /// pixels). Runs from the client's connection thread while this
    /// client is the active one; the session mutex serializes it with the
    /// main loop's local-input processing.
    ///
    /// The real position is the ground truth on a remote screen (the
    /// client's OS applied its own transform to our relative motion), so
    /// it both re-anchors the virtual cursor and updates the wall-arm
    /// state — mirroring the local-screen beacon exactly. When the beacon
    /// *parks* the cursor on a wall mid-push, the crossing is returned
    /// here so the caller can execute it on the spot (the client's
    /// position stream is the only input that may not be followed by
    /// another motion frame).
    pub fn on_remote_beacon(&mut self, id: u8, x: i32, y: i32) -> Vec<Action> {
        if !matches!(self.cursor.mode, Mode::Remote(cur) if cur == id) {
            return vec![];
        }
        let Some(screen) = self.layout.find(id) else { return vec![] };
        let now = Instant::now();
        self.remote_beacon_at = Some(now);
        // Clamp the report into the rect: the OS pins the cursor at the
        // last pixel, but a report in flight can be a hair past it. The
        // `.max(0)` keeps a degenerate zero-size rect from panicking.
        let x = x.clamp(0, (screen.rect.w - 1).max(0));
        let y = y.clamp(0, (screen.rect.h - 1).max(0));
        // Re-anchor the virtual cursor to the real position so the
        // stalled-stream fallback starts from reality, not from raw
        // deltas that acceleration ran far ahead of.
        self.cursor.x = screen.rect.x + x;
        self.cursor.y = screen.rect.y + y;
        let bits = wall_bits(&screen.rect, x, y);
        let newly = bits & !self.remote_at_wall;
        self.remote_at_wall = bits;
        if bits == 0 {
            // The client's real cursor is back inside its screen: any
            // edge-push attempt was a transient overshoot.
            self.remote_pushing = None;
            return vec![];
        }
        // A beacon parking the real cursor on a wall mid-push crosses on
        // the park itself — mirror [`Session::on_local_beacon`]. The
        // caller (the client's connection thread) executes the actions.
        for dir in DIRS {
            if newly & bit(dir) != 0
                && self
                    .remote_last_out
                    .is_some_and(|p| p.dir == dir && p.at.elapsed() < EDGE_PUSH_FRESH)
            {
                let actions = self.cross_from_remote(id, dir);
                if !actions.is_empty() {
                    return actions;
                }
            }
        }
        vec![]
    }

    /// Switch away from the remote screen `id` through `dir` (back home
    /// or on to another client). Resets the remote boundary state and
    /// snaps the virtual cursor to the destination's entry point (inset
    /// past the seam — see [`ENTRY_INSET`]).
    fn cross_from_remote(&mut self, id: u8, dir: Direction) -> Vec<Action> {
        match self.layout.neighbor(id, dir, self.cursor.x, self.cursor.y) {
            Some((next, x, y)) => {
                let (x, y) = self.inset_entry(next, dir, x, y);
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
    /// coords `x, y`), set the mode, and clear every boundary latch — the
    /// cursor has just arrived on a new screen, and any arm from the old
    /// one is meaningless. The destination's own wall/beacon stream
    /// re-arms from scratch.
    fn enter_screen(&mut self, id: u8, x: i32, y: i32) {
        let s = self.layout.find(id).expect("entry screen must exist");
        self.cursor.x = s.rect.x + x;
        self.cursor.y = s.rect.y + y;
        self.cursor.mode = if id == 0 { Mode::Local } else { Mode::Remote(id) };
        self.clear_boundary_state();
    }

    /// Reset every boundary latch (used on entry, layout swaps and
    /// forced returns home).
    fn clear_boundary_state(&mut self) {
        self.at_wall = 0;
        self.last_out = None;
        self.pushing = None;
        self.remote_at_wall = 0;
        self.remote_beacon_at = None;
        self.remote_last_out = None;
        self.remote_pushing = None;
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

/// The cursor model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    /// Virtual position.
    x: i32,
    y: i32,
    mode: Mode,
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
    /// really happens: a beacon arms the left wall (the real cursor
    /// reached it), then an outward push fires the crossing.
    fn cross_to_hp(s: &mut Session) {
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    /// Assert the only action is a switch to hp at its right edge.
    fn assert_switch_to_hp(actions: &[Action], y: i32) {
        match actions {
            [Action::SwitchTo { to, x, y: ay }] => {
                assert_eq!(*to, 1);
                assert_eq!(*x, 1871); // hp's right edge, inset 48 px from the seam
                assert_eq!(*ay, y);
            }
            other => panic!("expected SwitchTo to hp, got {other:?}"),
        }
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
        // The real cursor reaches the left wall (beacon arms it), then an
        // outward push fires the crossing.
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(s.mode(), Mode::Remote(1));
        // Virtual position was snapped to hp's entry point, 48 px past
        // the seam (-49, 540) — never exactly on the wall.
        assert_eq!(s.cursor_pos(), (-49, 540));
    }

    #[test]
    fn interior_beacon_never_arms_and_pushes_do_not_cross() {
        // The real cursor is mid-screen: even a hard outward push (raw
        // deltas run ahead of the visible cursor) must not cross until a
        // beacon puts the real cursor on the wall.
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveAbs { x: 500, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Local, "interior real cursor must never cross");
        assert_eq!(s.cursor_pos(), (0, 540)); // virtual clamped at the wall
    }

    #[test]
    fn beacon_park_mid_push_crosses_on_the_park_itself() {
        // A fast sweep: raw deltas race to the wall while the real cursor
        // is still travelling. The beacon that parks it there arrives
        // mid-push and must complete the crossing *on the park* — no
        // waiting for the next delta, no dead frame at the boundary.
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
        let actions = s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn beacon_park_after_push_went_stale_only_arms() {
        // A flick ends with the cursor at the wall, then the user stops
        // and waits: the push is no longer fresh when the park beacon
        // arrives, so it must only arm — resting at the edge never
        // crosses. A later outward push fires.
        let mut s = two_screens();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
        assert_eq!(s.mode(), Mode::Local, "resting at the wall must not cross");
        // A fresh push while parked crosses immediately (confirmed by the
        // beacon).
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn inward_motion_disarms_the_wall() {
        // The cursor parks on the left wall (armed), then the user moves
        // back inside: the wall must disarm, so a later outward push
        // cannot fire until a beacon re-arms it (this is the hysteresis
        // that keeps the seam placement from bouncing).
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }); // arm left
        // Move away from the wall — the real cursor leaves it.
        s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 }); // inward: disarm
        // An outward push without a fresh wall beacon must not cross.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Local);
        // The real cursor confirms it is back inside.
        s.on_local_event(Message::MouseMoveAbs { x: 100, y: 540 });
        // Let the push record go stale so the next park beacon only arms
        // (a beacon parks the wall mid-push would fire on the park).
        std::thread::sleep(EDGE_PUSH_FRESH + Duration::from_millis(20));
        // The real cursor reaches the wall again: the beacon arms it, and
        // only then does an outward push cross.
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        assert_switch_to_hp(&actions, 540);
    }

    #[test]
    fn sliding_along_the_wall_does_not_cross() {
        // The cursor is pinned on the left wall and slides vertically
        // (aiming at something near the edge). Vertical motion is not an
        // outward push through the left wall, so it must never fire.
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 0, dy: -200 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.mode(), Mode::Local);
        // Only a genuine outward push crosses.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        assert_switch_to_hp(&actions, 340);
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
    fn remote_motion_is_scaled_by_the_measured_pointer_gain() {
        // The server measures its own px-per-count (e.g. libinput's 0.5
        // at slow speeds) and scales forwarded motion by it, so a client
        // that places its cursor absolutely (1:1) mirrors the server's
        // cursor exactly. Default gain 1.0 leaves motion untouched; a
        // measured gain of 0.5 halves the forwarded counts and the
        // virtual cursor advance — both, so the client's landing spot
        // and the boundary state stay consistent.
        let mut s = two_screens();
        cross_to_hp(&mut s);
        // Gain 1.0 (default / not yet measured): forwarded verbatim.
        assert_eq!(
            s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 }),
            vec![Action::Send(Message::MouseMoveRel { dx: -10, dy: 0 })]
        );
        // The server measured 0.5 px/count (its cursor travels half the
        // raw counts): forwarded motion — and the virtual advance — are
        // halved (rounded per frame).
        s.set_gain(0.5);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -5, dy: 0 })]);
        // Sub-pixel frames round; the average stays 0.5.
        let sum: i64 = (0..20)
            .map(|_| {
                match s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 }).first().unwrap() {
                    Action::Send(Message::MouseMoveRel { dx, .. }) => *dx as i64,
                    other => panic!("unexpected {other:?}"),
                }
            })
            .sum();
        assert!((5..=15).contains(&sum), "20 x 1 count at 0.5 gain should sum near 10, got {sum}");
        // Local motion is never scaled (beacons re-anchor the virtual
        // cursor there).
        s.force_local();
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 10, dy: 0 }), vec![]);
    }

    #[test]
    fn crossing_back_returns_to_local() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // to hp, virtual (-1, 540) = hp's right edge
        // The client's real cursor sits where control placed it: on hp's
        // right wall (the shared edge with pc). The beacon arms it.
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        // A push right while the real cursor is on that wall crosses
        // home.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn crossing_home_requires_the_real_cursor_at_the_wall() {
        // Deltas alone must never cross back home: after acceleration the
        // raw deltas run far ahead of the client's real cursor. While the
        // real cursor is still interior, an outward overshoot only
        // forwards motion.
        let mut s = two_screens();
        cross_to_hp(&mut s);
        assert_eq!(s.on_remote_beacon(1, 900, 540), vec![]); // real cursor mid-screen
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 5, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "overshoot while interior must not cross");
        // Only once the client reports its real cursor on the shared wall
        // does the crossing happen — and with the push still fresh it
        // fires on the park itself (no dead frame at the boundary).
        let actions = s.on_remote_beacon(1, 1919, 540);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn entry_is_inset_past_the_seam() {
        // The cursor enters hp 48 px past the seam — never exactly on the
        // wall — so hp's first beacon reports an interior cursor, not a
        // park. (An entry exactly on the wall made the first beacon a
        // park with the crossing push still fresh, which bounced the
        // cursor straight back across the seam.)
        let mut s = two_screens();
        s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 });
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(
            s.on_remote_beacon(1, 1871, 540),
            vec![],
            "a beacon at the inset entry point is interior, not a wall park"
        );
        assert_eq!(s.mode(), Mode::Remote(1));
        // A genuine park at the shared wall still crosses home — the
        // inset only stops seam-jitter bounce, not real travel.
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
        // And coming home is inset too: the local beacon at the entry
        // point is interior and must not immediately re-cross.
        assert_eq!(
            s.on_local_event(Message::MouseMoveAbs { x: 48, y: 540 }),
            vec![],
            "the local beacon at the inset point is interior, not a wall park"
        );
    }

    #[test]
    fn remote_inward_motion_disarms_the_wall() {
        // After entering hp the cursor sits on the seam (hp's right
        // wall). Moving *into* hp disarms it, so a stray outward jitter
        // cannot bounce control straight back home.
        let mut s = two_screens();
        cross_to_hp(&mut s);
        s.on_remote_beacon(1, 1919, 540); // arm the seam wall
        s.on_local_event(Message::MouseMoveRel { dx: -1, dy: 0 }); // into hp: disarm
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 1, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "disarmed wall must not fire");
        // The real cursor must reach the wall again; with the push still
        // fresh, the park itself completes the crossing home.
        let actions = s.on_remote_beacon(1, 1919, 540);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn remote_beacon_park_mid_push_crosses_on_the_park() {
        // The user sweeps right across hp toward home and the beacon
        // parks the real cursor on the shared wall mid-push: the crossing
        // fires on the park itself (the actions are returned to the
        // client thread to execute), no dead frame at the boundary.
        let mut s = two_screens();
        cross_to_hp(&mut s);
        // A hard outward push races the virtual cursor out of hp.
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 2000, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: 2000, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1));
        // The beacon parks the real cursor on the wall mid-push: cross now.
        let actions = s.on_remote_beacon(1, 1919, 540);
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
        // Even with the real cursor pinned on that outer wall and a
        // fresh push, there is nowhere to go: motion forwards, hp keeps
        // control.
        assert_eq!(s.on_remote_beacon(1, 0, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -5, dy: 0 })]);
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
    fn remote_to_remote_switch_fires_on_the_park() {
        let layout = Layout::new(vec![
            Screen { id: 0, name: "pc".into(), rect: Rect { x: 0, y: 0, w: 1920, h: 1080 } },
            Screen { id: 1, name: "hp".into(), rect: Rect { x: -1920, y: 0, w: 1920, h: 1080 } },
            Screen { id: 2, name: "mac".into(), rect: Rect { x: -3840, y: 0, w: 1920, h: 1080 } },
        ]);
        let mut s = Session::new(layout, 0);
        // pc -> hp
        cross_to_hp(&mut s);
        // Swoop across hp toward its left wall (raw deltas overshoot the
        // rect; no beacon has confirmed the real cursor there yet).
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 });
        assert_eq!(actions, vec![Action::Send(Message::MouseMoveRel { dx: -2000, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "overshoot alone must not switch");
        // The client reports its real cursor parked on hp's left wall
        // while the sweep is still pushing: switch on to mac, on the
        // park itself.
        let actions = s.on_remote_beacon(1, 0, 540);
        match actions.as_slice() {
            [Action::SwitchTo { to, x, y }] => {
                assert_eq!(*to, 2);
                assert_eq!(*x, 1871); // mac's right edge, inset 48 px
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
        // A beacon showing the real cursor back inside confirms it was an
        // overshoot.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 300, y: 540 }), vec![]);
        assert_eq!(s.cursor_pos(), (300, 540));
        // Continuing from there stays local.
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -10, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn sustained_push_crosses_when_the_beacon_stream_stalls() {
        // A stalled beacon stream: the OS has pinned the pointer at the
        // edge, position events (and with them beacons) stop, and only
        // raw deltas keep flowing. Without a beacon the first push cannot
        // be confirmed — but sustained pushing past the fallback window
        // (with the virtual cursor outside the rect) must still cross, or
        // the cursor would stick at the edge forever.
        let mut s = two_screens();
        // The push reaches the edge and stays there (unconfirmed, no
        // switch yet).
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: -2000, dy: 0 }), vec![]);
        assert_eq!(s.mode(), Mode::Local, "first unconfirmed push must not switch");
        // Wait out the fallback window, then keep pushing.
        std::thread::sleep(EDGE_PUSH_FALLBACK + Duration::from_millis(20));
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -5, dy: 0 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(s.mode(), Mode::Remote(1));
    }

    #[test]
    fn local_abs_beacon_is_ignored_while_remote() {
        let mut s = two_screens();
        cross_to_hp(&mut s); // on hp, virtual (-49, 540)
        // A *local* capture beacon while remote is the hidden parked
        // cursor (meaningless): it must not resync the virtual position.
        let actions = s.on_local_event(Message::MouseMoveAbs { x: 50, y: 60 });
        assert_eq!(actions, vec![]);
        assert_eq!(s.cursor_pos(), (-49, 540)); // untouched
        // Crossing home is driven by the client's own beacon, not the
        // local one.
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 5, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
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
        cross_to_hp(&mut s); // virtual (-49, 540): the entry inset
        assert_eq!(s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 }), vec![Action::Send(Message::MouseMoveRel { dx: 1, dy: 0 })]);
        assert_eq!(s.mode(), Mode::Remote(1), "no beacon yet: one push must not cross");
        // Keep pushing until the virtual cursor has traversed the entry
        // inset and actually leaves hp's rect — then the fallback window
        // must bring control home.
        for _ in 0..60 {
            s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        }
        std::thread::sleep(REMOTE_BEACON_FRESH + EDGE_PUSH_FALLBACK + Duration::from_millis(20));
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 1, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
    }

    #[test]
    fn remote_beacon_resyncs_the_virtual_cursor() {
        // The client's real cursor (post-acceleration) is the ground
        // truth on its screen. A beacon must re-anchor the virtual cursor
        // so the stalled-stream fallback and entry math start from
        // reality instead of raw deltas that acceleration ran ahead of.
        let mut s = two_screens();
        cross_to_hp(&mut s); // virtual (-1, 540)
        // The client reports its real cursor mid-screen (our raw deltas
        // had overshot): snap to reality.
        s.on_remote_beacon(1, 900, 200);
        assert_eq!(s.cursor_pos(), (-1020, 200));
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

    #[test]
    fn wall_bits_marks_the_outer_band_only() {
        let rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };
        assert_eq!(wall_bits(&rect, 0, 540), BIT_LEFT);
        assert_eq!(wall_bits(&rect, 1, 540), BIT_LEFT); // band slack
        assert_eq!(wall_bits(&rect, 2, 540), 0);
        assert_eq!(wall_bits(&rect, 1918, 540), BIT_RIGHT);
        assert_eq!(wall_bits(&rect, 1919, 540), BIT_RIGHT);
        assert_eq!(wall_bits(&rect, 1919, 0), BIT_RIGHT | BIT_TOP); // corner
        assert_eq!(wall_bits(&rect, 960, 540), 0);
    }

    #[test]
    fn crossing_roundtrip_back_and_forth_is_crisp() {
        // Rapid back-and-forth at the boundary: each direction must cross
        // on a beacon-arm plus one push — no fallback timers in the
        // common path.
        let mut s = two_screens();
        // pc -> hp
        cross_to_hp(&mut s);
        // hp -> pc
        assert_eq!(s.on_remote_beacon(1, 1919, 540), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: 2, dy: 0 });
        assert_eq!(actions, vec![Action::SwitchToLocal { x: 48, y: 540 }]);
        assert_eq!(s.mode(), Mode::Local);
        // pc -> hp again, immediately.
        assert_eq!(s.on_local_event(Message::MouseMoveAbs { x: 0, y: 540 }), vec![]);
        let actions = s.on_local_event(Message::MouseMoveRel { dx: -2, dy: 0 });
        assert_switch_to_hp(&actions, 540);
        assert_eq!(s.mode(), Mode::Remote(1));
    }
}
