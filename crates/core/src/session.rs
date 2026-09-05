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

use kvmshare_protocol::message::{Message, Rect, Screen, ScreenInfo};

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
    /// Screens admitted at runtime for clients the configured layout had
    /// no screen for (see [`Session::admit_client`]). They live in
    /// [`Self::layout`] alongside the configured screens while their
    /// client is around; a config reload keeps the ones the config still
    /// does not name and drops the ones the user has since pinned (or
    /// removed) — so a Layout-page edit is authoritative, and a dynamic
    /// client never flaps on an unrelated reload.
    dynamic: Vec<Screen>,
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
            dynamic: Vec::new(),
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

    /// Admit a client into the session's layout, dynamically if needed.
    ///
    /// A client whose name already maps to a configured screen is left
    /// untouched (`admitted == false`). A client with no screen yet — a
    /// machine the layout has never seen — is admitted on the spot: a
    /// screen is created from its *reported* geometry and placed to the
    /// right of the current desktop, so the very first connection of a
    /// fresh pair of machines already works with zero layout
    /// configuration. The Layout page is where a permanent position is
    /// pinned; until then the screen exists only in the running session
    /// (a config reload keeps it while the client stays connected and
    /// drops it once the client is gone).
    ///
    /// Returns `None` only when the name collides with the server's own
    /// screen (id 0 — a machine cannot connect to itself) or when every
    /// screen id is taken.
    pub fn admit_client(&mut self, name: &str, info: ScreenInfo) -> Option<(u8, bool)> {
        if let Some(id) = self.assign_screen_id(name) {
            return Some((id, false));
        }
        // The local screen is this server's own identity; its name is
        // never assignable to a remote.
        if self.layout.screens.iter().any(|s| s.id == 0 && s.name == name) {
            return None;
        }
        let used: std::collections::HashSet<u8> = self.layout.screens.iter().map(|s| s.id).collect();
        let id = (1u8..=u8::MAX).find(|c| !used.contains(c))?;
        // Reported geometry is physical pixels; the layout works in
        // logical ones (same conversion as [`Self::update_screen_info`]).
        let w = (info.width as f32 / info.scale.max(0.1)) as i32;
        let h = (info.height as f32 / info.scale.max(0.1)) as i32;
        // To the right of everything currently on the desktop — the
        // plug-and-play default, collision-free because no existing
        // screen extends further right. The user rearranges it later in
        // the Layout page; `normalized()` then snaps it like any
        // GUI-built layout.
        let x = self.layout.screens.iter().map(|s| s.rect.x + s.rect.w).max().unwrap_or(0);
        let y = self.local.y;
        let screen = Screen {
            id,
            name: name.to_owned(),
            rect: Rect { x, y, w: w.max(1), h: h.max(1) },
        };
        self.dynamic.push(screen.clone());
        self.layout.screens.push(screen);
        self.layout = self.layout.normalized();
        Some((id, true))
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
    ///
    /// The incoming layout is authoritative for everything it names. A
    /// dynamically admitted client whose screen the config still does
    /// not provide rides along (so an unrelated layout edit never kicks
    /// it off); one the user has since pinned into the config — or
    /// removed — is dropped, because its configured copy (or its
    /// absence) now rules.
    pub fn swap_layout(&mut self, layout: Layout) -> Vec<Action> {
        let local = match layout.find(0) {
            Some(s) => s.rect,
            None => return vec![],
        };
        let was_remote = self.cursor.mode != Mode::Local;
        let (cx, cy) = local.center();

        let mut layout = layout;
        let mut kept = Vec::with_capacity(self.dynamic.len());
        for s in self.dynamic.drain(..) {
            let pinned = layout.screens.iter().any(|d| d.name == s.name);
            if !pinned {
                layout.screens.push(s.clone());
                kept.push(s);
            }
        }
        self.dynamic = kept;
        layout = layout.normalized();
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
#[path = "session_tests.rs"]
mod tests;

