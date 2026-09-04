//! High-resolution timer for Windows processes.
//!
//! Windows defaults to a coarse system timer (~15.6 ms granularity):
//! `Sleep` and socket read timeouts wake up to that late, no matter how
//! short they are asked to wait. For loops paced at a few milliseconds
//! (the client's motion tick, the capture's flush cadence) that turns
//! an even 250 Hz stream into ~15 ms clumps — the cursor visibly stops
//! and jumps. [`timeBeginPeriod(1)`](windows_sys::Win32::Media::timeBeginPeriod)
//! raises the timer resolution to 1 ms for the calling process, so the
//! loops wake at their intended cadence. The guard releases it on drop
//! (refcounted: both the server and the client engage it, whichever
//! leaves last restores the system default).

use std::sync::Mutex;

use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};

/// Engaged-count of high-resolution timers for this process.
static REFS: Mutex<u32> = Mutex::new(0);

/// RAII guard for the 1 ms Windows timer resolution.
pub struct HighResTimer;

impl HighResTimer {
    /// Raise the timer resolution to 1 ms for this process. Every guard
    /// engaged raises the refcount; the resolution returns to the system
    /// default when the last guard drops.
    pub fn engage() -> Self {
        let mut refs = REFS.lock().unwrap();
        if *refs == 0 {
            // SAFETY: timeBeginPeriod takes a period in [1, 16]; 1 is the
            // finest allowed. It is balanced by timeEndPeriod in Drop.
            unsafe {
                timeBeginPeriod(1);
            }
        }
        *refs += 1;
        HighResTimer
    }

    /// Engage for the whole process lifetime (the caller is a long-lived
    /// daemon; the guard is never dropped).
    pub fn engage_forever() {
        std::mem::forget(Self::engage());
    }
}

impl Drop for HighResTimer {
    fn drop(&mut self) {
        let mut refs = REFS.lock().unwrap();
        *refs = refs.saturating_sub(1);
        if *refs == 0 {
            // SAFETY: balances the engage above.
            unsafe {
                timeEndPeriod(1);
            }
        }
    }
}