//! Monotonic time shared by the server and client.

use std::sync::OnceLock;
use std::time::Instant;

/// Milliseconds since the first call in this process (monotonic anchor).
///
/// Liveness ticks and supervisor deadlines only ever compare values
/// produced by this function, so they share one epoch and are immune to
/// system clock changes — an NTP correction or manual time jump can
/// never falsely trip (or silence) a watchdog.
pub fn now_ms() -> u64 {
    static BOOT: OnceLock<Instant> = OnceLock::new();
    let boot = BOOT.get_or_init(Instant::now);
    boot.elapsed().as_millis() as u64
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;

