//! The kvmshare client.
//!
//! A machine controlled by a kvmshare server. Connects, says hello with
//! its host name, then injects the server's cursor/keyboard/clipboard
//! events into the local desktop.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kvmshare_app::guard::{self, RoleGuard};
use kvmshare_app::{hostname, machine_id, parse_client_args, state_dir, with_default_port, write_client_state, DEFAULT_PORT};
use kvmshare_core::client::{Client, SessionEnd};
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
    // The client drives this machine's cursor on a millisecond cadence;
    // it must outrank busy apps (and the BelowNormal priority Task
    // Scheduler launches with), or anything the user does on this
    // machine can starve the cursor.
    kvmshare_platform::raise_priority();

    // One role per machine, enforced at the OS level: refuse to start if
    // a server is running here, and hold our own lock for the process
    // lifetime (flock dies with us — no orphans).
    let _guard: RoleGuard = guard::acquire(guard::ROLE_CLIENT)?;

    let addr = with_default_port(&args.server_addr, DEFAULT_PORT);
    let name = args.name.unwrap_or_else(hostname);
    // This machine's stable id: sent in Hello so the server can trust
    // this machine by id (the allowlist's trusted-ids bypass) and list
    // it in the GUI. The GUI reads the same file.
    let id = machine_id(&state_dir());

    // Connect forever (reconnecting while the process lives). The role
    // lock keeps this the single client instance; stopping the process
    // (SIGTERM) is what ends the loop. Errors are printed once per state
    // change so a down server doesn't spam the log. The session can also
    // end on the server's command: disconnect (stop), reconnect/restart
    // (reconnect immediately).
    let mut warned = false;
    let mut prompt_warned = false;
    // The live state file the GUI reads (Home's connection panel): it
    // always says what this process is doing *right now* — connecting,
    // connected, or not connected. Written on every transition.
    let state_dir = state_dir();
    write_client_state(&state_dir, "disconnected", &addr);
    loop {
        // The UAC secure desktop (Windows): while a consent prompt is
        // up, no injected input can land anywhere, so connecting (or
        // reconnecting after the session ended for that reason) would
        // only churn a session into a wall. Wait it out — the person at
        // this machine answers the prompt, and the loop resumes when the
        // normal desktop returns.
        if kvmshare_platform::secure_desktop_active() {
            if !prompt_warned {
                log_warn!("Windows secure desktop active (UAC prompt) — waiting for it to be answered before (re)connecting");
                prompt_warned = true;
            }
            thread::sleep(RETRY_DELAY);
            continue;
        }
        prompt_warned = false;
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

        log_info!("connecting to {addr} as {name} (machine {id})");
        write_client_state(&state_dir, "connecting", &addr);
        match Client::connect(&addr, &name, &id, injector.screen_info()) {
            Ok(client) => {
                warned = false;
                log_info!("connected, screen id {}", client.own_id());
                write_client_state(&state_dir, "connected", &addr);
                // The outbox is reserved for app-level control messages;
                // the core run loop handles clipboard upload and
                // keepalives itself.
                let (_out_tx, out_rx) = mpsc::channel::<Message>();
                match client.run(injector, clipboard, &out_rx) {
                    // The server told us to disconnect: do not reconnect.
                    // The operator starts the client again when wanted.
                    Ok(SessionEnd::Disconnected) => {
                        log_info!("disconnected by the server — staying stopped");
                        write_client_state(&state_dir, "disconnected", &addr);
                        return Ok(());
                    }
                    // Reconnect/restart: immediately, fresh handshake.
                    Ok(SessionEnd::Reconnect) => {
                        log_info!("reconnect requested by the server");
                        continue;
                    }
                    Ok(SessionEnd::LinkClosed) => {
                        thread::sleep(RETRY_DELAY);
                        continue;
                    }
                    Err(e) => {
                        log_warn!("session ended: {e} — reconnecting");
                        write_client_state(&state_dir, "disconnected", &addr);
                        thread::sleep(RETRY_DELAY);
                    }
                }
            }
            Err(e) => {
                if !warned {
                    log_warn!("connect failed: {e} — retrying every 3 s");
                    warned = true;
                }
                write_client_state(&state_dir, "disconnected", &addr);
                thread::sleep(RETRY_DELAY);
            }
        }
    }
}