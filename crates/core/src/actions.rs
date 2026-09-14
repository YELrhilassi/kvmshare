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
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
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

/// The smallest HID usage a binding may name. Everything below it is
/// error/undefined space in the USB HID usage tables; the modifiers live
/// at 0xE0–0xE7. Recording and loading both gate on this.
const MIN_BINDABLE_KEY: u32 = 0x04;

/// One keystroke-to-action mapping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    #[serde(default)]
    pub mods: Mods,
    /// Canonical HID usage id of the non-modifier key.
    ///
    /// Also accepts an *integral* float (`71.0`): the GUI round-trips
    /// this file through JavaScript, where every number is an f64, and
    /// a naive writer can emit `key = 71.0`. Strictly rejecting that
    /// made serde drop the **entire** `[shortcuts]` section — every
    /// binding silently vanished. A genuinely fractional value is
    /// still an error.
    #[serde(deserialize_with = "u32_or_integral_float")]
    pub key: u32,
    /// `switch` (needs `screen`) | `cycle` | `lock` | `home`
    pub action: String,
    /// Target screen name for `switch` (ignored otherwise).
    #[serde(default)]
    pub screen: String,
}

/// Deserialize a `u32` from any integral number encoding.
fn u32_or_integral_float<'de, D: serde::Deserializer<'de>>(de: D) -> Result<u32, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = u32;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an integer HID usage id (an integral float is accepted)")
        }
        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| serde::de::Error::invalid_value(serde::de::Unexpected::Unsigned(v), &self))
        }
        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u32, E> {
            u32::try_from(v).map_err(|_| serde::de::Error::invalid_value(serde::de::Unexpected::Signed(v), &self))
        }
        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<u32, E> {
            if v.is_finite() && v >= 0.0 && v.fract() == 0.0 && v <= f64::from(u32::MAX) {
                Ok(v as u32)
            } else {
                Err(serde::de::Error::invalid_value(serde::de::Unexpected::Float(v), &self))
            }
        }
    }
    de.deserialize_u32(V)
}

/// The `[shortcuts]` config section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BindSection {
    /// Master switch. Off = engine fully inert (every key forwards).
    pub enabled: bool,
    pub bindings: Vec<Binding>,
}

impl BindSection {
    /// Drop every binding a user must never have meant, and log what was
    /// dropped. See [`Binding::is_bindable`] for the exact rule.
    ///
    /// Why the engine sanitizes and not just the GUI: the config file is
    /// a written artifact — an earlier GUI release (or a hand edit) can
    /// leave a binding like bare Tab in it, and the engine must stay
    /// correct against the file that exists, not the file the current
    /// GUI would write. A bad binding must never cost the user their
    /// keyboard; the whole section silently failing to load (what strict
    /// validation would do here) is exactly that.
    pub fn sanitized(mut self) -> Self {
        self.bindings.retain(|b| {
            if b.is_bindable() {
                true
            } else {
                kvmshare_log::log_warn!(
                    "shortcut binding for key 0x{:02x} is unsafe (bare or out of range) and was ignored — record it again with a modifier in the GUI",
                    b.key
                );
                false
            }
        });
        self
    }
}

impl Binding {
    /// Would swallowing this chord be safe? A binding must name a real
    /// key (everything under [`MIN_BINDABLE_KEY`] is error space), and
    /// either carry at least one modifier **or** name a key whose bare
    /// press carries no application meaning (see [`Binding::bare_safe`]).
    /// The keyboard's workhorse keys — Tab, letters, digits, Space,
    /// Enter — keep their meaning in every application, and a binding
    /// without modifiers swallows one of them system-wide: the engine
    /// consumes a bound key before apps see it (both by design and, on
    /// X11, via a passive grab). The Scroll Lock escape stays available
    /// regardless: it is wired in the capture layer, outside this
    /// config.
    pub fn is_bindable(&self) -> bool {
        self.key >= MIN_BINDABLE_KEY
            && !hid::is_modifier(self.key)
            && (self.mods.any() || Self::bare_safe(self.key))
    }

    /// Keys whose **bare** press no mainstream application acts on, so
    /// binding one without modifiers can never eat a keystroke the user
    /// meant for an app. The default chords are built from this set
    /// (Scroll Lock, Pause); F13–F24 join them because nothing binds
    /// them. Everything else needs a modifier.
    fn bare_safe(key: u32) -> bool {
        matches!(key, hid::SCROLL_LOCK | hid::PAUSE | 0x68..=0x73)
    }
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

impl Mods {
    /// Update the mod set from one key transition. Modifier keys are
    /// tracked by the engine itself (the session feeds it every key
    /// event), so the state is authoritative even when apps change
    /// focus mid-chord. `down` distinguishes press from release.
    pub fn apply_key(&mut self, key: u32, down: bool) {
        let bit: Option<&mut bool> = match key {
            hid::CTRL_L | hid::CTRL_R => Some(&mut self.ctrl),
            hid::SHIFT_L | hid::SHIFT_R => Some(&mut self.shift),
            hid::ALT_L | hid::ALT_R => Some(&mut self.alt),
            hid::META_L | hid::META_R => Some(&mut self.meta),
            _ => None,
        };
        if let Some(flag) = bit {
            *flag = down;
        }
    }

    /// Is any modifier in this set held?
    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.meta
    }
}

/// The engine: owns bindings and the chord-in-progress state.
#[derive(Debug)]
pub struct ActionEngine {
    cfg: BindSection,
    /// Live modifier state, maintained from the key stream itself.
    mods: Mods,
    /// Chord in progress: the pending non-modifier key and the mods
    /// held when it went down. `None` = idle.
    pending: Option<PendingChord>,
    /// The key whose press fired a chord: its release is consumed too.
    fired: Option<u32>,
    /// Keys consumed as chord-starts whose chords were abandoned:
    /// their releases are still the engine's (a consumed press must
    /// always own its release, or the client sees a stuck key).
    owned: Vec<u32>,
    /// The last chord-shaped key press seen (mods + key), bound or not
    /// — powers the GUI's live key detection display.
    last_seen: Option<(Mods, u32)>,
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
        Self::new_sanitized(cfg.sanitized())
    }

    /// The constructor after sanitization — exposed for tests that need
    /// to install a known-shape config directly.
    pub(crate) fn new_sanitized(cfg: BindSection) -> Self {
        Self {
            cfg,
            mods: Mods::NONE,
            pending: None,
            fired: None,
            owned: Vec::new(),
            last_seen: None,
        }
    }

    pub fn config(&self) -> &BindSection {
        &self.cfg
    }

    /// Adopt a new config section (hot reload). Drops any half-typed
    /// chord — the old prefix may mean nothing under the new bindings.
    /// Incoming bindings pass the same safety gate as startup (see
    /// [`BindSection::sanitized`]): a config edit must never arm a
    /// bare-Tab swallow mid-session either.
    pub fn set_config(&mut self, cfg: BindSection) {
        self.pending = None;
        self.cfg = cfg.sanitized();
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
    ///
    /// Modifier state is tracked internally from the key stream; the
    /// caller's `mods` argument is only a hint for backends that deliver
    /// a chord as one event with no modifier history (an empty hint is
    /// always safe).
    pub fn key_down(&mut self, key: u32, mods: Mods, at_home: bool) -> Option<UserAction> {
        if !self.cfg.enabled {
            return None;
        }
        self.tick();
        // Modifiers only update the live set; they never fire or start
        // chords, and always pass through.
        if hid::is_modifier(key) {
            self.mods.apply_key(key, true);
            if let Some(p) = &mut self.pending {
                p.mods = self.mods;
            }
            return None;
        }
        // A second non-modifier key while one is pending: ambiguous —
        // abandon the old chord, then evaluate this key normally.
        self.pending = None;

        // The effective chord: the engine's tracked modifiers, or the
        // caller's hint when the engine saw nothing (a backend that
        // delivers one event with mods pre-resolved).
        let effective = if self.mods.any() { self.mods } else { mods };
        self.last_seen = Some((effective, key));

        if let Some(b) = self.cfg.bindings.iter().find(|b| b.key == key && b.mods == effective) {
            self.fired = Some(key);
            return self.fire(b, at_home);
        }
        // Not a match: does this key start any chord? Then swallow and
        // remember — its modifiers may still complete it, and its
        // release must not leak to the client.
        if self.cfg.bindings.iter().any(|b| b.key == key) {
            self.pending = Some(PendingChord { key, mods: effective, at: Instant::now() });
            self.owned.push(key);
        }
        None
    }

    /// A local key went **up**. Returns the key when it belonged to the
    /// engine (a fired chord or a pending one) — the caller must not
    /// forward that release, or the client would see a key stuck down.
    pub fn key_up(&mut self, key: u32) -> Option<u32> {
        if hid::is_modifier(key) {
            self.mods.apply_key(key, false);
            return None;
        }
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

    /// The last chord-shaped key press (mods + HID key), bound or not —
    /// live key detection for the GUI.
    pub fn last_seen(&self) -> Option<(Mods, u32)> {
        self.last_seen
    }

    /// Would this (mods, key) pair fire a binding right now?
    pub fn is_bound(&self, mods: Mods, key: u32) -> bool {
        self.cfg.bindings.iter().any(|b| b.key == key && b.mods == mods)
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
