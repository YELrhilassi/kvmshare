//! The kvmshare server.
//!
//! The machine whose keyboard and mouse are shared. Owns the virtual
//! desktop layout, listens for clients, and forwards local input to
//! whichever client the cursor is on.

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use kvmshare_app::guard::{self, RoleGuard};
use kvmshare_app::log::{self, Level};
use kvmshare_app::{
    default_config_path, log_info, log_warn, parse_server_args, session_from_config, spawn_server_clipboard, Config,
};
use kvmshare_core::server::{Control, Server};

fn main() {
    if let Err(e) = run() {
        log::write_line(Level::Error, format_args!("{e}"));
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_server_args()?;
    log::init(&args.log_level.unwrap_or_else(log::level_from_env_or_default))?;

    // One role per machine, enforced at the OS level: refuse to start if
    // a client is running here, and hold our own lock for the process
    // lifetime (flock dies with us — no orphans).
    let _guard: RoleGuard = guard::acquire(guard::ROLE_SERVER)?;

    // Config → layout → session (the switching brain). When no --config
    // is given, fall back to the standard locations.
    let config_path = args.config.unwrap_or_else(default_config_path);
    let cfg = Config::load(&config_path)?;
    let port = if args.port != 0 { args.port } else { cfg.port };
    log_info!(
        "layout {} screens (local: {}), listening on :{port}",
        cfg.screens.len(),
        cfg.screens[0].name
    );

    // Platform: capture local input + control the local cursor.
    let (input, engine) = kvmshare_platform::server(None).map_err(|e| format!("platform: {e}"))?;
    let engine = Arc::new(Mutex::new(engine));

    let (ctl_tx, ctl_rx) = mpsc::channel();
    let server = Arc::new(
        Server::with_control(session_from_config(&cfg), port, Some(ctl_rx))
            .map_err(|e| format!("bind: {e}"))?,
    );

    // Clipboard: local changes are broadcast to every client.
    spawn_server_clipboard(engine.clone(), server.clone());

    // Config hot-reload: watch the file and adopt changes live, without
    // a restart (the GUI saves the config while the server keeps running).
    spawn_config_watcher(config_path, ctl_tx);

    // Run forever, forwarding local input.
    server.run(input, engine).map_err(|e| format!("server: {e}"))
}

/// How often the config watcher polls the file for changes.
const CONFIG_WATCH_POLL: Duration = Duration::from_millis(600);

/// Poll the config file and push a [`Control::Reload`] whenever its
/// content changes, so layout edits apply live. A transient parse error
/// (file mid-write) just defers the reload until the file is valid.
fn spawn_config_watcher(path: PathBuf, tx: mpsc::Sender<Control>) {
    thread::spawn(move || {
        let mut last: Option<(SystemTime, u64)> = None;
        loop {
            thread::sleep(CONFIG_WATCH_POLL);
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue, // config missing: keep the old one
            };
            let sig = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            if last == Some(sig) {
                continue;
            }
            match Config::load(&path) {
                Ok(cfg) => {
                    last = Some(sig);
                    if tx.send(Control::Reload(cfg.to_layout())).is_err() {
                        return; // server gone
                    }
                }
                Err(e) => log_warn!("config reload deferred: {e}"),
            }
        }
    });
}