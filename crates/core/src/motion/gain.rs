//! The pointer-gain calibrator: the server's own pixels-per-count
//! transform, measured from the capture stream.

/// Minimum raw counts in a gain window for it to be measurable.
const GAIN_MIN_REL: f64 = 12.0;
/// Minimum real travel (px) in a gain window for it to be measurable.
const GAIN_MIN_REAL: f64 = 4.0;
/// A window whose real travel exceeds this multiple of its raw counts is
/// a *jump*, not motion (a crossing park, a warp, a grab transition) —
/// excluded so one polluted sample cannot bend the estimate.
const GAIN_MAX_JUMP_FACTOR: f64 = 4.0;
/// The measured px-per-count is clamped to this range. Real transforms
/// (libinput, Windows EPP, accel settings) stay comfortably inside it;
/// the clamp keeps a single pathological window from exploding the gain.
const GAIN_MIN: f64 = 0.25;
const GAIN_MAX: f64 = 3.0;
/// After this many samples the smoother switches from a fast warm-up
/// average to the steady exponential rate.
const GAIN_WARMUP: u32 = 6;
/// Steady-state smoothing: how much one window moves the estimate.
const GAIN_ALPHA: f64 = 0.25;

/// Measures the server's own pointer transform — pixels of real cursor
/// travel per raw device count — from the capture stream.
///
/// The server's OS applies its pointer acceleration (libinput curve,
/// Windows EPP, whatever) to the physical mouse; a client's cursor must
/// travel exactly as far as the server's would for the same hand motion,
/// or the shared cursor visibly changes speed at the seam. The session
/// scales the counts it forwards by this measured gain, so the client
/// (which places its cursor absolutely, 1:1) mirrors the server's cursor
/// pixel-for-pixel — whatever acceleration settings either machine has.
///
/// Fed by the server's main loop: every raw delta into [`GainTracker::on_delta`]
/// and every real-position beacon into [`GainTracker::on_beacon`]. Beacons
/// only fire while the cursor is actually moving locally (the capture
/// forwards *changed* positions), so the estimate is measured while local
/// and frozen (unmeasured) while the cursor is on a client — exactly the
/// value the remote side needs.
#[derive(Debug, Default)]
pub struct GainTracker {
    /// Real cursor position at the last beacon.
    last: Option<(i32, i32)>,
    /// Raw counts seen since the last beacon.
    rel: (i64, i64),
    /// Smoothed px-per-count.
    gain: f64,
    /// Windows measured so far (warm-up ramp).
    samples: u32,
}

impl GainTracker {
    /// A tracker starting from a neutral 1.0 (counts map 1:1 to pixels)
    /// until the first clean local windows refine it.
    pub fn new() -> Self {
        Self { last: None, rel: (0, 0), gain: 1.0, samples: 0 }
    }

    /// Count one raw device delta since the last beacon.
    pub fn on_delta(&mut self, dx: i32, dy: i32) {
        self.rel.0 += dx as i64;
        self.rel.1 += dy as i64;
    }

    /// The real cursor reached `(x, y)`: close the window opened by the
    /// last beacon and update the smoothed gain. Returns the current
    /// estimate (for the caller to hand to the session). Idle and
    /// jump-polluted windows are skipped silently.
    pub fn on_beacon(&mut self, x: i32, y: i32) -> f64 {
        if let Some((px, py)) = self.last {
            let rel = (self.rel.0.abs() + self.rel.1.abs()) as f64;
            let real = ((x - px).abs() + (y - py).abs()) as f64;
            if rel >= GAIN_MIN_REL && real >= GAIN_MIN_REAL && real <= GAIN_MAX_JUMP_FACTOR * rel {
                let window = (real / rel).clamp(GAIN_MIN, GAIN_MAX);
                self.samples += 1;
                let alpha = if self.samples <= GAIN_WARMUP { 1.0 / self.samples as f64 } else { GAIN_ALPHA };
                self.gain += alpha * (window - self.gain);
            }
        }
        self.last = Some((x, y));
        self.rel = (0, 0);
        self.gain
    }

    /// The current px-per-count estimate (1.0 before any measurement).
    pub fn gain(&self) -> f64 {
        self.gain
    }
}

#[cfg(test)]
mod tests;