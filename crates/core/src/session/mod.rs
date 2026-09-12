//! The session: one cursor, one layout, and the rules for moving between
//! screens.
//!
//! The session is pure logic. It owns the *virtual* cursor position and
//! answers "what should happen next?" by returning [`Action`]s. The
//! caller (the server or a test harness) executes the actions.
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
//!   every local window it crosses, so local elements would react while
//!   the user is working on a client. The virtual cursor is driven
//!   entirely by *raw* motion and real-position beacons (see
//!   `kvmshare-platform`), which do not depend on the physical cursor's
//!   position at all — so a static park loses nothing.
//!
//! ## Layout
//!
//! * [`boundary`] — the crossing model: what "at the wall" means, how a
//!   crossing is armed and fired, and the push latches.
//! * [`motion`] — the event handlers that drive the boundary state from
//!   the two cursor streams (raw deltas and real-position beacons).

mod boundary;
mod motion;

use kvmshare_protocol::message::{Message, Rect, Screen, ScreenInfo};

use crate::actions::{ActionEngine, BindSection};
use crate::input::InputPrefs;
use crate::layout::Layout;
use crate::Mode;

pub use boundary::{EDGE_BAND, EDGE_PUSH_FALLBACK, EDGE_PUSH_FRESH, ENTRY_INSET, REMOTE_BEACON_FRESH};
pub use crate::actions::Mods;

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
    last_out: Option<boundary::Push>,
    /// A stalled-stream fallback in progress on the local screen.
    pushing: Option<boundary::Pushing>,

    // Remote-mode boundary state (mirrors the local side, fed by the
    // active client's real-cursor beacons).
    remote_at_wall: u8,
    /// When the last remote beacon arrived (`None` = none yet).
    remote_beacon_at: Option<std::time::Instant>,
    remote_last_out: Option<boundary::Push>,
    remote_pushing: Option<boundary::Pushing>,

    /// The server's measured pointer gain (pixels of real cursor travel
    /// per raw device count), applied to forwarded remote motion so the
    /// client's cursor travels exactly like the server's own would.
    /// Updated by the server loop from its `GainTracker`; defaults to
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
    /// The ids of client screens whose client is **connected right now**
    /// (see [`Session::on_client_connected`]). A crossing may only enter
    /// a screen with a live client: crossing into an empty slot would
    /// arm the engine's isolation and hide the local cursor with nothing
    /// on the other side to return control to — the machine would sit
    /// input-locked until the escape key, and every beacon watchdog
    /// would trip on a stream that cannot exist. A configured screen
    /// with its client offline is therefore a dead edge, exactly like a
    /// desktop edge with no neighbor.
    connected: std::collections::HashSet<u8>,
    /// The user's shortcut bindings (see [`crate::actions`]). Every
    /// local key event passes through it before anything else; chords
    /// it owns are consumed here and never reach the client.
    actions: ActionEngine,
    /// Whether edge crossings are locked (the user's ToggleLock
    /// action). While locked, walls are hard: outward pushes never
    /// switch — the cursor stays home. The escape key still works (it
    /// is the rescue hatch, handled before this flag).
    walls_locked: bool,
    /// User input preferences (pointer/wheel feel) applied to the
    /// forwarded stream — see [`crate::input`].
    prefs: InputPrefs,
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
            connected: std::collections::HashSet::new(),
            actions: ActionEngine::new(BindSection::default()),
            walls_locked: false,
            prefs: InputPrefs::default(),
        }
    }

    /// Update the pointer-gain estimate (see the field docs). Called by
    /// the server loop whenever a local motion window closes; affects
    /// only future *remote* motion.
    pub fn set_gain(&mut self, gain: f64) {
        self.gain = gain.clamp(0.25, 3.0);
    }

    /// Adopt new shortcut bindings (config hot-reload).
    pub fn set_bindings(&mut self, cfg: BindSection) {
        self.actions.set_config(cfg);
    }

    /// Adopt new input preferences (config hot-reload).
    pub fn set_prefs(&mut self, prefs: InputPrefs) {
        let mut prefs = prefs;
        prefs.clamp();
        self.prefs = prefs;
    }

    /// The keyboard chords the capture layer must **intercept at the
    /// OS level** (see [`Server::Engine::set_bound_chords`]): chords
    /// bound to kvmshare actions must beat whatever the desktop would do
    /// with them (Win+Tab, media keys, …). Each pair is `(mods, key)` —
    /// `mods` is the packed 4-bit mask documented on the Engine method
    /// (bit0 ctrl, bit1 alt, bit2 shift, bit3 meta); `key` is the
    /// canonical HID usage.
    pub fn bound_chords(&self) -> Vec<(u8, u32)> {
        let cfg = self.actions.config();
        if !cfg.enabled {
            return Vec::new();
        }
        cfg.bindings
            .iter()
            .map(|b| {
                let m = b.mods;
                let packed =
                    (m.ctrl as u8) | ((m.alt as u8) << 1) | ((m.shift as u8) << 2) | ((m.meta as u8) << 3);
                (packed, b.key)
            })
            .collect()
    }

    /// The last chord-shaped press the engine saw (mods + HID key), for
    /// the GUI's live key-detection display.
    pub fn last_seen_chord(&self) -> Option<(crate::actions::Mods, u32)> {
        self.actions.last_seen()
    }

    /// The current input preferences (read-only view).
    pub fn prefs(&self) -> &InputPrefs {
        &self.prefs
    }

    /// Are edge crossings currently locked?
    pub fn walls_locked(&self) -> bool {
        self.walls_locked
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
        // The layout lives in the same space the injector reports and
        // beacons from (physical pixels on Windows, root pixels on X11 —
        // where the scale is always 1.0), so the reported size is used
        // as-is.
        let w = info.width.max(1) as i32;
        let h = info.height.max(1) as i32;
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
    pub fn update_screen_info(&mut self, id: u8, info: ScreenInfo) {
        if let Some(s) = self.layout.screens.iter_mut().find(|s| s.id == id) {
            // Same space as [`Self::admit_client`] — see its comment.
            s.rect.w = info.width.max(1) as i32;
            s.rect.h = info.height.max(1) as i32;
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

    /// A client with the given id finished its handshake and is now a
    /// live peer: crossings may enter its screen. Called once per
    /// connection, after the client is registered server-side, so a
    /// crossing can never race ahead of the registration (its `Enter`
    /// would be dropped and the cursor stranded on a dead screen).
    pub fn on_client_connected(&mut self, id: u8) {
        self.connected.insert(id);
    }

    /// A client with the given id disconnected: drop back to the local
    /// screen if the cursor was on it, and make its screen a dead edge
    /// again (no crossing may enter a screen whose client is gone).
    pub fn on_client_disconnected(&mut self, id: u8) -> Action {
        self.connected.remove(&id);
        if self.cursor.mode == Mode::Remote(id) {
            self.force_local()
        } else {
            Action::Nothing
        }
    }

    /// A client was disconnected *by policy* (revoked, or its screen left
    /// the layout): additionally remove its screen from the desktop.
    ///
    /// Why not leave the screen as a dead edge: a revoked machine must not
    /// keep a place in the desktop it may no longer occupy — the operator
    /// refused the machine, not just its current session. It also keeps a
    /// re-admission clean: when the machine is un-revoked and returns, it
    /// is admitted fresh (its own position, right of the desktop) instead
    /// of silently re-inheriting a stale slot, and the GUI's layout view
    /// stops showing a screen for a machine that cannot connect.
    /// A screen the operator *pinned* in the config is unaffected: the
    /// running session's copy goes, and the config still defines it — the
    /// machine simply cannot connect while revoked.
    pub fn on_client_removed(&mut self, id: u8) -> Action {
        let action = self.on_client_disconnected(id);
        if self.layout.screens.iter().any(|s| s.id == id) {
            self.layout.screens.retain(|s| s.id != id);
            self.dynamic.retain(|s| s.id != id);
            self.layout = self.layout.normalized();
            self.clear_boundary_state();
        }
        action
    }

    /// May the cursor enter screen `id`? The local screen always; a
    /// remote screen only while its client is connected (see the
    /// [`Session::connected`] docs).
    fn reachable(&self, id: u8) -> bool {
        id == 0 || self.connected.contains(&id)
    }

    #[cfg(test)]
    pub fn cursor_pos(&self) -> (i32, i32) {
        (self.cursor.x, self.cursor.y)
    }
}

#[cfg(test)]
mod tests;