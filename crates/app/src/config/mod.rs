//! The server config file: the virtual desktop layout and the network
//! policy.
//!
//! Splitting: the model types and their validation live here; file
//! persistence (load/save/atomic write/path resolution) in `io`, and
//! layout geometry (wire layout + local-screen correction) in
//! `geometry`.

mod geometry;
mod ids;
mod io;

pub use ids::set_id;
pub use io::default_config_path;

use std::path::Path;

use kvmshare_core::server::Policy;


/// Default listen/connect port, used when a config or address omits one.
pub const DEFAULT_PORT: u16 = 24800;
/// Default screen size for config entries that omit width/height.
pub const DEFAULT_SCREEN_W: u32 = 1920;
pub const DEFAULT_SCREEN_H: u32 = 1080;

/// The server config file: the virtual desktop layout and the network
/// policy.
///
/// The **first** screen is always the server's own screen (id 0). The
/// remaining screens are clients, matched by the name a client sends in
/// its `Hello` (by default its hostname). Positions are relative to the
/// server screen: a client to the left has a negative `x`.
///
/// A config describes **one machine's role as a server**. It is loaded
/// and hot-reloaded only by that machine's server process, so the
/// server layout never collides with the same machine acting as a
/// client (a client obeys the *remote* server's layout, which it
/// receives on the wire).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub screens: Vec<ScreenConfig>,
    /// Connection policy (`[network]` section). Defaults are applied
    /// when the section is missing, so old configs stay valid.
    #[serde(default)]
    pub network: NetworkConfig,
    /// Keyboard shortcuts (`[shortcuts]` section). Defaults are applied
    /// when the section is missing; see [`kvmshare_core::actions`].
    #[serde(default)]
    pub shortcuts: kvmshare_core::BindSection,
    /// Input feel (`[input]` section): pointer speed, wheel speed,
    /// natural scroll. See [`kvmshare_core::input`].
    #[serde(default)]
    pub input: kvmshare_core::InputPrefs,
    /// Media-key routing (`[media]` section). See
    /// [`kvmshare_core::media`] and `docs/11-media-and-audio.md`.
    #[serde(default)]
    pub media: MediaConfig,
    /// Audio streaming (`[audio]` section).
    #[serde(default)]
    pub audio: AudioConfig,
}

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
}

const fn default_activity_floor() -> f32 {
    -50.0
}

fn default_media_target() -> String {
    kvmshare_core::media::MediaTarget::default().as_str()
}

/// The `[network]` section: who may connect to this server.
///
/// * `allowlist` — only accept clients whose exact name appears in the
///   layout, plus trusted machine ids (default `true`).
/// * `local_only` — only accept connections from the local network
///   (default `true`).
/// * `trusted_ids` — machine ids allowed to connect even when their
///   name is not in the layout yet (they are admitted dynamically on
///   their first connect).
/// * `revoked_ids` — machine ids that may **never** connect. A hard deny:
///   it is checked before the layout and before `trusted_ids`, so a
///   revoked machine cannot get in through a pinned screen. Both lists may
///   name the same id; revoke wins.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NetworkConfig {
    #[serde(default = "default_true")]
    pub allowlist: bool,
    #[serde(default = "default_true")]
    pub local_only: bool,
    #[serde(default)]
    pub trusted_ids: Vec<String>,
    #[serde(default)]
    pub revoked_ids: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            allowlist: true,
            local_only: true,
            trusted_ids: Vec::new(),
            revoked_ids: Vec::new(),
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScreenConfig {
    pub name: String,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default = "default_scale")]
    pub scale: f32,
}

const fn default_port() -> u16 {
    DEFAULT_PORT
}
const fn default_width() -> u32 {
    DEFAULT_SCREEN_W
}
const fn default_height() -> u32 {
    DEFAULT_SCREEN_H
}
const fn default_scale() -> f32 {
    1.0
}

impl Config {
    /// Load and validate a config file. The first screen is the server's
    /// own; it must exist, and names must be unique.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
        if self.screens.is_empty() {
            return Err("config must list at least one screen (the server's own)".into());
        }
        let mut seen = std::collections::HashSet::new();
        for s in &self.screens {
            if s.name.trim().is_empty() {
                return Err("screen names must not be empty".into());
            }
            if !seen.insert(s.name.clone()) {
                return Err(format!("duplicate screen name {:?}", s.name));
            }
            // Geometry sanity: these values become the session's edge
            // math. A zero, negative, or absurd dimension once produced
            // a screen the cursor could enter but never leave; NaN or
            // ±Inf scale would poison every coordinate normalization it
            // touches. Bounds mirror ScreenInfo::MAX_DIMENSION (the wire
            // clamp) so config and handshake agree on what a screen is.
            if s.width == 0 || s.height == 0 {
                return Err(format!("screen {:?} must have non-zero dimensions", s.name));
            }
            let max = kvmshare_protocol::message::ScreenInfo::MAX_DIMENSION;
            if s.width > max || s.height > max {
                return Err(format!(
                    "screen {:?} dimensions exceed the maximum ({max})",
                    s.name
                ));
            }
            if !s.scale.is_finite() || s.scale <= 0.0 || s.scale > 10.0 {
                return Err(format!(
                    "screen {:?} scale must be finite, positive, and at most 10.0 (got {})",
                    s.name, s.scale
                ));
            }
        }
        // Media routing targets are validated here, not at first use: a
        // config that cannot route is a config that should not load.
        self.media.to_prefs()?;
        // The activity floor is compared against a measured level, so a
        // NaN or a level above digital full scale would make the
        // "is anything playing" answer permanently wrong in one
        // direction or the other.
        if !self.audio.activity_floor_db.is_finite()
            || self.audio.activity_floor_db > 0.0
            || self.audio.activity_floor_db < -120.0
        {
            return Err(format!(
                "[audio] activity_floor_db must be finite, at most 0.0 dBFS, and at least -120.0 (got {})",
                self.audio.activity_floor_db
            ));
        }
        Ok(())
    }

    /// The connection policy this config describes, in the form the
    /// running server consumes. One place maps `[network]` → `Policy`, so
    /// the startup path and the hot-reload path can never drift (they did:
    /// the reload only sent the layout, so trust/revoke changes were
    /// ignored until a restart).
    pub fn network_policy(&self) -> Policy {
        Policy {
            allowlist: self.network.allowlist,
            local_only: self.network.local_only,
            trusted_ids: self.network.trusted_ids.clone(),
            revoked_ids: self.network.revoked_ids.clone(),
        }
    }

    /// A default config describing *this machine only*: the server's own
    /// screen under its real hostname and its real display geometry, and
    /// no invented clients. Clients are admitted dynamically when they
    /// connect and can be pinned into a permanent position from the
    /// Layout page — a machine-accurate starting point beats a sample
    /// that assumes someone else's two-machine desktop.
    pub fn for_this_machine() -> Self {
        // The layout lives in the same space the capture beacons and the
        // engine warps use (physical pixels on Windows, root pixels on
        // X11 — where the reported scale is always 1.0), so the walls
        // sit exactly where the real cursor can reach them. The scale in
        // ScreenInfo is informational only.
        let (w, h) = match kvmshare_platform::primary_display() {
            Some(info) => (info.width.max(1), info.height.max(1)),
            None => (DEFAULT_SCREEN_W, DEFAULT_SCREEN_H),
        };
        Self {
            port: DEFAULT_PORT,
            screens: vec![ScreenConfig {
                name: super::hostname::hostname(),
                width: w.max(1),
                height: h.max(1),
                x: 0,
                y: 0,
                scale: 1.0,
            }],
            network: NetworkConfig::default(),
            shortcuts: kvmshare_core::BindSection::default(),
            input: kvmshare_core::InputPrefs::default(),
            media: MediaConfig::default(),
            audio: AudioConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_to_layout() {
        let text = r#"
            port = 24800
            [[screens]]
            name = "pc"
            width = 1920
            height = 1080
            x = 0
            y = 0
            [[screens]]
            name = "hp"
            width = 1920
            height = 1080
            x = -1920
            y = 0
        "#;
        let path = std::env::temp_dir().join("kvmshare-test-config.toml");
        std::fs::write(&path, text).unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(cfg.port, 24800);
        assert_eq!(cfg.screens.len(), 2);
        let layout = cfg.to_layout();
        assert_eq!(layout.screens[0].id, 0);
        assert_eq!(layout.screens[0].name, "pc");
        assert_eq!(layout.screens[1].id, 1);
        assert_eq!(layout.screens[1].name, "hp");
        assert_eq!(layout.screens[1].rect.x, -1920);
        // Network policy defaults to secure.
        assert!(cfg.network.allowlist);
        assert!(cfg.network.local_only);
        assert!(cfg.network.trusted_ids.is_empty());
    }

    #[test]
    fn config_parses_network_section() {
        let text = r#"
            port = 24800
            [[screens]]
            name = "pc"
            [network]
            allowlist = false
            local_only = false
            trusted_ids = ["machine-1", "machine-2"]
        "#;
        let path = std::env::temp_dir().join("kvmshare-test-network.toml");
        std::fs::write(&path, text).unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(!cfg.network.allowlist);
        assert!(!cfg.network.local_only);
        assert_eq!(cfg.network.trusted_ids, vec!["machine-1".to_string(), "machine-2".to_string()]);
        assert!(cfg.network.revoked_ids.is_empty());
    }

    /// `revoked_ids` round-trips through the file and reaches the policy
    /// exactly as written — the list the server enforces.
    #[test]
    fn config_parses_revoked_ids_and_maps_them_to_the_policy() {
        let text = r#"
            port = 24800
            [[screens]]
            name = "pc"
            [network]
            trusted_ids = ["machine-1"]
            revoked_ids = ["machine-bad", "70b97d38"]
        "#;
        let path = std::env::temp_dir().join("kvmshare-test-revoked.toml");
        std::fs::write(&path, text).unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(cfg.network.revoked_ids, vec!["machine-bad".to_string(), "70b97d38".to_string()]);

        let policy = cfg.network_policy();
        assert!(policy.is_revoked("machine-bad"));
        // Short form, prefix match: the full id of the same machine too.
        assert!(policy.is_revoked("70b97d38631dda4b8f6ef627d753022d"));
        assert!(!policy.is_revoked("machine-1"));
        // Both lists coexist; trust is unaffected by revocation.
        assert!(policy.is_trusted("machine-1"));
    }

    #[test]
    fn old_config_without_network_section_stays_valid() {
        let text = r#"
            port = 24800
            [[screens]]
            name = "pc"
        "#;
        let path = std::env::temp_dir().join("kvmshare-test-old.toml");
        std::fs::write(&path, text).unwrap();
        let cfg = Config::load(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(cfg.network.allowlist);
        assert!(cfg.network.local_only);
        assert!(cfg.network.revoked_ids.is_empty(), "an old config has nothing revoked");
    }

    #[test]
    fn config_rejects_duplicate_names() {
        let text = r#"
            port = 24800
            [[screens]]
            name = "pc"
            [[screens]]
            name = "pc"
        "#;
        let path = std::env::temp_dir().join("kvmshare-test-dup.toml");
        std::fs::write(&path, text).unwrap();
        let err = Config::load(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(err.contains("duplicate screen name"), "got: {err}");
    }

    /// Load a config from an inline TOML string.
    fn load_str(name: &str, text: &str) -> Result<Config, String> {
        let path = std::env::temp_dir().join(format!("kvmshare-test-{name}.toml"));
        std::fs::write(&path, text).unwrap();
        let result = Config::load(&path);
        std::fs::remove_file(&path).ok();
        result
    }

    /// A config with no `[media]`/`[audio]` sections keeps working, and
    /// its defaults change nothing: media keys follow the cursor, exactly
    /// as they did before the feature existed.
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