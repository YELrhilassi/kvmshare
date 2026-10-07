//! The media-control router: which machine receives a media key.
//!
//! # The problem this solves
//!
//! Ordinary keys follow the cursor, and that is correct. Media keys do
//! not: the music may be playing on the other machine while you type
//! here, and moving the cursor just to pause a track defeats the point of
//! a KVM. So media keys are classified at the capture edge (see
//! [`MediaCommand`]) and then **routed by policy**, independently of
//! where the cursor is.
//!
//! # Two categories, because they are two different questions
//!
//! * **Transport** (`PlayPause`, `Next`, `Stop`, ...) acts on the *media
//!   source* — the machine running the player.
//! * **Volume** (`VolumeUp`, `Mute`, ...) acts on the *output you are
//!   listening to* — which, when both machines are audible at once, may
//!   be a different machine entirely.
//!
//! So [`MediaPrefs`] carries one target per category, and each resolves
//! on its own.
//!
//! # The rule that matters most
//!
//! **A target that cannot be resolved falls back to [`ResolvedTarget::Local`]**,
//! which means *do not intercept — let the local OS have the key*. A
//! feature that silently swallows the volume keys when no peer is
//! connected would be a bug, not a feature; [`MediaPrefs::fallback_local`]
//! exists so that behaviour is explicit, and it defaults to on.

use kvmshare_protocol::message::MediaCommand;

/// Where a category's media keys should go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaTarget {
    /// This machine. The key is **not intercepted**: the local OS
    /// receives it exactly as if kvmshare were not running.
    Local,
    /// Wherever the cursor is — the classic behaviour, and the default,
    /// because it changes nothing.
    FollowFocus,
    /// A specific machine, pinned by its stable machine id. Stable across
    /// reconnects and address changes, unlike a screen id.
    Machine(String),
    /// The machine that most recently had audio playing (see
    /// [`crate::media::MediaContext::last_active`]).
    LastActiveSource,
    /// Follow the cursor, but when the cursor is home fall back to the
    /// last machine that made sound.
    FocusOrLastActive,
}

impl Default for MediaTarget {
    fn default() -> Self {
        // Behaviour-preserving: with the default policy, a media key does
        // exactly what it did before this feature existed.
        MediaTarget::FollowFocus
    }
}

impl MediaTarget {
    /// Parse the config-file form. Unknown values are an error rather
    /// than a silent default — a typo in a routing policy should be
    /// visible, not mysterious.
    pub fn parse(text: &str) -> Result<Self, String> {
        let t = text.trim();
        // The pinned form takes an argument, so match it before the
        // fixed names.
        if let Some(id) = t.strip_prefix("machine:") {
            let id = id.trim();
            if id.is_empty() {
                return Err("media target \"machine:\" needs a machine id".into());
            }
            return Ok(MediaTarget::Machine(id.to_string()));
        }
        Ok(match t {
            "local" => MediaTarget::Local,
            "follow_focus" => MediaTarget::FollowFocus,
            "last_active_source" => MediaTarget::LastActiveSource,
            "focus_or_last_active" => MediaTarget::FocusOrLastActive,
            other => {
                return Err(format!(
                    "unknown media target {other:?} (expected local, follow_focus, \
                     last_active_source, focus_or_last_active, or machine:<id>)"
                ))
            }
        })
    }

    /// The config-file form, so what is written back can be re-read.
    pub fn as_str(&self) -> String {
        match self {
            MediaTarget::Local => "local".into(),
            MediaTarget::FollowFocus => "follow_focus".into(),
            MediaTarget::Machine(id) => format!("machine:{id}"),
            MediaTarget::LastActiveSource => "last_active_source".into(),
            MediaTarget::FocusOrLastActive => "focus_or_last_active".into(),
        }
    }
}

/// The `[media]` section: how media keys are routed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPrefs {
    /// Master switch. With this off, capture does not classify at all and
    /// media keys behave exactly as they would without kvmshare
    /// installed — which is what makes the feature safe to ship enabled.
    pub route_media_keys: bool,
    /// Target for playback/transport commands.
    pub transport: MediaTarget,
    /// Target for volume/mute commands.
    pub volume: MediaTarget,
    /// When a target cannot be resolved, send the command to the local
    /// machine instead of dropping it. Off means "swallow the key",
    /// which is rarely what anyone wants; it is configurable because
    /// "rarely" is not "never".
    pub fallback_local: bool,
}

impl Default for MediaPrefs {
    fn default() -> Self {
        Self {
            route_media_keys: true,
            transport: MediaTarget::default(),
            volume: MediaTarget::default(),
            fallback_local: true,
        }
    }
}

/// Where a media command ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedTarget {
    /// Do not intercept: the local OS handles the key natively. Also the
    /// answer for every unresolved target when `fallback_local` is set.
    Local,
    /// Send it to this screen (a connected client).
    Remote(u8),
    /// Swallow the command. Only ever returned when the policy says so
    /// explicitly.
    Drop,
}

/// What the router needs to know about the world.
///
/// Deliberately a borrowed snapshot rather than a live session handle:
/// resolution is then a pure function of state that can be tested
/// exhaustively without a server, a socket, or a machine.
pub struct MediaContext<'a> {
    /// Screen id the cursor is on right now; `None` when it is home on
    /// this machine.
    pub focus: Option<u8>,
    /// Screen id that most recently reported audio playing; `None` when
    /// nothing is playing anywhere.
    pub last_active: Option<u8>,
    /// The user's explicit override, latched by the media-target
    /// shortcut until pressed again. When set it outranks every target
    /// policy — an inference must never outvote a human. `None` = the
    /// configured policy decides.
    pub override_target: Option<u8>,
    /// Resolve a machine id to its screen id, when that machine is
    /// currently connected.
    pub screen_of: &'a dyn Fn(&str) -> Option<u8>,
}

impl<'a> MediaContext<'a> {
    /// A context with nothing connected — the case that must never
    /// swallow a key.
    pub fn alone() -> Self {
        Self { focus: None, last_active: None, override_target: None, screen_of: &|_| None }
    }
}

/// Route one media command.
///
/// Pure: same inputs, same answer. The capture edge uses this to decide
/// whether to consume a key at all ([`ResolvedTarget::Local`] means *let
/// it through untouched*), and the server uses the `Remote` case to
/// address the outbound message.
pub fn resolve(
    prefs: &MediaPrefs,
    command: MediaCommand,
    ctx: &MediaContext<'_>,
) -> ResolvedTarget {
    // The master switch short-circuits everything: with routing off, no
    // key is ever intercepted, whatever the per-category policy says.
    // (An override while routing is off therefore does nothing — routing
    // off means "kvmshare is not here", which is the switch's whole point.)
    if !prefs.route_media_keys {
        return ResolvedTarget::Local;
    }

    // The user's explicit override outranks every category, including the
    // volume/output split: they said where media lives *now*, and both
    // categories follow until the shortcut is pressed again. An override
    // naming a departed machine is cleared by the session, so it can only
    // name a connected screen here — which makes the fallback unreachable
    // in this branch by construction.
    if let Some(id) = ctx.override_target {
        return ResolvedTarget::Remote(id);
    }

    let target = if command.is_volume() {
        &prefs.volume
    } else {
        &prefs.transport
    };

    let resolved = match target {
        MediaTarget::Local => Some(ResolvedTarget::Local),
        MediaTarget::FollowFocus => ctx.focus.map(ResolvedTarget::Remote),
        MediaTarget::Machine(id) => (ctx.screen_of)(id).map(ResolvedTarget::Remote),
        MediaTarget::LastActiveSource => ctx.last_active.map(ResolvedTarget::Remote),
        MediaTarget::FocusOrLastActive => {
            ctx.focus.or(ctx.last_active).map(ResolvedTarget::Remote)
        }
    };

    match resolved {
        Some(r) => r,
        // Unresolved: either hand the key to the local OS, or drop it if
        // the operator explicitly asked for that.
        None => {
            if prefs.fallback_local {
                ResolvedTarget::Local
            } else {
                ResolvedTarget::Drop
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmshare_protocol::message::MediaCommand;

    /// `screen_of` for a context that must outlive a local borrow, so it
    /// is a plain `fn` rather than a closure over locals.
    fn screen_of(id: &str) -> Option<u8> {
        if id == "hp-machine-id" {
            Some(1)
        } else {
            None
        }
    }

    /// A context with `hp` (screen 1) connected and focused.
    fn focused_on_hp(last_active: Option<u8>) -> MediaContext<'static> {
        MediaContext { focus: Some(1), last_active, override_target: None, screen_of: &screen_of }
    }

    #[test]
    fn routing_off_never_intercepts_anything() {
        let prefs = MediaPrefs {
            route_media_keys: false,
            transport: MediaTarget::Machine("hp-machine-id".into()),
            volume: MediaTarget::Machine("hp-machine-id".into()),
            fallback_local: false,
        };
        let ctx = focused_on_hp(Some(1));
        for cmd in MediaCommand::ALL {
            assert_eq!(
                resolve(&prefs, cmd, &ctx),
                ResolvedTarget::Local,
                "{cmd:?} must pass through when routing is off"
            );
        }
    }

    /// The default policy is exactly today's behaviour: media keys go
    /// where the cursor is.
    #[test]
    fn default_policy_follows_the_cursor() {
        let prefs = MediaPrefs::default();
        let ctx = focused_on_hp(None);
        assert_eq!(
            resolve(&prefs, MediaCommand::PlayPause, &ctx),
            ResolvedTarget::Remote(1)
        );

        // Cursor home → local, and never swallowed.
        let home = MediaContext { focus: None, last_active: None, override_target: None, screen_of: &|_| None };
        assert_eq!(
            resolve(&prefs, MediaCommand::PlayPause, &home),
            ResolvedTarget::Local
        );
    }

    /// The headline case: working on one machine, controlling the other.
    #[test]
    fn transport_can_be_pinned_while_working_elsewhere() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::Machine("hp-machine-id".into()),
            volume: MediaTarget::Local,
            fallback_local: true,
        };
        // Cursor is home, but transport still reaches hp...
        let ctx = MediaContext { focus: None, last_active: None, override_target: None, screen_of: &screen_of };
        assert_eq!(
            resolve(&prefs, MediaCommand::Next, &ctx),
            ResolvedTarget::Remote(1)
        );
        // ...while volume stays with the output on this machine.
        assert_eq!(resolve(&prefs, MediaCommand::VolumeUp, &ctx), ResolvedTarget::Local);
        assert_eq!(resolve(&prefs, MediaCommand::Mute, &ctx), ResolvedTarget::Local);
    }

    /// Volume and transport resolve independently, in the same instant.
    #[test]
    fn the_two_categories_are_independent() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::LastActiveSource,
            volume: MediaTarget::FollowFocus,
            fallback_local: true,
        };
        // Focused on screen 2, but sound came from screen 3.
        let ctx = MediaContext { focus: Some(2), last_active: Some(3), override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Remote(3));
        assert_eq!(resolve(&prefs, MediaCommand::VolumeDown, &ctx), ResolvedTarget::Remote(2));
    }

    /// The nice default for real use: follow the cursor, but keep
    /// controlling whatever was last making noise once the cursor is home.
    #[test]
    fn focus_or_last_active_prefers_focus_then_falls_back() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::FocusOrLastActive,
            volume: MediaTarget::FocusOrLastActive,
            fallback_local: true,
        };
        let ctx = MediaContext { focus: Some(2), last_active: Some(3), override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Remote(2));

        // Cursor home: the last active source keeps control, so the user
        // never has to reach for the mouse to pause a track.
        let ctx = MediaContext { focus: None, last_active: Some(3), override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Remote(3));
    }

    /// Last active source with nothing playing anywhere is unresolved →
    /// local, never a swallowed key.
    #[test]
    fn last_active_source_falls_back_to_local_when_silent() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::LastActiveSource,
            volume: MediaTarget::LastActiveSource,
            fallback_local: true,
        };
        let ctx = MediaContext { focus: Some(1), last_active: None, override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Local);
    }

    /// A pinned machine that is not connected cannot receive anything, so
    /// the key goes local rather than vanishing.
    #[test]
    fn pinned_machine_disconnected_falls_back_to_local() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::Machine("some-other-laptop".into()),
            volume: MediaTarget::default(),
            fallback_local: true,
        };
        let ctx = MediaContext { focus: Some(1), last_active: Some(1), override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Local);
    }

    /// ...and drops it only when the operator asked for exactly that.
    #[test]
    fn fallback_off_drops_rather_than_redirecting() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::Machine("disconnected".into()),
            volume: MediaTarget::default(),
            fallback_local: false,
        };
        let ctx = MediaContext { focus: Some(1), last_active: None, override_target: None, screen_of: &|_| None };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Drop);
        // An explicit `local` target is still local — the fallback only
        // governs *unresolved* targets.
        assert_eq!(resolve(&prefs, MediaCommand::VolumeUp, &ctx), ResolvedTarget::Remote(1));
    }

    /// Alone on the network: every command is local, whatever the policy.
    /// This is the case that must never swallow a key.
    #[test]
    fn alone_on_the_network_every_key_is_local() {
        let policies = [
            MediaTarget::Local,
            MediaTarget::FollowFocus,
            MediaTarget::Machine("hp-machine-id".into()),
            MediaTarget::LastActiveSource,
            MediaTarget::FocusOrLastActive,
        ];
        for p in policies {
            let prefs = MediaPrefs {
                route_media_keys: true,
                transport: p.clone(),
                volume: p.clone(),
                fallback_local: true,
            };
            for cmd in MediaCommand::ALL {
                assert_eq!(
                    resolve(&prefs, cmd, &MediaContext::alone()),
                    ResolvedTarget::Local,
                    "{p:?} / {cmd:?}"
                );
            }
        }
    }

    /// The user's explicit override outranks every configured policy —
    /// transport, volume, whatever they said — because it is the answer to
    /// "the router picked wrong".
    #[test]
    fn the_override_outranks_every_policy() {
        let policies = [
            MediaTarget::Local,
            MediaTarget::FollowFocus,
            MediaTarget::Machine("hp-machine-id".into()),
            MediaTarget::LastActiveSource,
            MediaTarget::FocusOrLastActive,
        ];
        for p in policies {
            let prefs = MediaPrefs {
                route_media_keys: true,
                transport: p.clone(),
                volume: p,
                fallback_local: true,
            };
            // Cursor on 2, sound from 3, policy saying whatever it says:
            // the override wins.
            let ctx = MediaContext {
                focus: Some(2),
                last_active: Some(3),
                override_target: Some(1),
                screen_of: &screen_of,
            };
            for cmd in MediaCommand::ALL {
                assert_eq!(resolve(&prefs, cmd, &ctx), ResolvedTarget::Remote(1), "{cmd:?}");
            }
        }
    }

    /// An override never survives routing being switched off: the master
    /// switch means "kvmshare is not here", and an override cannot make a
    /// disabled feature act.
    #[test]
    fn an_override_is_inert_while_routing_is_off() {
        let prefs = MediaPrefs {
            route_media_keys: false,
            ..MediaPrefs::default()
        };
        let ctx = MediaContext {
            focus: None,
            last_active: None,
            override_target: Some(1),
            screen_of: &screen_of,
        };
        assert_eq!(resolve(&prefs, MediaCommand::PlayPause, &ctx), ResolvedTarget::Local);
    }

    #[test]
    fn targets_roundtrip_through_their_config_form() {
        for t in [
            MediaTarget::Local,
            MediaTarget::FollowFocus,
            MediaTarget::Machine("98980a4d9afac273a9aac53ec1c57c35".into()),
            MediaTarget::LastActiveSource,
            MediaTarget::FocusOrLastActive,
        ] {
            assert_eq!(MediaTarget::parse(&t.as_str()), Ok(t.clone()), "{t:?}");
        }
    }

    /// A typo is an error, not a silent fallback to some default that
    /// would route keys somewhere the user did not ask for.
    #[test]
    fn unknown_target_is_a_config_error() {
        assert!(MediaTarget::parse("local_only").is_err());
        assert!(MediaTarget::parse("").is_err());
        assert!(MediaTarget::parse("machine:").is_err());
    }
}
