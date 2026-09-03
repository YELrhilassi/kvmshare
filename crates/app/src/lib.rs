//! Shared plumbing for the `kvmshare-server` and `kvmshare-client`
//! executables: configuration, argument parsing, and the server-side
//! clipboard poller.
//!
//! The binaries themselves are thin — one page each — so the interesting
//! logic stays in `kvmshare-core` and `kvmshare-platform`.

pub mod guard;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kvmshare_core::layout::Layout;
use kvmshare_core::server::{Engine, Server};
use kvmshare_core::session::Session;
use kvmshare_protocol::message::{Message, Rect, Screen};

// ---------------------------------------------------------------------------
// Shared defaults
// ---------------------------------------------------------------------------

/// Default listen/connect port, used when a config or address omits one.
pub const DEFAULT_PORT: u16 = 24800;
/// Default screen size for config entries that omit width/height.
pub const DEFAULT_SCREEN_W: u32 = 1920;
pub const DEFAULT_SCREEN_H: u32 = 1080;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The server config file: the virtual desktop layout.
///
/// The **first** screen is always the server's own screen (id 0). The
/// remaining screens are clients, matched by the name a client sends in
/// its `Hello` (by default its hostname). Positions are relative to the
/// server screen: a client to the left has a negative `x`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub screens: Vec<ScreenConfig>,
}

#[derive(Debug, Clone, serde::Deserialize)]
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
        Layout::new(screens)
    }

    /// A default two-machine config, handy for first runs and tests.
    pub fn example() -> Self {
        Self {
            port: DEFAULT_PORT,
            screens: vec![
                ScreenConfig {
                    name: "server".into(),
                    width: DEFAULT_SCREEN_W,
                    height: DEFAULT_SCREEN_H,
                    x: 0,
                    y: 0,
                    scale: 1.0,
                },
                ScreenConfig {
                    name: "client".into(),
                    width: DEFAULT_SCREEN_W,
                    height: DEFAULT_SCREEN_H,
                    x: -(DEFAULT_SCREEN_W as i32),
                    y: 0,
                    scale: 1.0,
                },
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// Command-line arguments (deliberately small; see README for the GUI)
// ---------------------------------------------------------------------------

pub struct ServerArgs {
    pub config: Option<PathBuf>,
    pub port: u16,
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

/// Parse `kvmshare-server [--config PATH] [--port N]`.
pub fn parse_server_args() -> Result<ServerArgs, String> {
    let mut config: Option<PathBuf> = None;
    let mut port: Option<u16> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => config = Some(PathBuf::from(args.next().ok_or("--config needs a path")?)),
            "--port" | "-p" => {
                let raw = args.next().ok_or("--port needs a number")?;
                port = Some(raw.parse().map_err(|_| format!("bad port {raw:?}"))?);
            }
            "--help" | "-h" => {
                println!("usage: kvmshare-server [--config PATH] [--port N]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ServerArgs { config, port: port.unwrap_or(0) })
}

pub struct ClientArgs {
    pub server_addr: String,
    pub name: Option<String>,
}

/// Parse `kvmshare-client SERVER[:PORT] [--name NAME]`.
pub fn parse_client_args() -> Result<ClientArgs, String> {
    let mut addr: Option<String> = None;
    let mut name: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--name" | "-n" => name = Some(args.next().ok_or("--name needs a value")?),
            "--help" | "-h" => {
                println!("usage: kvmshare-client SERVER[:PORT] [--name NAME]");
                std::process::exit(0);
            }
            other if addr.is_none() => addr = Some(other.to_owned()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(ClientArgs { server_addr: addr.ok_or("missing server address")?, name })
}

/// Normalize `host` or `host:port` to `host:port` (default port).
pub fn with_default_port(host: &str, default_port: u16) -> String {
    if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:{default_port}")
    }
}

// ---------------------------------------------------------------------------
// Host name
// ---------------------------------------------------------------------------

/// The machine's host name, used as the default client name.
pub fn hostname() -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(h) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let h = h.trim().to_owned();
        if !h.is_empty() {
            return h;
        }
    }
    "client".into()
}

// ---------------------------------------------------------------------------
// Server clipboard poller
// ---------------------------------------------------------------------------

/// How often the server checks its local clipboard for changes.
const CLIPBOARD_POLL: Duration = Duration::from_millis(500);

/// Watch the local clipboard and broadcast changes to every client.
///
/// Content that arrived *from* a client (already applied to the local
/// clipboard via the engine) is skipped via [`Engine::clipboard_last_injected`],
/// so a copy on a client does not echo back to all clients.
pub fn spawn_server_clipboard(engine: Arc<Mutex<Box<dyn Engine>>>, server: Arc<Server>) {
    thread::spawn(move || {
        let mut last_seen: Option<(String, Vec<u8>)> = None;
        loop {
            thread::sleep(CLIPBOARD_POLL);
            let cur = engine.lock().unwrap().clipboard_get();
            let last_injected = engine.lock().unwrap().clipboard_last_injected();
            if let Some(cur) = cur {
                if last_seen.as_ref() != Some(&cur) && last_injected.as_ref() != Some(&cur) {
                    let (mime, data) = cur.clone();
                    if let Err(e) = server.broadcast(&Message::Clipboard { mime, data }) {
                        eprintln!("clipboard broadcast: {e}");
                    }
                    last_seen = Some(cur);
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Session construction
// ---------------------------------------------------------------------------

/// Build a session from a config, for the server.
pub fn session_from_config(cfg: &Config) -> Session {
    let layout = cfg.to_layout();
    Session::new(layout, 0)
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

    #[test]
    fn client_addr_normalization() {
        assert_eq!(with_default_port("pc", 24800), "pc:24800");
        assert_eq!(with_default_port("pc:1234", 24800), "pc:1234");
        assert_eq!(with_default_port("192.168.1.69", 24800), "192.168.1.69:24800");
    }
}