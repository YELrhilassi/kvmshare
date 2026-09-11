//! The server config file: the virtual desktop layout and the network
//! policy.
//!
//! Splitting: the model types and their validation live here; file
//! persistence (load/save/atomic write/path resolution) in `io`, and
//! layout geometry (wire layout + local-screen correction) in
//! `geometry`.

mod geometry;
mod io;

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
}