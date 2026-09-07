//! The server supervisor: heartbeats and the watchdog that recovers a
//! wedged input path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use kvmshare_log::log_error;

use crate::time::now_ms;

/// How often the main loop polls for control messages while idle.
pub const CONTROL_POLL: Duration = Duration::from_millis(100);

/// A running server.
/// Heartbeats the server supervisor watches. Each field is a millisecond
/// timestamp (see [`now_ms`]) updated by its owning thread every loop
/// iteration. A tick that goes stale while the cursor is on a client
/// means that thread is wedged — and a wedged input-path thread while
/// remote can leave this machine's keyboard and mouse trapped (the
/// engine lock blocks crossings, or the capture thread holds the local
/// input grab forever), so the supervisor recovers by exiting cleanly
/// (see [`supervisor_loop`]).
#[derive(Default)]
pub struct Liveness {
    /// The main input loop (wakes every ≤ [`CONTROL_POLL`]).
    pub loop_tick_ms: AtomicU64,
    /// The platform's capture thread (wakes every ~2 ms). This is the
    /// thread that owns the local input grab while remote — the one
    /// whose wedge traps the machine. Shared (`Arc`) because the
    /// platform creates and owns the thread that writes it.
    pub capture_tick_ms: Arc<AtomicU64>,
}

/// How often the supervisor wakes to check the heartbeats.
const SUPERVISOR_POLL: Duration = Duration::from_millis(500);
/// A heartbeat older than this (ms) while remote is a genuine wedge: a
/// healthy main loop wakes every [`CONTROL_POLL`] and a healthy capture
/// loop every ~2 ms, so 3 s means the thread has missed hundreds of
/// wakes (never a false positive from load).
const SUPERVISOR_STALL_MS: u64 = 3000;
/// Exit code the supervisor uses to ask the process manager (the GUI)
/// for a restart. Distinct from a crash (1) so the manager can tell a
/// deliberate recovery from a failure.
pub const EXIT_RESTART: i32 = 66;

/// The watchdog for the server's input path. While the cursor is on a
/// client, the local machine must be able to bring it home: a wedged
/// main loop holds the engine lock that crossings need, and a wedged
/// capture thread holds the local input grab forever — either way the
/// local keyboard and mouse stay trapped until the process dies. The
/// supervisor shares nothing with those threads but the liveness
/// atomics, so whatever wedges them cannot block it. On a stall it
/// exits with [`EXIT_RESTART`]; process exit closes every fd and X
/// connection, which releases every kernel and X grab — the machine is
/// never left input-dead, and the process manager (the GUI) restarts a
/// clean server. Disabled when no real capture is present (test
/// harnesses): there is nothing to trap.
pub fn supervisor_loop(active: Arc<Mutex<Option<u8>>>, liveness: Arc<Liveness>) -> ! {
    loop {
        thread::sleep(SUPERVISOR_POLL);
        if liveness.capture_tick_ms.load(Ordering::Relaxed) == 0 {
            continue; // no real capture thread: nothing to guard
        }
        // `try_lock`: the supervisor must never block on a lock a wedged
        // thread holds — that would kill the watchdog itself. If the
        // active-state lock is held, assume the worst (remote) and let
        // the tick ages decide.
        let remote = match active.try_lock() {
            Ok(guard) => guard.is_some(),
            Err(_) => true,
        };
        if !remote {
            continue; // local idle is not a stall
        }
        let now = now_ms();
        let loop_age = now.saturating_sub(liveness.loop_tick_ms.load(Ordering::Relaxed));
        let capture_age = now.saturating_sub(liveness.capture_tick_ms.load(Ordering::Relaxed));
        if loop_age > SUPERVISOR_STALL_MS || capture_age > SUPERVISOR_STALL_MS {
            log_error!(
                "SUPERVISOR: input path stalled while the cursor is on a client (main loop {loop_age:?}, capture {capture_age:?}) — exiting with code {EXIT_RESTART} so the manager restarts a clean server; local input is never left trapped"
            );
            // Give the log writer a moment to drain the line, then exit
            // (which releases every grab via fd/connection close).
            thread::sleep(Duration::from_millis(120));
            std::process::exit(EXIT_RESTART);
        }
    }
}