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
use kvmshare_log::{log_error, log_info, log_warn};
use kvmshare_protocol::message::Message;

/// How long to wait between connection attempts. The server may not be up
/// yet, or may restart — a client that dies on a refused connection would
/// be useless.
const RETRY_DELAY: Duration = Duration::from_secs(3);

fn main() {
    if let Err(e) = run() {
        log_error!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_client_args()?;
    kvmshare_log::init(
        &args.log_level.unwrap_or_else(kvmshare_log::level_from_env_or_default),
        args.log_ctl,
    )?;

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
        // Platform: the local injector (moves the cursor, injects keys...)
        // and the standalone clipboard service. They are separate so a
        // clipboard call that stalls can never freeze the cursor.
        let (mut injector, clipboard) = match kvmshare_platform::client(None) {
            Ok(pair) => pair,
            Err(e) => {
                if !warned {
                    log_warn!("platform: {e} — retrying every 3 s");
                    warned = true;
                }
                thread::sleep(RETRY_DELAY);
                continue;
            }
        };

        log_info!("connecting to {addr} as {name}");
        match Client::connect(&addr, &name, injector.screen_info()) {
            Ok(client) => {
                warned = false;
                log_info!("connected, screen id {}", client.own_id());
                // The outbox is reserved for app-level control messages;
                // the core run loop handles clipboard upload and
                // keepalives itself.
                let (_out_tx, out_rx) = mpsc::channel::<Message>();
                if let Err(e) = client.run(injector, clipboard, &out_rx) {
                    log_warn!("session ended: {e} — reconnecting");
                }
            }
            Err(e) => {
                if !warned {
                    log_warn!("connect failed: {e} — retrying every 3 s");
                    warned = true;
                }
            }
        }
        thread::sleep(RETRY_DELAY);
    }
}