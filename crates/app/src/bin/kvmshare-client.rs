//! The kvmshare client.
//!
//! A machine controlled by a kvmshare server. Connects, says hello with
//! its host name, then injects the server's cursor/keyboard/clipboard
//! events into the local desktop.

// windowsgui on Windows: same reasoning as the server — a background
// daemon must not own a console, and a task-started (elevated) client
// with the console subsystem flashed a cmd window on the desktop.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use kvmshare_app::guard::{self, RoleGuard};
use kvmshare_app::{
    client_config_path, hostname, machine_id, parse_client_args, revoked_policy, state_dir,
    with_default_port, write_client_state, write_client_state_connected,
    write_client_state_refused, write_client_state_stopped, ClientConfig, DEFAULT_PORT,
};
use kvmshare_core::client::{Client, SessionEnd};
use kvmshare_log::{log_error, log_info, log_warn};
use kvmshare_protocol::message::Message;

/// How long to wait between connection attempts. The server may not be up
/// yet, or may restart — a client that dies on a refused connection would
/// be useless.
const RETRY_DELAY: Duration = Duration::from_secs(3);

/// This machine's audio setup, or `None` when it asks for nothing.
///
/// The client is otherwise config-free — it obeys the *remote* server's
/// layout — but `[audio]` describes this machine's own hardware and
/// consent, so it must come from this machine's own file (see
/// [`ClientConfig`]). Only the audio section is read, so a file that is
/// missing, empty, or has never been written means "no audio", which is
/// exactly what a machine with no `[audio]` section asks for.
fn client_audio_setup() -> Option<kvmshare_core::client::AudioSetup> {
    let path = client_config_path();
    let cfg = match ClientConfig::load(&path) {
        Ok(cfg) => cfg,
        Err(e) => {
            // A file that exists but does not parse is worth a word: the
            // user asked for something and is not getting it. A missing
            // file is the default state and stays quiet.
            log_warn!("audio: ignoring {}: {e}", path.display());
            return None;
        }
    };
    if !cfg.audio.is_active() {
        return None;
    }
    Some(kvmshare_core::client::AudioSetup {
        options: cfg.audio.to_options(),
        backend: kvmshare_platform::audio::backend(),
        // Live status for the GUI (the audio.state file).
        status: Some(kvmshare_app::audio_status_sink(state_dir())),
    })
}

fn main() {
    if let Err(e) = run() {
        log_error!("{e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_client_args()?;
    // The Media page's Test button plays a tone through *this* machine's
    // configured output, and the audio backend is the platform layer the Go
    // GUI cannot link — so the client binary answers it, the same way it
    // would answer a device listing. Before the role lock and before any
    // socket: a machine that is not running a client must still be able to
    // hear whether its output works.
    if let Some(test) = &args.audio_test_tone {
        println!("{}", kvmshare_app::audio_test_tone_json(&test.device, test.seconds));
        return Ok(());
    }
    if let Some(f) = &args.log_file {
        kvmshare_log::set_log_file(f.clone());
    }
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

    // Repair an output route a previous process left behind (see the
    // server's equivalent). Once per role process, never at backend
    // construction — the test-tone helper builds a backend too.
    kvmshare_platform::audio::recover_exclusive_output();

    let addr = with_default_port(&args.server_addr, DEFAULT_PORT);
    if args.server_addr.is_empty() {
        return Err("no server address (pass HOST[:PORT], or stage it in the --args-file)".into());
    }
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
    // Servers this machine must not hold a session with (revoked in the
    // GUI, handed to us at spawn via env — or via the state-dir file on
    // the scheduled-task spawn, which carries no custom environment).
    // Enforced right after the handshake, once the server's id is known
    // — see `kvmshare_app::trust`.
    let revoked = revoked_policy(&state_dir);
    write_client_state(&state_dir, "disconnected", &addr);
    // A stale audio status from a previous run is not this run's: clear it
    // so the GUI never shows a link that no longer exists.
    kvmshare_app::clear_audio_state(&state_dir);
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
                // The server revealed its id in `Welcome`; if the operator
                // revoked it on this machine, end the session now and do
                // not reconnect. Written as a *requested stop* so the
                // GUI's auto-connect does not immediately retry.
                if revoked.is_revoked(client.server_id()) {
                    log_warn!(
                        "server {} is revoked on this machine — refusing the session",
                        client.server_id()
                    );
                    write_client_state_stopped(&state_dir, &addr);
                    return Ok(());
                }
                warned = false;
                log_info!("connected, screen id {}", client.own_id());
                // The id, not the address, is what the GUI matches the
                // session to a peer by — addresses churn (DHCP, dual
                // interfaces, beacon expiry); the id does not.
                write_client_state_connected(&state_dir, &addr, client.server_id());
                // The outbox is reserved for app-level control messages;
                // the core run loop handles clipboard upload and
                // keepalives itself.
                let (_out_tx, out_rx) = mpsc::channel::<Message>();
                // Control-ownership transitions land in control.state:
                // away=1 while the server drives this machine's devices,
                // gone when control is back. The GUI reads it to gate
                // key recording and show "being controlled" truthfully.
                let state_for_control = state_dir.clone();
                let on_control: kvmshare_core::client::ControlObserver =
                    Box::new(move |controlled| {
                        let path = state_for_control.join("control.state");
                        if controlled {
                            let tmp = path.with_extension("state.tmp");
                            if std::fs::write(&tmp, "controlled=1\n").is_ok() {
                                let _ = std::fs::rename(&tmp, &path);
                            }
                        } else {
                            let _ = std::fs::remove_file(&path);
                        }
                    });
                // Audio: this machine's own `[audio]` section decides
                // whether it takes part. Read per connection rather than
                // once, so a config edit takes effect on the next
                // reconnect without restarting the client — the client is
                // already a loop, and the only stateful part of audio is
                // the run's own socket.
                let client = match client_audio_setup() {
                    Some(setup) => client.with_audio(setup),
                    None => client,
                };
                match client.run(injector, clipboard, &out_rx, Some(on_control)) {
                    // The server told us to disconnect: do not reconnect.
                    // The operator starts the client again when wanted.
                    Ok(SessionEnd::Disconnected) => {
                        log_info!("disconnected by the server — staying stopped");
                        // The `stopped` marker tells the GUI this was a
                        // requested stop, not a transient drop, so its
                        // auto-connect does not undo it.
                        write_client_state_stopped(&state_dir, &addr);
                        return Ok(());
                    }
                    // Reconnect/restart: immediately, fresh handshake.
                    Ok(SessionEnd::Reconnect) => {
                        log_info!("reconnect requested by the server");
                        continue;
                    }
                    Ok(SessionEnd::LinkClosed) => {
                        // The state file must never keep claiming a
                        // connection that no longer exists: write
                        // "disconnected" before the retry delay, so the
                        // GUI's Home page stops showing "in control" the
                        // moment the link drops (it flips to "connecting"
                        // when the loop's next attempt starts).
                        write_client_state(&state_dir, "disconnected", &addr);
                        log_info!("link closed — reconnecting in {RETRY_DELAY:?}");
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
                // A refusal is the server answering a question, not the
                // network failing to deliver it. Retrying cannot change
                // the answer — the machine must be un-revoked, trusted,
                // or re-added to the layout by an operator — so the loop
                // stops and says why. (The old behavior retried forever:
                // the GUI read "connecting…" while the server answered
                // every attempt with the same refusal.)
                if let Some(refusal) = kvmshare_core::client::refusal_from(&e) {
                    log_error!("{refusal} — staying stopped");
                    write_client_state_refused(&state_dir, &addr, &refusal.text);
                    std::process::exit(1);
                }
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