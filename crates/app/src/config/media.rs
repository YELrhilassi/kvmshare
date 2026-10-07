//! The `[media]` and `[audio]` config sections.
//!
//! These two travel together because the GUI edits them on one page and
//! the client's own config file consists of exactly one of them. The
//! wire strings are owned by `kvmshare_core::media` and the runtime
//! options by `kvmshare_core::audio::runtime`; this module is the
//! file-shape mapping plus validation, so the startup path and the hot
//! reload can never disagree about what a section means.

use super::default_true;

/// The `[media]` section: where playback and volume keys go.
///
/// Targets are stored as strings rather than enums so the file is
/// readable and hand-editable, and so a typo produces a parse error
/// instead of silently taking a default: a routing policy that quietly
/// does something other than what it says would be worse than one that
/// refuses to load.
///
/// Defaults change **nothing**: with `follow_focus` (the default for
/// both categories) a media key goes exactly where it went before this
/// feature existed. The richer policies are opt-in.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MediaConfig {
    /// Master switch. `false` = media keys are never intercepted.
    #[serde(default = "default_true")]
    pub route_media_keys: bool,
    /// Where play/pause/next/previous/stop/seek go.
    #[serde(default = "default_media_target")]
    pub transport: String,
    /// Where volume/mute go — the *output* the user is listening to,
    /// which can be a different machine from the media source.
    #[serde(default = "default_media_target")]
    pub volume: String,
    /// Hand an unresolvable target to the local machine instead of
    /// swallowing the key.
    #[serde(default = "default_true")]
    pub fallback_local: bool,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            route_media_keys: true,
            transport: default_media_target(),
            volume: default_media_target(),
            fallback_local: true,
        }
    }
}

impl MediaConfig {
    /// The routing policy in the form the capture edge and the server
    /// consume. Parsing lives here (not at the call site) so the running
    /// server and the hot-reload path can never disagree about what the
    /// file means — the same reason [`Config::network_policy`] exists.
    pub fn to_prefs(&self) -> Result<kvmshare_core::media::MediaPrefs, String> {
        Ok(kvmshare_core::media::MediaPrefs {
            route_media_keys: self.route_media_keys,
            transport: parse_target("transport", &self.transport)?,
            volume: parse_target("volume", &self.volume)?,
            fallback_local: self.fallback_local,
        })
    }

    /// Write a policy back in config form (GUI edits round-trip through
    /// the file, so what is shown is what will be loaded).
    pub fn from_prefs(prefs: &kvmshare_core::media::MediaPrefs) -> Self {
        Self {
            route_media_keys: prefs.route_media_keys,
            transport: prefs.transport.as_str(),
            volume: prefs.volume.as_str(),
            fallback_local: prefs.fallback_local,
        }
    }
}

/// Parse one routing target, naming the field in the error so a long
/// config file points at the offending line.
fn parse_target(field: &str, text: &str) -> Result<kvmshare_core::media::MediaTarget, String> {
    kvmshare_core::media::MediaTarget::parse(text)
        .map_err(|e| format!("[media] {field}: {e}"))
}

/// The `[audio]` section: carry this machine's output to the peer, play
/// the peer's, both, or neither.
///
/// Off by default, and deliberately so: audio capture reads what the
/// machine is playing, and nothing should be captured or transmitted
/// until a human says so.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioConfig {
    /// Send this machine's output to the peer.
    #[serde(default)]
    pub send: bool,
    /// Play what the peer sends.
    #[serde(default)]
    pub receive: bool,
    /// Device to capture. Empty or `default` = the system default output
    /// (its loopback is captured — never a microphone).
    #[serde(default)]
    pub capture_device: String,
    /// Device to play the peer's audio to. Empty or `default` = the
    /// system default output.
    #[serde(default)]
    pub playback_device: String,
    /// Level below which audio counts as silence, in dBFS. Drives the
    /// "is this machine playing something" answer that the media
    /// router's `last_active_source` policy needs.
    #[serde(default = "default_activity_floor")]
    pub activity_floor_db: f32,
    /// Which machine to share audio with, as a machine id (a prefix is
    /// accepted, like the trust policy).
    ///
    /// Empty means "the only connected client" — an audio link describes
    /// one pair of machines, so with more than one client the choice stops
    /// being obvious, and the link is dropped rather than streaming this
    /// machine's output to a machine the user never picked.
    ///
    /// Read only where the choice has to be made — the **server** role.
    /// A client has exactly one peer (the server that admitted it), so its
    /// copy of `[audio]` leaves this unset and it is ignored there.
    #[serde(default)]
    pub peer: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            send: false,
            receive: false,
            capture_device: String::new(),
            playback_device: String::new(),
            activity_floor_db: default_activity_floor(),
            peer: String::new(),
        }
    }
}

impl AudioConfig {
    /// The running audio settings, in the form the runtime consumes. One
    /// mapping, so the startup path and a hot reload cannot drift.
    pub fn to_options(&self) -> kvmshare_core::audio::runtime::AudioOptions {
        kvmshare_core::audio::runtime::AudioOptions {
            send: self.send,
            receive: self.receive,
            capture_device: self.capture_device.clone(),
            playback_device: self.playback_device.clone(),
            activity_floor_db: self.activity_floor_db,
        }
    }

    /// Whether this section asks for anything at all.
    pub fn is_active(&self) -> bool {
        self.send || self.receive
    }

    /// Reject values that cannot be honoured.
    ///
    /// The activity floor is compared against a measured level, so a NaN or
    /// a value above digital full scale would make the "is anything
    /// playing" answer permanently wrong in one direction or the other.
    /// Both roles run this, because both roles run the same pipeline.
    pub fn validate(&self) -> Result<(), String> {
        if !self.activity_floor_db.is_finite()
            || self.activity_floor_db > 0.0
            || self.activity_floor_db < -120.0
        {
            return Err(format!(
                "[audio] activity_floor_db must be finite, at most 0.0 dBFS, and at least -120.0 (got {})",
                self.activity_floor_db
            ));
        }
        Ok(())
    }
}

const fn default_activity_floor() -> f32 {
    -50.0
}

fn default_media_target() -> String {
    kvmshare_core::media::MediaTarget::default().as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// Load a config from an inline TOML string (the same fixture shape
    /// the parent module's tests use).
    fn load_str(name: &str, text: &str) -> Result<Config, String> {
        let path = std::env::temp_dir().join(format!("kvmshare-test-{name}.toml"));
        std::fs::write(&path, text).unwrap();
        let result = Config::load(&path);
        std::fs::remove_file(&path).ok();
        result
    }

    #[test]
    fn missing_media_and_audio_sections_are_inert() {
        let cfg = load_str("inert", "port = 24800\n[[screens]]\nname = \"pc\"\n").unwrap();
        let prefs = cfg.media.to_prefs().unwrap();
        assert!(prefs.route_media_keys, "routing is on but conservative");
        assert_eq!(prefs.transport, kvmshare_core::media::MediaTarget::FollowFocus);
        assert_eq!(prefs.volume, kvmshare_core::media::MediaTarget::FollowFocus);
        assert!(prefs.fallback_local, "a media key is never swallowed by default");
        assert!(!cfg.audio.send, "audio capture is off until asked for");
        assert!(!cfg.audio.receive);
    }

    /// The headline configuration: keep the media keys on one machine
    /// while working on another, over a pinned machine id.
    #[test]
    fn media_section_routes_categories_independently() {
        let cfg = load_str(
            "media",
            r#"
            port = 24800
            [[screens]]
            name = "pc"
            [media]
            transport = "machine:98980a4d9afac273a9aac53ec1c57c35"
            volume = "local"
        "#,
        )
        .unwrap();
        let prefs = cfg.media.to_prefs().unwrap();
        assert_eq!(
            prefs.transport,
            kvmshare_core::media::MediaTarget::Machine(
                "98980a4d9afac273a9aac53ec1c57c35".to_string()
            )
        );
        assert_eq!(prefs.volume, kvmshare_core::media::MediaTarget::Local);
    }

    /// A typo in a routing target refuses to load. Silently routing keys
    /// somewhere the user did not ask for is worse than an error.
    #[test]
    fn an_unknown_media_target_fails_validation() {
        let err = load_str(
            "badtarget",
            "port = 24800\n[[screens]]\nname = \"pc\"\n[media]\ntransport = \"remote\"\n",
        )
        .unwrap_err();
        assert!(err.contains("[media] transport"), "got: {err}");
        assert!(err.contains("unknown media target"), "got: {err}");
    }

    /// The policy survives a round trip out to config form and back, so
    /// the GUI can write what it shows.
    #[test]
    fn media_prefs_round_trip_through_config() {
        let prefs = kvmshare_core::media::MediaPrefs {
            route_media_keys: false,
            transport: kvmshare_core::media::MediaTarget::LastActiveSource,
            volume: kvmshare_core::media::MediaTarget::FocusOrLastActive,
            fallback_local: false,
        };
        assert_eq!(MediaConfig::from_prefs(&prefs).to_prefs().unwrap(), prefs);
    }

    /// An impossible activity floor is rejected: the value is compared
    /// against a measured level, so a NaN makes the comparison always
    /// false and the "is playing" answer permanently wrong.
    #[test]
    fn an_impossible_activity_floor_is_rejected() {
        for floor in ["f32::NAN", "1.0", "-200.0"] {
            let value = match floor {
                "f32::NAN" => "nan".to_string(),
                other => other.to_string(),
            };
            let err = load_str(
                "badfloor",
                &format!(
                    "port = 24800\n[[screens]]\nname = \"pc\"\n[audio]\nactivity_floor_db = {value}\n"
                ),
            )
            .unwrap_err();
            assert!(err.contains("activity_floor_db"), "{floor} got: {err}");
        }
    }

    /// A usable audio floor loads and reaches the config unchanged.
    #[test]
    fn audio_section_parses() {
        let cfg = load_str(
            "audio",
            r#"
            port = 24800
            [[screens]]
            name = "pc"
            [audio]
            send = true
            receive = true
            capture_device = "alsa_output.pci-0000_00_1f.3.analog-stereo"
            activity_floor_db = -45.0
        "#,
        )
        .unwrap();
        assert!(cfg.audio.send);
        assert!(cfg.audio.receive);
        assert_eq!(cfg.audio.capture_device, "alsa_output.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(cfg.audio.activity_floor_db, -45.0);
    }
}
