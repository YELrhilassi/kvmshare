//! The event handlers that drive the boundary state.
//!
//! Two cursor streams feed the session: **raw deltas** (pre-acceleration
//! device counts, instantaneous but running ahead of the visible cursor)
//! and **real-position beacons** (where the visible cursor actually is —
//! the server's own capture on the local side, the active client's
//! reports on the remote side). A crossing needs both, together: a
//! beacon *arms* a wall, an outward push *fires* it. See
//! [`super::boundary`] for the full model.

use std::time::Instant;

use kvmshare_protocol::message::{KeyKind, Message, Rect};

use crate::actions::UserAction;
use crate::layout::Direction;
use crate::Mode;

use super::boundary::*;
use super::{Action, Session};

impl Session {
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
            Message::MouseWheel { dx, dy } => {
                let (dx, dy) = self.prefs.transform_wheel(dx, dy);
                self.forward_while_remote(Message::MouseWheel { dx, dy })
            }
            Message::Key { kind, key } => self.on_local_key(kind, key),
            // The user pressed the escape key (Scroll Lock) while the
            // cursor was on a client: bring control home, no matter what
            // the client is doing. This is the universal "unstick" — it
            // works even when the client's machine cannot inject input
            // (an elevated window, a wedged session, a dead client).
            Message::Escape => vec![self.force_local()],
            _ => vec![],
        }
    }

    /// A local key event, run through the action engine first: a chord
    /// the engine owns is consumed (never forwarded) and its action —
    /// if any — executed; everything else forwards as before while the
    /// cursor is away. The Scroll Lock escape intercept lives in the
    /// capture layer and runs *before* this, so `home` stays available
    /// even with shortcuts disabled.
    fn on_local_key(&mut self, kind: KeyKind, key: u32) -> Vec<Action> {
        let at_home = matches!(self.cursor.mode, Mode::Local);
        match kind {
            KeyKind::Down => {
                if let Some(action) = self.actions.key_down(key, self.mods_snapshot(), at_home) {
                    return self.execute_user_action(action);
                }
            }
            KeyKind::Up => {
                if self.actions.key_up(key).is_some() {
                    return vec![]; // the release belongs to the engine
                }
            }
            KeyKind::Repeat => {}
        }
        self.forward_while_remote(Message::Key { kind, key })
    }

    /// Modifier state for the action engine. The capture layer reports
    /// modifier presses as ordinary key events; the engine accumulates
    /// them into a chord's mod set itself, so a fresh event needs only
    /// the empty set unless a chord is mid-flight — the engine's own
    /// bookkeeping is authoritative between events. (Full mod tracking
    /// arrives with the customizable-binding UI; the default chords are
    /// modifier-free.)
    fn mods_snapshot(&self) -> crate::actions::Mods {
        crate::actions::Mods::NONE
    }

    /// Run one user action from the engine. Anything it cannot do in
    /// the current state (switch to an offline client, cycle with no
    /// reachable screens) is a no-op — never an error the user must
    /// see; the shortcut did nothing this time.
    fn execute_user_action(&mut self, action: UserAction) -> Vec<Action> {
        match action {
            UserAction::SwitchToScreen { to } => {
                let id = self.layout.screens.iter().find(|s| s.name == to).map(|s| s.id);
                match id {
                    Some(id) if self.reachable(id) => self.jump_to(id),
                    _ => vec![],
                }
            }
            UserAction::SwitchNext => {
                // The next reachable screen in layout order after the
                // current one (wrapping). With one reachable screen
                // there is nowhere to go: no-op.
                let current = match self.cursor.mode {
                    Mode::Local => 0,
                    Mode::Remote(id) => id,
                };
                let ids: Vec<u8> = self.layout.screens.iter().map(|s| s.id).collect();
                let start = ids.iter().position(|&id| id == current).map(|p| p + 1).unwrap_or(0);
                let next = (0..ids.len())
                    .map(|k| ids[(start + k) % ids.len()])
                    .find(|&id| id != current && self.reachable(id));
                match next {
                    Some(id) => self.jump_to(id),
                    None => vec![],
                }
            }
            UserAction::ToggleLock => {
                self.walls_locked = !self.walls_locked;
                vec![]
            }
            UserAction::GoHome if !matches!(self.cursor.mode, Mode::Local) => {
                vec![self.force_local()]
            }
            UserAction::GoHome => vec![],
        }
    }

    /// Jump the cursor to screen `id`, entering at its center — the
    /// direct counterpart of a wall crossing, without one: no inset
    /// seam (the cursor did not travel an edge) and no direction.
    fn jump_to(&mut self, id: u8) -> Vec<Action> {
        let (cx, cy) = match self.layout.find(id) {
            Some(s) => s.rect.center(),
            None => return vec![],
        };
        self.enter_screen(id, cx, cy);
        if id == 0 {
            vec![Action::SwitchToLocal { x: cx, y: cy }]
        } else {
            vec![Action::SwitchTo { to: id, x: cx, y: cy }]
        }
    }
}

impl Session {
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

    /// Forward a message to the active client, if the cursor is remote.
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
        // by the server's measured pointer gain ([`Session::set_gain`]) so
        // the virtual cursor — and the motion forwarded to the client —
        // moves exactly like the server's own visible cursor would for
        // the same hand motion. Local motion stays raw: the server's
        // real-position beacons re-anchor the virtual cursor anyway.
        let (sx, sy) = match self.cursor.mode {
            Mode::Remote(_) => {
                let g = self.gain * self.prefs.pointer_speed;
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
        // The user's wall lock: walls are hard while locked. The
        // escape key and the shortcut actions bypass this (they are
        // explicit requests, not wall pushes).
        if self.walls_locked {
            return vec![];
        }
        match self.layout.neighbor(0, dir, self.cursor.x, self.cursor.y) {
            // Only a screen with a live client is a destination — a
            // configured-but-offline client is a dead edge (see the
            // [`Session::connected`] docs): crossing into it would arm
            // the engine's isolation with nothing on the other side to
            // return control to.
            Some((id, x, y)) if self.reachable(id) => {
                let (x, y) = self.inset_entry(id, dir, x, y);
                self.enter_screen(id, x, y);
                vec![Action::SwitchTo { to: id, x, y }]
            }
            _ => vec![], // dead edge: stay
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
        if self.walls_locked {
            return vec![];
        }
        match self.layout.neighbor(id, dir, self.cursor.x, self.cursor.y) {
            // Home is always reachable; another client only while it is
            // connected (see the [`Session::connected`] docs).
            Some((next, x, y)) if self.reachable(next) => {
                let (x, y) = self.inset_entry(next, dir, x, y);
                self.enter_screen(next, x, y);
                if next == 0 {
                    vec![Action::SwitchToLocal { x, y }]
                } else {
                    vec![Action::SwitchTo { to: next, x, y }]
                }
            }
            _ => vec![], // outer edge of the desktop: stay
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
    /// forced returns home). `pub(super)` because the session root calls
    /// it from `swap_layout`.
    pub(super) fn clear_boundary_state(&mut self) {
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
}