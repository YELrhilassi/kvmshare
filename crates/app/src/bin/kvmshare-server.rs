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
use kvmshare_app::{
    default_config_path, machine_id, parse_server_args, session_from_config, spawn_server_clipboard,
    state_dir, Config,
};
use kvmshare_core::server::{Control, Options, Policy, Server, ServerEvent};
use kvmshare_log::{log_error, log_info, log_warn};
use kvmshare_protocol::message::ScreenInfo;

fn main() {
    if let Err(e) = run() {
        log_error!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_server_args()?;
    kvmshare_log::init(
        &args.log_level.unwrap_or_else(kvmshare_log::level_from_env_or_default),
        args.log_ctl,
    )?;
    // This role drives the user's cursor on a millisecond cadence; it
    // must be scheduled above whatever else the machine is doing.
    kvmshare_platform::raise_priority();

    // One role per machine, enforced at the OS level: refuse to start if
    // a client is running here, and hold our own lock for the process
    // lifetime (flock dies with us — no orphans).
    let _guard: RoleGuard = guard::acquire(guard::ROLE_SERVER)?;

    // Config → layout → session (the switching brain). When no --config
    // is given, fall back to the standard locations. A config that does
    // not exist yet is never an error: the server creates a
    // machine-accurate default (this machine's real name + display, no
    // invented clients) and says so — a server must never die on a file
    // it is about to create, and never inherit some other machine's
    // layout. The Layout page is where the user pins clients in
    // permanently; until then each client is admitted dynamically on
    // its first connect.
    let config_path = args.config.unwrap_or_else(default_config_path);
    let (mut cfg, created) = Config::load_or_create(&config_path)?;
    // The local screen's size is this machine's display, not a choice.
    // A stale config (an old default, or the display changed) leaves the
    // boundary walls far from the real cursor space, so the cursor can
    // never cross to a neighbor. Correct it to the platform's real
    // geometry and persist, so the GUI's Layout page shows the truth and
    // the next start agrees. Neighbors placed against the old edges are
    // shifted to stay adjacent (see Config::correct_local_screen).
    if cfg.correct_local_screen() {
        let local = &cfg.screens[0];
        log_info!(
            "corrected local screen {:?} to {}x{} from the platform's display geometry (neighbors kept adjacent)",
            local.name,
            local.width,
            local.height
        );
        if let Err(e) = cfg.save(&config_path) {
            log_warn!("could not persist corrected local screen size: {e}");
        }
    }
    let port = if args.port != 0 { args.port } else { cfg.port };
    log_info!(
        "layout {} screens (local: {}), listening on :{port}",
        cfg.screens.len(),
        cfg.screens[0].name
    );
    if created {
        log_info!(
            "no layout at {} — created a default describing this machine only; clients are added dynamically on first connect and can be pinned in the Layout page",
            config_path.display()
        );
    }

    // Platform: capture local input + control the local cursor, and the
    // standalone clipboard service (its own lock — see the poller docs).
    let (input, engine, clipboard, liveness) = kvmshare_platform::server(None).map_err(|e| format!("platform: {e}"))?;
    let engine = Arc::new(Mutex::new(engine));
    let clipboard: Arc<Mutex<Box<dyn kvmshare_core::client::Clipboard>>> = Arc::new(Mutex::new(clipboard));

    let (ctl_tx, ctl_rx) = mpsc::channel();
    // Lifecycle events (client connect/disconnect) flow out to the app
    // layer, which persists the GUI's connected-client list and applies
    // auto-config (screen sizes reported by clients).
    let (evt_tx, evt_rx) = mpsc::channel();
    let state = state_dir();
    let policy = Policy {
        allowlist: cfg.network.allowlist,
        local_only: cfg.network.local_only,
        trusted_ids: cfg.network.trusted_ids.clone(),
    };
    let server = Arc::new(
        Server::with_options(
            session_from_config(&cfg),
            port,
            Options {
                control: Some(ctl_rx),
                policy,
                events: Some(evt_tx),
                server_id: machine_id(&state),
            },
        )
        .map_err(|e| format!("bind: {e}"))?,
    );

    // Clipboard: local changes are broadcast to every client.
    spawn_server_clipboard(clipboard.clone(), server.clone());

    // Config hot-reload: watch the file and adopt changes live, without
    // a restart (the GUI saves the config while the server keeps running).
    spawn_config_watcher(config_path.clone(), ctl_tx.clone());

    // The GUI's connected-client list: persist lifecycle events to
    // `clients.json` in the state dir (the GUI polls it), and
    // auto-configure screen sizes a client reports back into the config
    // file (the user never has to type a resolution).
    spawn_event_sink(evt_rx, state.clone(), config_path.clone());

    // The GUI's per-client control file: `server.cmd` lines like
    // `disconnect hp` / `reconnect hp` / `restart hp` become
    // [`Control::ClientCommand`] messages on the main loop.
    spawn_server_cmd_watcher(state.join("server.cmd"), ctl_tx);

    // Run forever, forwarding local input. The supervisor inside watches
    // the input path's health and, on a wedge while the cursor is on a
    // client, exits with a code the process manager restarts from — the
    // local machine is never left input-trapped.
    server.run(input, engine, clipboard, liveness).map_err(|e| format!("server: {e}"))
}

/// How often the config watcher polls the file for changes.
const CONFIG_WATCH_POLL: Duration = Duration::from_millis(600);

/// How often the `server.cmd` control file is polled.
const CMD_WATCH_POLL: Duration = Duration::from_millis(250);

/// Consume the server's lifecycle events: persist the connected-client
/// list (`clients.json`) for the GUI and auto-configure screen sizes a
/// client reports back into the config file.
///
/// The config write is deliberately narrow: it only updates `width` /
/// `height` of the screen whose *name* matches the connected client,
/// and only when the reported logical size differs from what the config
/// says. Positions, names and the network section are never touched, so
/// the user's layout decisions are preserved; the hot-reload watcher
/// picks the corrected sizes up live.
fn spawn_event_sink(
    rx: mpsc::Receiver<ServerEvent>,
    state_dir: PathBuf,
    config_path: PathBuf,
) {
    thread::spawn(move || {
        use std::collections::HashMap;
        let mut clients: HashMap<String, (String, String, u64)> = HashMap::new(); // name → (id, addr, since_ms)
        while let Ok(evt) = rx.recv() {
            match evt {
                ServerEvent::ClientConnected { name, id, addr, since_ms, info } => {
                    clients.insert(name.clone(), (id, addr, since_ms));
                    let _ = auto_config_screen_size(&config_path, &name, &info);
                }
                ServerEvent::ClientDisconnected { name } => {
                    clients.remove(&name);
                }
            }
            persist_clients(&state_dir, &clients);
        }
    });
}

/// Serialize the current client map as `clients.json` (atomic write).
fn persist_clients(state_dir: &PathBuf, clients: &std::collections::HashMap<String, (String, String, u64)>) {
    #[derive(serde::Serialize)]
    struct Entry<'a> {
        name: &'a str,
        id: &'a str,
        addr: &'a str,
        since_ms: u64,
    }
    let list: Vec<Entry> = clients
        .iter()
        .map(|(name, (id, addr, since_ms))| Entry { name, id, addr, since_ms: *since_ms })
        .collect();
    let text = match serde_json::to_string(&list) {
        Ok(t) => t,
        Err(_) => return,
    };
    let path = state_dir.join("clients.json");
    let _ = std::fs::create_dir_all(state_dir);
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, &text).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Correct a screen's configured size to the size a client just reported
/// (logical pixels = physical ÷ scale).
///
/// The reported size is ground truth: the client injects into and
/// beacons from its own physical display, and the crossing math is only
/// right when the layout matches it. The live session already adopts
/// the report unconditionally on connect and on every resolution change
/// (`Session::update_screen_info`); this keeps the persisted file in
/// step so the GUI's Layout page shows reality and the next start agrees
/// from the first second. A size that came from a *previous* client
/// report — a phantom from an older, broken build (a root window
/// stretched over a disconnected monitor, e.g. 3840×1080) — is exactly
/// what must be corrected, so there is no "user-typed" exception: the
/// live layout would override it anyway on the next connect.
///
/// Only touches `width`/`height` of the matching screen. Positions,
/// names and the network section are never touched, so the user's
/// layout decisions are preserved; the hot-reload watcher picks the
/// corrected sizes up live.
fn auto_config_screen_size(config_path: &PathBuf, name: &str, info: &ScreenInfo) -> Result<(), String> {
    let mut cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(_) => return Ok(()), // config missing/broken — nothing to correct
    };
    let Some(screen) = cfg.screens.iter_mut().find(|s| s.name == name) else {
        return Ok(()); // not a configured screen — nothing to correct
    };
    // Layout coordinates are the same space the client injects into and
    // beacons from (physical pixels on Windows, root pixels on X11 — the
    // reported scale is informational), so the reported size is used
    // as-is.
    let w = info.width.max(1);
    let h = info.height.max(1);
    if w == screen.width && h == screen.height {
        return Ok(()); // already correct
    }
    screen.width = w;
    screen.height = h;
    let text = match toml::to_string_pretty(&cfg) {
        Ok(t) => t,
        Err(_) => return Ok(()),
    };
    let tmp = config_path.with_extension("toml.tmp");
    if std::fs::write(&tmp, &text).is_ok() {
        let _ = std::fs::rename(&tmp, config_path);
        log_info!("auto-configured screen {name:?} to {w}x{h} from the client's reported geometry");
    }
    Ok(())
}

/// Poll the `server.cmd` control file and turn each line into a
/// [`Control::ClientCommand`]. Lines are `<command> <name>` where
/// `command` is disconnect | reconnect | restart. The file is truncated
/// after a successful read (atomic tmp+rename), so the GUI can keep
/// appending commands while the server is processing.
fn spawn_server_cmd_watcher(path: PathBuf, tx: mpsc::Sender<Control>) {
    thread::spawn(move || {
        loop {
            thread::sleep(CMD_WATCH_POLL);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) if !t.trim().is_empty() => t,
                _ => continue,
            };
            // Clear the file first: even if parsing fails, the commands
            // have been consumed (a partial write must not replay).
            let tmp = path.with_extension("cmd.tmp");
            if std::fs::write(&tmp, b"").is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
            for line in text.lines() {
                let line = line.trim();
                let mut parts = line.splitn(2, ' ');
                let (Some(cmd), Some(name)) = (parts.next(), parts.next()) else { continue };
                let command = match cmd {
                    "disconnect" => kvmshare_protocol::id::control::DISCONNECT,
                    "reconnect" => kvmshare_protocol::id::control::RECONNECT,
                    "restart" => kvmshare_protocol::id::control::RESTART,
                    _ => {
                        log_warn!("server.cmd: unknown command {cmd:?}");
                        continue;
                    }
                };
                if tx.send(Control::ClientCommand { name: name.to_owned(), command }).is_err() {
                    return; // server gone
                }
            }
        }
    });
}

/// Poll the config file and push a [`Control::Reload`] whenever its
/// content changes, so layout edits apply live. A transient parse error
/// (file mid-write) just defers the reload until the file is valid.
fn spawn_config_watcher(path: PathBuf, tx: mpsc::Sender<Control>) {
    thread::spawn(move || {
        // Prime the watcher with the file as it is now, so startup does
        // not log a spurious "layout reloaded" — only real changes do.
        let mut last: Option<(SystemTime, u64)> = std::fs::metadata(&path)
            .ok()
            .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
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