//! Motion accumulation shared by every capture backend and the client's
//! injection pacing.
//!
//! Device input arrives as raw deltas at device rate (up to 1000 Hz for a
//! modern mouse). Forwarding one message per raw event floods the wire
//! with ~17-byte frames and turns any network load into visible cursor
//! jitter. [`PendingMotion`] merges deltas and emits whole pixels at a
//! fixed cadence ([`MOTION_PERIOD`]) — the total motion is identical,
//! but the frame rate is capped well above what the eye can follow.
//!
//! The same accumulator paces the **client's** injection: relative motion
//! arrives over the network in clumps (UDP bursts, scheduling jitter),
//! and injecting each arrival as it lands makes the cursor jump in
//! syncopation with the network. Draining through [`PendingMotion`]
//! re-spreads a clump at the fixed cadence, so the visible cursor tracks
//! the hand smoothly no matter how the wire delivers the frames — and the
//! client OS's pointer transform sees the same per-frame deltas the
//! server's capture produced, so its acceleration profile stays honest.

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
    /// calling loop at its poll cadence, so motion is never held back
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

    /// Whether whole pixels are pending (a non-zero amount of motion
    /// accumulated but not yet emitted). Callers use it to decide whether
    /// to wake early and drain.
    pub fn has_pending(&self) -> bool {
        self.fx.trunc() != 0.0 || self.fy.trunc() != 0.0
    }
}

/// Paced forwarding of *already-quantized* motion frames (the client's
/// injection half).
///
/// Relative motion arrives over the network in clumps — UDP bursts,
/// scheduling jitter — and injecting each arrival as it lands makes the
/// visible cursor jump in syncopation with the wire. Each incoming frame
/// is already one [`MOTION_PERIOD`] quantum (the server coalesces at the
/// same cadence), so frames must be **replayed, not summed**: emitting
/// one frame per period re-spreads a clump over its natural duration,
/// keeps the client OS's pointer transform honest, and adds no latency
/// when the wire is healthy (one frame per period in, one out).
#[derive(Debug, Default)]
pub struct PacedFrames {
    frames: std::collections::VecDeque<(i32, i32)>,
}

impl PacedFrames {
    /// Queue one incoming motion frame.
    pub fn push(&mut self, dx: i32, dy: i32) {
        self.frames.push_back((dx, dy));
    }

    /// Emit one queued frame at [`MOTION_PERIOD`] cadence, if any is due.
    /// Callers run this on every loop iteration, so a clump drains
    /// smoothly even when no new frames are arriving.
    pub fn flush(&mut self, last: &mut Instant, send: &mut dyn FnMut(i32, i32)) {
        if self.frames.is_empty() {
            return;
        }
        let now = Instant::now();
        if now.duration_since(*last) < MOTION_PERIOD {
            return;
        }
        *last = now;
        if let Some((dx, dy)) = self.frames.pop_front() {
            send(dx, dy);
        }
    }

    /// Emit every queued frame immediately, ignoring the cadence. Used
    /// before ordering-critical events (a click, a key, control leaving)
    /// so the motion that preceded them lands first.
    pub fn drain_now(&mut self, send: &mut dyn FnMut(i32, i32)) {
        while let Some((dx, dy)) = self.frames.pop_front() {
            send(dx, dy);
        }
    }

    /// Whether any frame is queued. Callers use it to wake early so a
    /// clump keeps draining even on a quiet wire.
    pub fn has_pending(&self) -> bool {
        !self.frames.is_empty()
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

    #[test]
    fn has_pending_sees_only_whole_pixels() {
        let mut m = PendingMotion::default();
        assert!(!m.has_pending());
        m.push(0.4, 0.0);
        assert!(!m.has_pending(), "sub-pixel motion is not pending");
        m.push(0.7, 0.0);
        assert!(m.has_pending(), "a whole pixel is pending");
        assert_eq!(m.take_whole(), Some((1, 0)));
        assert!(!m.has_pending());
    }

    #[test]
    fn paced_frames_replay_a_clump_one_period_at_a_time() {
        let mut p = PacedFrames::default();
        // Initialize as if a frame went out one period ago, so the first
        // queued frame emits immediately (nothing is ever delayed by the
        // startup instant).
        let mut last = Instant::now() - MOTION_PERIOD;
        let mut sent: Vec<(i32, i32)> = Vec::new();
        // A burst of three frames (a network clump) arrives at once.
        p.push(10, 0);
        p.push(20, 0);
        p.push(30, 0);
        assert!(p.has_pending());
        // First flush emits one frame immediately (nothing sent yet).
        p.flush(&mut last, &mut |dx, dy| sent.push((dx, dy)));
        assert_eq!(sent, vec![(10, 0)]);
        assert!(p.has_pending());
        // Immediately flushing again is not due yet: no double-pace.
        p.flush(&mut last, &mut |dx, dy| sent.push((dx, dy)));
        assert_eq!(sent, vec![(10, 0)]);
        // After a period, the next frame goes out — and so on, so the
        // clump is re-spread over its natural duration.
        std::thread::sleep(MOTION_PERIOD + std::time::Duration::from_millis(2));
        p.flush(&mut last, &mut |dx, dy| sent.push((dx, dy)));
        assert_eq!(sent, vec![(10, 0), (20, 0)]);
        std::thread::sleep(MOTION_PERIOD + std::time::Duration::from_millis(2));
        p.flush(&mut last, &mut |dx, dy| sent.push((dx, dy)));
        assert_eq!(sent, vec![(10, 0), (20, 0), (30, 0)]);
        assert!(!p.has_pending());
    }

    #[test]
    fn paced_frames_drain_now_flushes_ordering_critical_motion() {
        let mut p = PacedFrames::default();
        p.push(3, 0);
        p.push(4, 0);
        let mut sent = Vec::new();
        p.drain_now(&mut |dx, dy| sent.push((dx, dy)));
        assert_eq!(sent, vec![(3, 0), (4, 0)]);
        assert!(!p.has_pending());
    }
}