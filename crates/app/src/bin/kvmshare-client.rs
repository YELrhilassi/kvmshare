//! The kvmshare client.
//!
//! A machine controlled by a kvmshare server. Connects, says hello with
//! its host name, then injects the server's cursor/keyboard/clipboard
//! events into the local desktop.

use std::sync::mpsc;

use kvmshare_app::{hostname, parse_client_args, with_default_port, DEFAULT_PORT};
use kvmshare_core::client::Client;
use kvmshare_protocol::message::Message;

fn main() {
    if let Err(e) = run() {
        eprintln!("kvmshare-client: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_client_args()?;
    let addr = with_default_port(&args.server_addr, DEFAULT_PORT);
    let name = args.name.unwrap_or_else(hostname);

    // Platform: the local injector (moves the cursor, injects keys...).
    let mut injector = kvmshare_platform::client(None).map_err(|e| format!("platform: {e}"))?;

    println!("kvmshare-client: connecting to {addr} as {name}");
    let client = Client::connect(&addr, &name, injector.screen_info()).map_err(|e| format!("connect: {e}"))?;
    println!("kvmshare-client: connected, screen id {}", client.own_id());

    // The outbox is reserved for app-level control messages; the core run
    // loop handles clipboard upload and keepalives itself.
    let (_out_tx, out_rx) = mpsc::channel::<Message>();
    client.run(injector, &out_rx).map_err(|e| format!("session: {e}"))
}