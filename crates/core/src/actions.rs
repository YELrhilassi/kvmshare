//! The action engine: user-facing commands bound to keystrokes, handled
//! in exactly one place.
//!
//! ## Why here, and why portable
//!
//! Every input path funnels through [`crate::session::Session`], and
//! every key arrives there as a **canonical USB HID usage id** (the
//! protocol's key identity — X11 keycodes, evdev codes and Windows scan
//! codes are all translated at the platform edge). A chord matcher that
//! sees HID usages therefore works identically on every platform pair:
//! a binding recorded on Windows fires the same on Linux, with the
//! server as client, client as server, or cursor at home. Nothing here
//! touches the OS — capture and injection stay in the platform crate;
//! the engine only *decides*.
//!
//! ## Shape
//!
//! * [`UserAction`] — what a keystroke can *do*, expressed in session
//!   terms (switch / lock / control). New actions extend here without
//!   touching the platform layer.
//! * [`Binding`] — one chord (`mods` + `key`) mapped to one action.
//! * [`BindSection`] — the `[shortcuts]` TOML section, owned by the
//!   server config, hot-reloaded like the layout.
//! * [`ActionEngine`] — holds the bindings and the chord-in-progress
//!   state. The session feeds it every key press; it returns a user
//!   action when a chord completes and owns the "is this chord mine?"
//!   answer: a key that starts one of *its* chords is swallowed (never
//!   forwarded to the client) until the chord resolves; anything else
//!   passes through untouched.
//!
//! Defaults are conservative: the Scroll Lock escape stays hard-wired
//! in the capture layer (it must work even with zero config), and the
//! default chord set only binds keys with no application meaning.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A user-facing command the engine can fire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserAction {
    /// Switch the cursor to the screen named `to` (layout-order name,
    /// e.g. "hp"). Direction-free: the user names machines, not sides.
    SwitchToScreen { to: String },
    /// Cycle through the reachable screens in layout order.
    SwitchNext,
    /// Toggle the desktop edge walls: locked = crossings disabled.
    ToggleLock,
    /// Return control to the server immediately (same as the escape key).
    GoHome,
}

/// Modifier bits for a chord. Platform-independent: every platform's
/// modifier keys arrive here as the same HID usages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, meta: false };
}

/// HID usages the engine distinguishes: the eight modifiers plus the
/// keys the default chords bind. (Full tables live in the platform
/// crate; the engine only needs these.)
pub mod hid {
    pub const SCROLL_LOCK: u32 = 0x47;
    pub const PAUSE: u32 = 0x48;

    pub const CTRL_L: u32 = 0xE0;
    pub const CTRL_R: u32 = 0xE4;
    pub const SHIFT_L: u32 = 0xE1;
    pub const SHIFT_R: u32 = 0xE5;
    pub const ALT_L: u32 = 0xE2;
    pub const ALT_R: u32 = 0xE6;
    pub const META_L: u32 = 0xE3;
    pub const META_R: u32 = 0xE7;

    /// Is this usage one of the modifier keys?
    pub fn is_modifier(key: u32) -> bool {
        matches!(
            key,
            CTRL_L | CTRL_R | SHIFT_L | SHIFT_R | ALT_L | ALT_R | META_L | META_R
        )
    }
}

/// One keystroke-to-action mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    #[serde(default)]
    pub mods: Mods,
    /// Canonical HID usage id of the non-modifier key.
    pub key: u32,
    /// `switch` (needs `screen`) | `cycle` | `lock` | `home`
    pub action: String,
    /// Target screen name for `switch` (ignored otherwise).
    #[serde(default)]
    pub screen: String,
}

/// The `[shortcuts]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindSection {
    /// Master switch. Off = engine fully inert (every key forwards).
    pub enabled: bool,
    pub bindings: Vec<Binding>,
}

impl Default for BindSection {
    fn default() -> Self {
        Self {
            enabled: true,
            bindings: vec![
                // Scroll Lock alone: cycle to the next screen. (Scroll
                // Lock *with the cursor away* still means "go home":
                // the capture layer's escape intercept runs first.)
                Binding { mods: Mods::NONE, key: hid::SCROLL_LOCK, action: "cycle".into(), screen: String::new() },
                // Pause alone: toggle the wall lock. No application
                // binds it, so a bare press is safe to swallow.
                Binding { mods: Mods::NONE, key: hid::PAUSE, action: "lock".into(), screen: String::new() },
            ],
        }
    }
}

/// How long a half-typed chord lingers before being dropped. A chord
/// whose prefix matched stays armed while its key is held; this window
/// only bounds the abandoned-chord case, so a half-chord never blocks
/// an unrelated later key forever.
const CHORD_LINGER: Duration = Duration::from_secs(5);

/// The engine: owns bindings and the chord-in-progress state.
#[derive(Debug)]
pub struct ActionEngine {
    cfg: BindSection,
    /// Chord in progress: the pending non-modifier key and the mods
    /// held when it went down. `None` = idle.
    pending: Option<PendingChord>,
    /// The key whose press fired a chord: its release is consumed too.
    fired: Option<u32>,
    /// Keys consumed as chord-starts whose chords were abandoned:
    /// their releases are still the engine's (a consumed press must
    /// always own its release, or the client sees a stuck key).
    owned: Vec<u32>,
}

#[derive(Debug)]
struct PendingChord {
    /// The non-modifier key currently held (at most one — a second
    /// non-modifier key abandons the chord: ambiguous).
    key: u32,
    mods: Mods,
    at: Instant,
}

impl ActionEngine {
    pub fn new(cfg: BindSection) -> Self {
        Self { cfg, pending: None, fired: None, owned: Vec::new() }
    }

    pub fn config(&self) -> &BindSection {
        &self.cfg
    }

    /// Adopt a new config section (hot reload). Drops any half-typed
    /// chord — the old prefix may mean nothing under the new bindings.
    pub fn set_config(&mut self, cfg: BindSection) {
        self.pending = None;
        self.cfg = cfg;
        // `owned` survives: those presses were consumed, so their
        // releases stay ours regardless of the new bindings.
    }

    /// A local key went **down**. Returns `Some(action)` when a chord
    /// completes — chords fire on **press** (snappy, no release
    /// dependency, repeat-safe). Keys that start one of the engine's
    /// chords are consumed (held for resolution); everything else passes
    /// through untouched. `at_home` tells the engine whether the cursor
    /// is on the server screen — some actions only mean something away —
    /// but consumption rules are identical in both states, so a chord
    /// never half-leaks to a client.
    pub fn key_down(&mut self, key: u32, mods: Mods, at_home: bool) -> Option<UserAction> {
        if !self.cfg.enabled {
            return None;
        }
        self.tick();
        // Modifiers only refresh a pending chord's mod set; they never
        // fire or start chords, and always pass through.
        if hid::is_modifier(key) {
            if let Some(p) = &mut self.pending {
                p.mods = mods;
            }
            return None;
        }
        // A second non-modifier key while one is pending: ambiguous —
        // abandon the old chord, then evaluate this key normally.
        self.pending = None;

        if let Some(b) = self.cfg.bindings.iter().find(|b| b.key == key && b.mods == mods) {
            self.fired = Some(key);
            return self.fire(b, at_home);
        }
        // Not a match: does this key start any chord? Then swallow and
        // remember — its modifiers may still complete it, and its
        // release must not leak to the client.
        if self.cfg.bindings.iter().any(|b| b.key == key) {
            self.pending = Some(PendingChord { key, mods, at: Instant::now() });
            self.owned.push(key);
        }
        None
    }

    /// A local key went **up**. Returns the key when it belonged to the
    /// engine (a fired chord or a pending one) — the caller must not
    /// forward that release, or the client would see a key stuck down.
    pub fn key_up(&mut self, key: u32) -> Option<u32> {
        if self.fired == Some(key) {
            self.fired = None;
            return Some(key);
        }
        if let Some(pos) = self.owned.iter().position(|&k| k == key) {
            self.owned.remove(pos);
            return Some(key);
        }
        if let Some(p) = &self.pending {
            if p.key == key {
                return Some(key);
            }
        }
        None
    }

    /// Expire a half-typed chord that outlived its window.
    pub fn tick(&mut self) {
        if let Some(p) = &self.pending {
            if p.at.elapsed() > CHORD_LINGER {
                self.pending = None;
            }
        }
    }

    fn fire(&self, spec: &Binding, at_home: bool) -> Option<UserAction> {
        match spec.action.as_str() {
            "switch" if !spec.screen.is_empty() => Some(UserAction::SwitchToScreen { to: spec.screen.clone() }),
            "cycle" => Some(UserAction::SwitchNext),
            "lock" => Some(UserAction::ToggleLock),
            "home" if !at_home => Some(UserAction::GoHome),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
