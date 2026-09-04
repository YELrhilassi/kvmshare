//! Motion accumulation shared by every capture backend.
//!
//! Device input arrives as raw deltas at device rate (up to 1000 Hz for
//! a modern mouse). Forwarding one message per raw event floods the wire
//! with ~17-byte frames and turns any network load into visible cursor
//! jitter. [`PendingMotion`] merges deltas and emits whole pixels at a
//! fixed cadence ([`MOTION_PERIOD`]) — the total motion is identical,
//! but the frame rate is capped well above what the eye can follow.

use std::time::Instant;

/// Minimum gap between forwarded motion messages. ~250 Hz keeps motion
/// perfectly smooth while cutting frame count 4x (and far more for very
/// fast devices). The total delta is preserved — only the message rate
/// changes.
pub const MOTION_PERIOD: std::time::Duration = std::time::Duration::from_millis(4);

/// Motion pending forwarding: raw (fractional) deltas merged between
/// sends, emitted as whole pixels at [`MOTION_PERIOD`] cadence. The
/// fractional remainder is kept so slow moves still accumulate into full
/// pixels instead of being truncated away.
#[derive(Debug, Default)]
pub struct PendingMotion {
    fx: f64,
    fy: f64,
    /// When the last motion message was sent (rate limiter).
    last_send: Option<Instant>,
}

impl PendingMotion {
    /// Merge one raw delta into the accumulator.
    pub fn push(&mut self, dx: f64, dy: f64) {
        self.fx += dx;
        self.fy += dy;
    }

    /// Take the accumulated whole pixels (leaving the fractional
    /// remainder), or `None` when there is less than a pixel in total.
    ///
    /// Truncation is toward zero on purpose: `floor` would turn a −0.4 px
    /// accumulator into a −1 px event (overshooting by 0.6), biasing the
    /// virtual cursor leftward under micro-jitter. Truncation keeps the
    /// signed fraction and only emits once a full pixel is crossed in
    /// either direction — symmetric for both signs.
    pub fn take_whole(&mut self) -> Option<(i32, i32)> {
        let ix = self.fx.trunc() as i32;
        let iy = self.fy.trunc() as i32;
        if ix == 0 && iy == 0 {
            return None;
        }
        self.fx -= ix as f64;
        self.fy -= iy as f64;
        Some((ix, iy))
    }

    /// Forward accumulated motion at [`MOTION_PERIOD`] cadence. Emits a
    /// message via `send` when whole pixels are due. Called from the
    /// capture loop at its poll cadence, so motion is never held back
    /// longer than one period.
    pub fn flush(&mut self, send: &mut dyn FnMut(i32, i32)) {
        let Some((ix, iy)) = self.take_whole() else { return };
        let now = Instant::now();
        let due = match self.last_send {
            Some(t) => now.duration_since(t) >= MOTION_PERIOD,
            None => true,
        };
        if !due {
            // Not time yet: put the pixels back for the next flush.
            self.fx += ix as f64;
            self.fy += iy as f64;
            return;
        }
        self.last_send = Some(now);
        send(ix, iy);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_fractional_remainder() {
        let mut m = PendingMotion::default();
        // Sub-pixel motion is held until it crosses a full pixel, with
        // no overshoot in either direction: -0.9px stays put...
        m.push(-0.3, 0.0);
        m.push(-0.3, 0.0);
        m.push(-0.3, 0.0);
        assert_eq!(m.take_whole(), None);
        assert!((m.fx + 0.9).abs() < 1e-9);
        // ...until it crosses -1, and then it emits exactly -1 (a floor
        // here would have emitted -1 already at -0.9 and biased the
        // cursor leftward under jitter).
        m.push(-0.3, 0.0);
        assert_eq!(m.take_whole(), Some((-1, 0)));
        assert!((m.fx + 0.2).abs() < 1e-9);
        // Nothing more to take until more motion accrues.
        assert_eq!(m.take_whole(), None);
        // Fast motion passes through whole (the leftover -0.2 from the
        // slow drift above offsets the 12: 11.8 -> 11).
        m.push(12.0, 5.9);
        assert_eq!(m.take_whole(), Some((11, 5)));
        assert!((m.fx - 0.8).abs() < 1e-9);
        assert!((m.fy - 0.9).abs() < 1e-9);
    }
}