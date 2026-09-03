//! The kvmshare server.
//!
//! The machine whose keyboard and mouse are shared. Owns the virtual
//! desktop layout, listens for clients, and forwards local input to
//! whichever client the cursor is on.

use std::sync::{Arc, Mutex};

use kvmshare_app::{default_config_path, parse_server_args, session_from_config, spawn_server_clipboard, Config};
use kvmshare_core::server::Server;

fn main() {
    if let Err(e) = run() {
        eprintln!("kvmshare-server: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_server_args()?;

    // Config → layout → session (the switching brain). When no --config
    // is given, fall back to the standard locations.
    let config_path = args.config.unwrap_or_else(default_config_path);
    let cfg = Config::load(&config_path)?;
    let port = if args.port != 0 { args.port } else { cfg.port };
    println!(
        "kvmshare-server: layout {} screens (local: {}), listening on :{port}",
        cfg.screens.len(),
        cfg.screens[0].name
    );

    // Platform: capture local input + control the local cursor.
    let (input, engine) = kvmshare_platform::server(None).map_err(|e| format!("platform: {e}"))?;
    let engine = Arc::new(Mutex::new(engine));

    let server = Arc::new(Server::bind(session_from_config(&cfg), port).map_err(|e| format!("bind: {e}"))?);

    // Clipboard: local changes are broadcast to every client.
    spawn_server_clipboard(engine.clone(), server.clone());

    // Run forever, forwarding local input.
    server.run(input, engine).map_err(|e| format!("server: {e}"))
}