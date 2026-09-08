//! The server config file: the virtual desktop layout, plus the shared
//! defaults and the atomic file writer.

use std::path::{Path, PathBuf};

use kvmshare_core::layout::Layout;
use kvmshare_protocol::message::{Rect, Screen};

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NetworkConfig {
    #[serde(default = "default_true")]
    pub allowlist: bool,
    #[serde(default = "default_true")]
    pub local_only: bool,
    #[serde(default)]
    pub trusted_ids: Vec<String>,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { allowlist: true, local_only: true, trusted_ids: Vec::new() }
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

    /// Build the wire layout: id 0 is the server, then 1..n in order.
    pub fn to_layout(&self) -> Layout {
        let screens = self
            .screens
            .iter()
            .enumerate()
            .map(|(id, s)| Screen {
                id: id as u8,
                name: s.name.clone(),
                rect: Rect {
                    x: s.x,
                    y: s.y,
                    w: s.width as i32,
                    h: s.height as i32,
                },
            })
            .collect();
        // Normalize: snap near-adjacent screens into exact contact and
        // align their perpendicular spans, so crossings land pixel-exact
        // regardless of how the GUI's canvas positioned the screens.
        Layout::new(screens).normalized()
    }

    /// Correct the server's own screen (the first entry, id 0) to the
    /// platform's real display geometry, and shift screens that were
    /// placed against the old (wrong) right/bottom edges by the edge
    /// delta so they stay adjacent.
    ///
    /// The local screen's size is not a user choice — it is this
    /// machine's display. A stale config (an old default, or the display
    /// changed since) leaves the boundary walls far from the real cursor
    /// space: the capture beacons the *physical* cursor position, and a
    /// wall the real cursor can never reach can never be armed — so the
    /// cursor cannot cross to the neighbor at all. Correcting only the
    /// size is not enough: a neighbor placed against the old right edge
    /// would then sit in a gap. Screens wholly right of the old right
    /// edge (or below the old bottom edge) shift by exactly the edge
    /// delta — their spacing is preserved, they just follow the edge
    /// they were built against. Screens left of / above the local screen
    /// anchor to the unchanged left/top edge and never move.
    ///
    /// Returns whether the config changed. No-op when the platform
    /// cannot report a display or the size already matches. Called at
    /// every server start; after the first correction the config is
    /// right, so it stops changing.
    pub fn correct_local_screen(&mut self) -> bool {
        match kvmshare_platform::primary_display() {
            Some(info) => self.correct_local_screen_to(info.width.max(1), info.height.max(1)),
            None => false,
        }
    }

    /// The pure part of [`Config::correct_local_screen`], testable
    /// without a platform.
    fn correct_local_screen_to(&mut self, w: u32, h: u32) -> bool {
        let Some(local) = self.screens.first_mut() else { return false };
        if local.width == w && local.height == h {
            return false;
        }
        let (ow, oh) = (local.width, local.height);
        local.width = w;
        local.height = h;
        let (dx, dy) = (w as i32 - ow as i32, h as i32 - oh as i32);
        for s in self.screens.iter_mut().skip(1) {
            if s.x >= ow as i32 {
                s.x += dx;
            }
            if s.y >= oh as i32 {
                s.y += dy;
            }
        }
        true
    }

    /// Persist this config to `path` atomically (temp file + rename), so
    /// a concurrent reader (the hot-reload watcher, the GUI) never sees
    /// a torn write.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| format!("encode config: {e}"))?;
        atomic_write(path, &text)
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

    /// Load the config at `path`, creating a machine-accurate default
    /// ([`Config::for_this_machine`]) when nothing is there yet. The
    /// caller learns whether the file was created so it can say so.
    ///
    /// This is the only path a *first* server start goes through: a
    /// missing config is never an error (the server would otherwise die
    /// on a file it was about to create) and never a stale copy of
    /// another machine's layout.
    pub fn load_or_create(path: &Path) -> Result<(Self, bool), String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let cfg: Config = toml::from_str(&text)
                    .map_err(|e| format!("parse {}: {e}", path.display()))?;
                cfg.validate()?;
                Ok((cfg, false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::for_this_machine();
                let text = toml::to_string_pretty(&cfg)
                    .map_err(|e| format!("encode default config: {e}"))?;
                atomic_write(path, &text)?;
                Ok((cfg, true))
            }
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    }
}

/// Write `text` to `path` atomically (temp file + rename), creating
/// parent directories. The running server watches this file, so it must
/// never observe a torn write.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))
}

/// Where the server looks for its config when `--config` is not given:
/// the `KVMSHARE_CONFIG` env var, then `~/.config/kvmshare/`, then the
/// current directory.
pub fn default_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("KVMSHARE_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home).join(".config/kvmshare/kvmshare-server.toml");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("kvmshare-server.toml")
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
    }

    // A stale local screen size (old default, display changed) is the
    // classic "cursor cannot cross" bug: the capture beacons the real
    // cursor position, and a layout wall the cursor can never reach is
    // never armed. The correction resizes the local screen to reality
    // and shifts neighbors placed against the old edges so they stay
    // adjacent.
    #[test]
    fn correct_local_screen_resizes_and_keeps_neighbors_adjacent() {
        let mut cfg = Config {
            port: 24800,
            screens: vec![
                ScreenConfig { name: "HP".into(), width: 1920, height: 1080, x: 0, y: 0, scale: 1.0 },
                ScreenConfig { name: "pc".into(), width: 3840, height: 1080, x: 1920, y: 0, scale: 1.0 },
                ScreenConfig { name: "left".into(), width: 1920, height: 1080, x: -1920, y: 0, scale: 1.0 },
                ScreenConfig { name: "below".into(), width: 1920, height: 1080, x: 0, y: 1080, scale: 1.0 },
            ],
            network: NetworkConfig::default(),
        };

        assert!(cfg.correct_local_screen_to(1024, 768), "a stale size must be corrected");
        // Local screen now matches reality.
        assert_eq!((cfg.screens[0].width, cfg.screens[0].height), (1024, 768));
        // The right neighbor was adjacent to the old right edge (1920);
        // it follows the corrected edge (1024) — still adjacent, gap
        // preserved.
        assert_eq!(cfg.screens[1].x, 1024);
        // The left neighbor anchors to the unchanged left edge: unmoved.
        assert_eq!(cfg.screens[2].x, -1920);
        // The screen below the old bottom edge (1080) follows the new
        // bottom edge (768).
        assert_eq!(cfg.screens[3].y, 768);

        // A second pass is a no-op: the config is already correct.
        assert!(!cfg.correct_local_screen_to(1024, 768), "a correct config must not change");
        assert_eq!(cfg.screens[1].x, 1024);
    }

    // The correction must never move a screen that sits inside the old
    // local rect (an overlapping config) — only screens placed against
    // the old edges follow them.
    #[test]
    fn correct_local_screen_leaves_overlapping_screens_alone() {
        let mut cfg = Config {
            port: 24800,
            screens: vec![
                ScreenConfig { name: "HP".into(), width: 1920, height: 1080, x: 0, y: 0, scale: 1.0 },
                ScreenConfig { name: "overlap".into(), width: 800, height: 600, x: 1500, y: 200, scale: 1.0 },
            ],
            network: NetworkConfig::default(),
        };
        assert!(cfg.correct_local_screen_to(1024, 768));
        assert_eq!(cfg.screens[1].x, 1500, "an overlapping screen must not shift");
        assert_eq!(cfg.screens[1].y, 200);
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