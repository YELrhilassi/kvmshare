//! The server's motion accumulator: raw fractional deltas merged between
//! sends, emitted as whole pixels at the shared cadence.

/// Motion pending forwarding: raw (fractional) deltas merged between
/// sends, emitted as whole pixels at [`crate::motion::MOTION_PERIOD`]
/// cadence. The fractional remainder is kept so slow moves still
/// accumulate into full pixels instead of being truncated away.
#[derive(Debug, Default)]
pub struct PendingMotion {
    fx: f64,
    fy: f64,
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

    /// Forward whatever whole pixels have accumulated, immediately.
    ///
    /// The caller invokes this once per drain pass (the evdev reader
    /// after reading every device, the X11 capture once per poll), so
    /// events that arrive in the same pass coalesce into one message and
    /// nothing is ever held back waiting for a cadence. Holding motion
    /// for a fixed period (the old behaviour) added up to a period of
    /// latency *and* batched events into bursts — the wire then delivered
    /// clumps, and a clumped command stream is exactly the stutter the
    /// eye sees. Emitting per pass keeps the stream as even as the hand.
    pub fn flush(&mut self, send: &mut dyn FnMut(i32, i32)) {
        if let Some((ix, iy)) = self.take_whole() {
            send(ix, iy);
        }
    }

    /// Whether whole pixels are pending (a non-zero amount of motion
    /// accumulated but not yet emitted). Callers use it to decide whether
    /// to wake early and drain.
    pub fn has_pending(&self) -> bool {
        self.fx.trunc() != 0.0 || self.fy.trunc() != 0.0
    }
}

#[cfg(test)]
mod tests;