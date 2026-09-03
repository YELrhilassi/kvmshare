//! The kvmshare client.
//!
//! A machine controlled by a kvmshare server. Connects, says hello with
//! its host name, then injects the server's cursor/keyboard/clipboard
//! events into the local desktop.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kvmshare_app::guard::{self, RoleGuard};
use kvmshare_app::{hostname, parse_client_args, with_default_port, DEFAULT_PORT};
use kvmshare_core::client::Client;
use kvmshare_protocol::message::Message;

/// How long to wait between connection attempts. The server may not be up
/// yet, or may restart — a client that dies on a refused connection would
/// be useless.
const RETRY_DELAY: Duration = Duration::from_secs(3);

fn main() {
    if let Err(e) = run() {
        eprintln!("kvmshare-client: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_client_args()?;

    // One role per machine, enforced at the OS level: refuse to start if
    // a server is running here, and hold our own lock for the process
    // lifetime (flock dies with us — no orphans).
    let _guard: RoleGuard = guard::acquire(guard::ROLE_CLIENT)?;

    let addr = with_default_port(&args.server_addr, DEFAULT_PORT);
    let name = args.name.unwrap_or_else(hostname);

    // Connect forever (reconnecting while the process lives). The role
    // lock keeps this the single client instance; stopping the process
    // (SIGTERM) is what ends the loop. Errors are printed once per state
    // change so a down server doesn't spam the log.
    let mut warned = false;
    loop {
        // Platform: the local injector (moves the cursor, injects keys...).
        let mut injector = match kvmshare_platform::client(None) {
            Ok(i) => i,
            Err(e) => {
                if !warned {
                    eprintln!("kvmshare-client: platform: {e} — retrying every 3 s");
                    warned = true;
                }
                thread::sleep(RETRY_DELAY);
                continue;
            }
        };

        println!("kvmshare-client: connecting to {addr} as {name}");
        match Client::connect(&addr, &name, injector.screen_info()) {
            Ok(client) => {
                warned = false;
                println!("kvmshare-client: connected, screen id {}", client.own_id());
                // The outbox is reserved for app-level control messages;
                // the core run loop handles clipboard upload and
                // keepalives itself.
                let (_out_tx, out_rx) = mpsc::channel::<Message>();
                if let Err(e) = client.run(injector, &out_rx) {
                    eprintln!("kvmshare-client: session ended: {e} — reconnecting");
                }
            }
            Err(e) => {
                if !warned {
                    eprintln!("kvmshare-client: connect failed: {e} — retrying every 3 s");
                    warned = true;
                }
            }
        }
        thread::sleep(RETRY_DELAY);
    }
}