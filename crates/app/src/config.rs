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

/// The server config file: the virtual desktop layout.
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

    /// A default config describing *this machine only*: the server's own
    /// screen under its real hostname and its real display geometry, and
    /// no invented clients. Clients are admitted dynamically when they
    /// connect and can be pinned into a permanent position from the
    /// Layout page — a machine-accurate starting point beats a sample
    /// that assumes someone else's two-machine desktop.
    pub fn for_this_machine() -> Self {
        let (w, h) = match kvmshare_platform::primary_display() {
            // ScreenInfo carries physical pixels + DPI scale; layout
            // coordinates are logical, so divide (same conversion the
            // running session applies to client reports).
            Some(info) => {
                let s = info.scale.max(0.1);
                ((info.width as f32 / s) as u32, (info.height as f32 / s) as u32)
            }
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