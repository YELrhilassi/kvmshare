//! The client supervisor watchdog.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kvmshare_log::log_error;

use crate::client::shared::Shared;
use crate::time::now_ms;

/// The supervisor watchdog, on its own thread: it shares nothing with
/// the workers but two atomic liveness ticks, so nothing that wedges
/// them can wedge it. While control is on this machine, a motion thread
/// that stops ticking means the cursor is no longer being steered — a
/// blocking call is holding a lock the motion loop needs. Recovery is
/// deliberately blunt and always safe: force-restore local input (the
/// user's own mouse/keyboard work again, no matter what), then end the
/// session so the reconnect path starts a clean client. The stall is
/// logged loudly with the liveness facts so the root cause is visible.
pub(crate) fn supervisor_loop(shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(500));
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        // Only guard while this machine is being controlled: local idle
        // is not a stall.
        if !shared.active.load(Ordering::Acquire) {
            continue;
        }
        let now = now_ms();
        let motion_age = now.saturating_sub(shared.motion_tick_ms.load(Ordering::Relaxed));
        let tcp_age = now.saturating_sub(shared.tcp_tick_ms.load(Ordering::Relaxed));
        // The motion loop ticks at ~4 ms; any age over 3 s while active
        // is a genuine wedge (a healthy loop cannot be quiet that long),
        // never a false positive.
        if motion_age > 3000 {
            // Probe which lock the stalled motion thread is likely
            // waiting on: a lock we cannot acquire here is held by a
            // wedged thread (the motion loop takes motion then injector
            // every tick).
            let motion_locked = shared.motion.try_lock().is_err();
            let injector_locked = shared.injector.try_lock().is_err();
            let clipboard_locked = shared.clipboard.try_lock().is_err();
            log_error!(
                "SUPERVISOR: motion thread stalled {motion_age} ms while control is on this machine (tcp thread {tcp_age} ms; locks held: motion={motion_locked} injector={injector_locked} clipboard={clipboard_locked}) — releasing local input and restarting the session"
            );
            // Force-restore the machine's own input. `try_lock` so the
            // supervisor itself can never block on a wedged lock.
            if let Ok(mut inj) = shared.injector.try_lock() {
                inj.emergency_release();
            }
            // End the session: the TCP loop wakes within its read
            // timeout, run() unwinds, and the binary reconnects fresh.
            shared.stop.store(true, Ordering::Relaxed);
            break;
        }
    }
}