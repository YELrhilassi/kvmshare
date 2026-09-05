//! Motion accumulation and cursor steering shared by the capture (server)
//! and injection (client) halves.
//!
//! Device input arrives as raw deltas at device rate (up to 1000 Hz for a
//! modern mouse). Forwarding one message per raw event floods the wire
//! with ~17-byte frames and turns any network load into visible cursor
//! jitter. [`PendingMotion`] (the server's capture) merges deltas and
//! emits whole pixels at a fixed cadence ([`MOTION_PERIOD`]) — the total
//! motion is identical, but the frame rate is capped well above what the
//! eye can follow.
//!
//! The client half is a **closed loop** ([`PositionFollower`]): received
//! frames advance a commanded position and are injected verbatim, and
//! every tick the real cursor is corrected toward the command with a
//! damped move. The client OS's own pointer transform (acceleration)
//! therefore cannot make the cursor run away from the hand — an
//! over-moving OS is corrected back, a lost frame is pushed forward, and
//! no replay queue exists for a backlog to form in.


/// Minimum gap between forwarded motion messages. ~250 Hz keeps motion
/// perfectly smooth while cutting frame count 4x (and far more for very
/// fast devices). The total delta is preserved — only the message rate
/// changes.
pub const MOTION_PERIOD: std::time::Duration = std::time::Duration::from_millis(4);

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

/// Motion pending forwarding: raw (fractional) deltas merged between
/// sends, emitted as whole pixels at [`MOTION_PERIOD`] cadence. The
/// fractional remainder is kept so slow moves still accumulate into full
/// pixels instead of being truncated away.
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



/// The shared cursor on the client is steered by a **closed loop**: every
/// received motion frame advances a commanded position and is injected
/// verbatim (zero added latency), and each loop tick the real cursor
/// position is compared against the command — any residual error
/// (the OS's pointer acceleration over-moving, a lost frame
/// under-moving, a backlog that formed while the loop stalled) is
/// corrected with a damped injection.
///
/// This replaces the old open-loop design — queue the frames, replay one
/// per tick — which was only as good as the tick cadence: when the loop
/// woke late (Windows timer granularity, a clipboard read stalling), the
/// queue became a backlog and the visible cursor crawled behind the hand,
/// kept moving in the old direction after a reversal, and then jumped to
/// catch up. A closed loop has no queue and no backlog: the command is
/// wherever the hand is *now*, and the damped correction converges on it
/// every tick regardless of how the wire or the OS behaved.
#[derive(Debug)]
pub struct PositionFollower {
    /// Where the server wants the cursor, in local screen pixels
    /// (fractional: command deltas and corrections accumulate exactly).
    tx: f64,
    ty: f64,
    /// Whether a command is being followed (control is on this machine).
    active: bool,
    /// Sub-pixel remainder of injected corrections (truncation carry).
    fx: f64,
    fy: f64,
}

impl Default for PositionFollower {
    fn default() -> Self {
        Self { tx: 0.0, ty: 0.0, active: false, fx: 0.0, fy: 0.0 }
    }
}

/// Fraction of a received frame injected immediately (feedforward).
/// Bounded by 1/max_plant_gain: the client OS's acceleration can at most
/// ~double relative input (Windows EPP measured ≈1.85× at speed), so
/// injecting half the frame can never overshoot — a 2x plant turns it
/// into exactly the full frame, and anything less leaves the rest to the
/// closed-loop correction. This keeps the cursor close to the hand at
/// speed (low steady-state lag) without the sawtooth an un-damped 1:1
/// feedforward produces against an amplifying plant.
const FEED_FORWARD: f64 = 0.5;
/// Fraction of the residual error corrected per tick. Kept well below
/// 1.0 so a plant that *over*-moves (Windows EPP amplifies relative
/// input up to ~2x) can never oscillate: injecting gain×error against a
/// 2x plant moves 2·gain×error — with gain 0.4 that is 0.8×error per
/// tick, an asymptotically stable approach.
const FOLLOW_GAIN: f64 = 0.4;
/// Largest single-tick correction in either axis (pixels). Bounds the
/// effect of a transiently invisible cursor (an elevated window) and
/// keeps a recovery from ever looking like a teleport.
const FOLLOW_MAX_STEP: f64 = 32.0;
/// Largest correction flushed before an ordering-critical event (px).
/// Clicks must land where the motion pointed, but a wedged cursor must
/// not be able to drag the click across the screen.
const FOLLOW_FLUSH_CAP: f64 = 64.0;

impl PositionFollower {
    /// Control entered at `(x, y)`: the command starts there.
    pub fn enter(&mut self, x: i32, y: i32) {
        self.tx = x as f64;
        self.ty = y as f64;
        self.active = true;
        self.fx = 0.0;
        self.fy = 0.0;
    }

    /// Control left: stop following.
    pub fn leave(&mut self) {
        self.active = false;
        self.fx = 0.0;
        self.fy = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// One received motion frame: advance the command by the full frame
    /// and return the capped feedforward portion for the caller to inject
    /// immediately. The remainder stays in the command-vs-real error and
    /// is delivered by the damped corrections, so an amplifying OS can
    /// never make the cursor run past the hand. A healthy 1:1 wire still
    /// tracks with only a few ticks of convergence.
    pub fn push(&mut self, dx: i32, dy: i32) -> (i32, i32) {
        self.tx += dx as f64;
        self.ty += dy as f64;
        self.fx += dx as f64 * FEED_FORWARD;
        self.fy += dy as f64 * FEED_FORWARD;
        let ix = self.fx.trunc() as i32;
        let iy = self.fy.trunc() as i32;
        if ix == 0 && iy == 0 {
            return (0, 0);
        }
        self.fx -= ix as f64;
        self.fy -= iy as f64;
        (ix, iy)
    }

    /// Advance the command by a full frame without injecting anything.
    /// Used by absolute-placement backends ([`Injector::absolute_motion`])
    /// where the caller places the cursor at the whole command itself —
    /// the command stays the single source of truth for telemetry and
    /// ordering flushes, but no feedforward fraction is carved out (the
    /// placement is exact, so there is no remainder to feed forward).
    pub fn advance(&mut self, dx: i32, dy: i32) {
        self.tx += dx as f64;
        self.ty += dy as f64;
    }

    /// Absolute re-anchor (defensive placement): the command and the
    /// real cursor both move to `(x, y)`.
    pub fn reanchor(&mut self, x: i32, y: i32) {
        self.tx = x as f64;
        self.ty = y as f64;
        self.fx = 0.0;
        self.fy = 0.0;
    }

    /// The current commanded position, rounded to whole pixels. Used by
    /// absolute-placement backends, which place the cursor here every
    /// tick (the placement is the loop).
    pub fn command(&self) -> (i32, i32) {
        (self.tx.round() as i32, self.ty.round() as i32)
    }

    /// The damped correction toward the command given where the real
    /// cursor is now. `None` when idle or already on target. The caller
    /// injects the returned counts; sub-pixel remainder is carried so
    /// small errors still converge instead of truncating to zero.
    pub fn correct(&mut self, real: (i32, i32)) -> Option<(i32, i32)> {
        if !self.active {
            return None;
        }
        let ex = (self.tx - real.0 as f64) * FOLLOW_GAIN;
        let ey = (self.ty - real.1 as f64) * FOLLOW_GAIN;
        let ex = ex.clamp(-FOLLOW_MAX_STEP, FOLLOW_MAX_STEP);
        let ey = ey.clamp(-FOLLOW_MAX_STEP, FOLLOW_MAX_STEP);
        if ex == 0.0 && ey == 0.0 {
            return None;
        }
        self.fx += ex;
        self.fy += ey;
        let ix = self.fx.trunc() as i32;
        let iy = self.fy.trunc() as i32;
        if ix == 0 && iy == 0 {
            return None;
        }
        self.fx -= ix as f64;
        self.fy -= iy as f64;
        Some((ix, iy))
    }

    /// Flush the whole residual error as one move (capped). Used before
    /// ordering-critical events so a click lands on the command point
    /// even if the cursor was still converging.
    pub fn flush(&mut self, real: (i32, i32)) -> Option<(i32, i32)> {
        if !self.active {
            return None;
        }
        let ex = (self.tx - real.0 as f64).clamp(-FOLLOW_FLUSH_CAP, FOLLOW_FLUSH_CAP);
        let ey = (self.ty - real.1 as f64).clamp(-FOLLOW_FLUSH_CAP, FOLLOW_FLUSH_CAP);
        self.fx = 0.0;
        self.fy = 0.0;
        if ex == 0.0 && ey == 0.0 {
            return None;
        }
        Some((ex.round() as i32, ey.round() as i32))
    }

    /// The residual error (command minus real), for telemetry: a healthy
    /// follower keeps this near zero; it grows when the cursor cannot
    /// keep up (blocked injection, a stalled loop) — the signal that
    /// windowed magnitude measurements hide.
    pub fn error(&self, real: (i32, i32)) -> (i32, i32) {
        ((self.tx - real.0 as f64) as i32, (self.ty - real.1 as f64) as i32)
    }
}

/// Live motion telemetry: what was *requested* versus where the real
/// cursor actually went.
///
/// A KVM forwards relative counts and lets each client OS apply its own
/// pointer transform (speed / acceleration settings). When the client's
/// transform is far from the server's, the shared cursor feels wrong on
/// that machine — crawling "through gel" when the client maps counts to
/// fewer pixels than the server does. This probe makes the mismatch
/// visible instead of a feeling:
///
/// * **requested** — the counts this side asked the OS to move (the
///   injected frames, or on the server the raw device counts).
/// * **actual** — how far the *real* cursor travelled in the same window
///   (post-transform, what the OS actually did with those counts).
///
/// The ratio actual/requested per window is that OS's effective
/// pixels-per-count at the speed of that window — the number to compare
/// between machines. The probe logs one line per window at **trace**
/// level only when a window had any motion, so normal operation and
/// lower log levels are untouched. It runs off the hot path: it is fed
/// by the caller at the counting points and sampled on the caller's own
/// cadence.
#[derive(Debug)]
pub struct MotionProbe {
    /// Window length: requested/actual are compared over this period.
    interval: std::time::Duration,
    last: std::time::Instant,
    /// Set while a measurement session is open (control entered).
    active: bool,
    /// Real cursor position when the session opened — the anchor both
    /// the expected trajectory and window deltas start from.
    anchor: (i32, i32),
    /// Expected position (anchor + cumulative requested counts).
    exp: (i64, i64),
    /// Counts requested since the last window started.
    req_win: (i64, i64),
    /// Real cursor position when the last window started.
    real_win: (i32, i32),
}

impl Default for MotionProbe {
    fn default() -> Self {
        Self::new(std::time::Duration::from_millis(100))
    }
}

impl MotionProbe {
    /// A probe that compares requested and actual motion over `interval`
    /// windows.
    pub fn new(interval: std::time::Duration) -> Self {
        Self {
            interval,
            last: std::time::Instant::now(),
            active: false,
            anchor: (0, 0),
            exp: (0, 0),
            req_win: (0, 0),
            real_win: (0, 0),
        }
    }

    /// Open a session anchored at the real cursor position (control
    /// entered). Both the expected trajectory and the window deltas
    /// start from here.
    pub fn enter(&mut self, real: (i32, i32)) {
        self.active = true;
        self.anchor = real;
        self.exp = (0, 0);
        self.req_win = (0, 0);
        self.real_win = real;
        self.last = std::time::Instant::now();
    }

    /// Close the session (control left).
    pub fn leave(&mut self) {
        self.active = false;
    }

    /// Count one requested motion frame (`dx`, `dy` counts).
    pub fn requested(&mut self, dx: i32, dy: i32) {
        self.exp.0 += dx as i64;
        self.exp.1 += dy as i64;
        self.req_win.0 += dx as i64;
        self.req_win.1 += dy as i64;
    }

    /// Sample the real cursor position. When a full window has elapsed
    /// and the window had motion, calls `emit` with the observation:
    /// `(req_dx, req_dy, act_dx, act_dy, exp_x, exp_y, real_x, real_y)`
    /// where `exp` is the anchor + cumulative requested counts and
    /// `real` is where the cursor actually is — the two trajectories
    /// plotted side by side show the OS transform directly.
    ///
    /// The probe lazily anchors on the first sample it ever sees (a
    /// server-side probe is never explicitly `enter`ed), and skips
    /// windows with no motion entirely.
    pub fn sample(&mut self, real: (i32, i32), emit: &mut dyn FnMut(i64, i64, i64, i64, i64, i64, i64, i64)) {
        if !self.active {
            // First observation anchors the window deltas silently (and
            // discards any counts that accumulated before it, so the
            // first window starts clean).
            self.active = true;
            self.anchor = real;
            self.real_win = real;
            self.req_win = (0, 0);
            self.last = std::time::Instant::now();
            return;
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last) < self.interval {
            return;
        }
        self.last = now;
        let (ax, ay) = (real.0 - self.real_win.0, real.1 - self.real_win.1);
        let idle = self.req_win == (0, 0) && ax == 0 && ay == 0;
        self.real_win = real;
        let (rx, ry) = self.req_win;
        self.req_win = (0, 0);
        if idle {
            return;
        }
        emit(rx, ry, ax as i64, ay as i64, self.exp.0, self.exp.1, real.0 as i64, real.1 as i64);
    }

    /// Whether a sample is due now (used to avoid a real-position query
    /// on the hot path when the window has not elapsed). True whenever
    /// the window has elapsed — even before the first sample — so an
    /// un-entered probe can lazy-anchor on its first due call.
    pub fn due(&self) -> bool {
        std::time::Instant::now().duration_since(self.last) >= self.interval
    }

    /// Whether a measurement session is open (between `enter` and
    /// `leave`, or after the probe lazy-anchored on its first sample).
    pub fn active(&self) -> bool {
        self.active
    }
}

#[cfg(test)]
mod gain_tests {
    use super::*;

    #[test]
    fn measures_px_per_count_from_delta_and_beacon_windows() {
        let mut g = GainTracker::new();
        assert_eq!(g.gain(), 1.0, "neutral until measured");
        // The first beacon anchors the window (nothing to measure
        // against yet); 100 counts of raw motion, real cursor travelled
        // 50 px: 0.5 px/count (a slow-speed libinput curve).
        g.on_beacon(0, 0);
        for _ in 0..25 {
            g.on_delta(4, 0);
        }
        let gain = g.on_beacon(50, 0);
        assert!((gain - 0.5).abs() < 0.01, "window sets the gain, got {gain}");
        assert_eq!(g.gain(), gain);
    }

    #[test]
    fn skips_idle_and_jump_windows() {
        let mut g = GainTracker::new();
        g.on_beacon(0, 0); // anchor
        // Idle: almost no counts, no travel — the gain must not move
        // from 1.0 (a parked cursor would otherwise read as 0 px/count).
        g.on_delta(1, 0);
        assert_eq!(g.on_beacon(1, 0), 1.0);
        // A jump (a crossing park/warp: real travel far beyond the
        // counts) must be excluded, not averaged in.
        g.on_delta(5, 0);
        assert_eq!(g.on_beacon(400, 0), 1.0, "jump windows are skipped");
    }

    #[test]
    fn smooths_toward_the_true_transform() {
        let mut g = GainTracker::new();
        // Ten windows of 100 counts / 60 px travel each: the estimate
        // must converge near 0.6 and stay there (not drift per window).
        let mut target = 0;
        for _ in 0..10 {
            for _ in 0..25 {
                g.on_delta(4, 0);
            }
            target += 60; // the real cursor advances 60 px per window
            g.on_beacon(target, 0);
        }
        assert!((g.gain() - 0.6).abs() < 0.1, "converged near 0.6, got {}", g.gain());
    }

    #[test]
    fn gain_is_frozen_while_no_beacons_flow() {
        // While the cursor is on a client the local cursor is parked:
        // deltas keep arriving but no beacons close a window — the gain
        // must stay exactly where it was.
        let mut g = GainTracker::new();
        for _ in 0..25 {
            g.on_delta(4, 0);
        }
        g.on_beacon(50, 0);
        let frozen = g.gain();
        for _ in 0..100 {
            g.on_delta(9, 0); // remote-era motion, no beacons
        }
        assert_eq!(g.gain(), frozen);
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn probe_reports_requested_vs_actual_per_window() {
        let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
        let mut out = Vec::new();
        p.enter((100, 200));
        p.requested(10, 0);
        p.requested(20, 5);
        // Window elapsed, real cursor moved only 6 px for the 30
        // requested counts: a 0.2 px/count transform (the gel signature).
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((106, 201), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert_eq!(out.len(), 1);
        let (req_x, req_y, act_x, act_y, exp_x, exp_y, real_x, real_y) = out[0];
        assert_eq!((req_x, req_y), (30, 5), "requested counts for the window");
        assert_eq!((act_x, act_y), (6, 1), "actual travel in the window");
        assert_eq!((exp_x, exp_y), (30, 5), "expected = anchor + requested");
        assert_eq!((real_x, real_y), (106, 201), "real position");
    }

    #[test]
    fn probe_lazy_anchors_when_never_entered() {
        // A server-side probe is never explicitly entered: its first due
        // sample must silently anchor and the next one measure.
        let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
        let mut out = Vec::new();
        // Counts before the first sample are discarded, not measured.
        p.requested(999, 999);
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((50, 50), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert!(out.is_empty(), "first sample only anchors");
        assert!(p.active());
        p.requested(100, 0);
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((80, 50), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert_eq!(out.len(), 1);
        let (req_x, req_y, act_x, act_y, ..) = out[0];
        assert_eq!((req_x, req_y), (100, 0), "only post-anchor counts measured");
        assert_eq!((act_x, act_y), (30, 0));
    }

    #[test]
    fn probe_is_silent_without_motion_or_when_inactive() {
        let mut p = MotionProbe::new(std::time::Duration::from_millis(10));
        let mut out = Vec::new();
        // Inactive: never logs.
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((5, 5), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert!(out.is_empty());
        // Active but idle: window with no motion logs nothing.
        p.enter((0, 0));
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((0, 0), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert!(out.is_empty());
        // Leaving closes the session.
        p.leave();
        p.requested(1, 1);
        std::thread::sleep(std::time::Duration::from_millis(15));
        p.sample((1, 1), &mut |a, b, c, d, e, f, g, h| out.push((a, b, c, d, e, f, g, h)));
        assert!(out.is_empty());
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

    /// Simulate a closed-loop plant: `gain` is the OS transform (1.0 = a
    /// faithful 1:1 OS, 2.0 = Windows EPP doubling fast input). Each
    /// `correct` injection moves the simulated real cursor by
    /// `gain`×counts, then the caller re-reads it — exactly how the real
    /// loop behaves.
    struct Plant {
        real: (f64, f64),
        gain: f64,
    }

    impl Plant {
        fn new(gain: f64) -> Self {
            Self { real: (0.0, 0.0), gain }
        }
        fn move_rel(&mut self, dx: i32, dy: i32) {
            self.real.0 += dx as f64 * self.gain;
            self.real.1 += dy as f64 * self.gain;
        }
        fn position(&self) -> (i32, i32) {
            (self.real.0.round() as i32, self.real.1.round() as i32)
        }
    }

    /// Drive a follower to convergence against a plant, the way the
    /// client loop does: push a feedforward frame, then tick `correct`
    /// until the real cursor stops moving.
    fn converge(f: &mut PositionFollower, plant: &mut Plant, max_ticks: usize) -> usize {
        for i in 0..max_ticks {
            let before = plant.position();
            if let Some((dx, dy)) = f.correct(before) {
                plant.move_rel(dx, dy);
            }
            let after = plant.position();
            if before == after && f.error(before) == (0, 0) {
                return i;
            }
        }
        max_ticks
    }

    #[test]
    fn follower_tracks_command_and_absorbs_os_gain() {
        // Half feedforward on a 1:1 OS: the frame lands halfway, and the
        // closed loop delivers the rest. Converges, no overshoot.
        let mut f = PositionFollower::default();
        f.enter(100, 100);
        let mut plant = Plant::new(1.0);
        plant.real = (100.0, 100.0);
        let (dx, dy) = f.push(50, -20);
        plant.move_rel(dx, dy);
        assert_eq!(plant.position(), (125, 90));
        assert_eq!(f.error(plant.position()), (25, -10));
        let ticks = converge(&mut f, &mut plant, 200);
        assert!(ticks < 200, "follower must converge");
        assert_eq!(f.error(plant.position()), (0, 0));
        // Settled: no more corrections (no limit cycle).
        assert_eq!(f.correct(plant.position()), None);

        // A 2x OS (Windows EPP): half feedforward against a 2x plant
        // lands exactly on the command (0.5 x 2 = 1.0) — the OS can
        // never run the cursor past the hand, and there is nothing left
        // to correct.
        let mut f = PositionFollower::default();
        f.enter(100, 100);
        let mut plant = Plant::new(2.0);
        plant.real = (100.0, 100.0);
        let (dx, dy) = f.push(50, -20);
        plant.move_rel(dx, dy);
        assert_eq!(plant.position(), (150, 80), "0.5 ff x 2x gain = full frame");
        assert_eq!(f.error(plant.position()), (0, 0));
        assert_eq!(f.correct(plant.position()), None, "no residual, no correction");
    }

    #[test]
    fn follower_recovers_from_a_lost_frame() {
        // The hand moved 40px but one frame (10px) was lost on the wire:
        // the command says 40, the real cursor only travelled 30 (1:1
        // OS). The follower must push the remaining 10px.
        let mut f = PositionFollower::default();
        f.enter(0, 0);
        let mut plant = Plant::new(1.0);
        let _ = f.push(30, 0); // received frames
        plant.move_rel(30, 0); // 30px applied; 10px lost before arrival
        let (cmd, _) = f.push(10, 0); // this frame never arrives at the OS
        let _ = cmd;
        let err = f.error(plant.position());
        assert_eq!(err, (10, 0), "command (40) minus real (30)");
        let ticks = converge(&mut f, &mut plant, 100);
        assert!(ticks < 100);
        assert_eq!(f.error(plant.position()), (0, 0), "lost motion is recovered");
    }

    #[test]
    fn follower_has_no_direction_persistence() {
        // The queue-replay failure mode this design eliminates: after a
        // direction reversal the cursor must immediately chase the new
        // command — there is no backlog of old-direction frames to drain.
        let mut f = PositionFollower::default();
        f.enter(500, 0);
        let mut plant = Plant::new(1.0);
        plant.real = (500.0, 0.0);
        // Fast sweep left (feedforward is half a frame; the closed loop
        // delivers the rest), then an instant reversal right.
        let (dx, _) = f.push(-100, 0);
        plant.move_rel(dx, 0);
        let (dx, _) = f.push(-100, 0);
        plant.move_rel(dx, 0);
        assert_eq!(plant.position(), (400, 0));
        // Reverse: the very next injected motion must already point
        // right — no backlog of old-direction frames to drain.
        let (dx, _) = f.push(100, 0);
        plant.move_rel(dx, 0);
        let (dx, _) = f.push(100, 0);
        plant.move_rel(dx, 0);
        assert_eq!(plant.position(), (500, 0));
        // The remaining residual converges immediately.
        let ticks = converge(&mut f, &mut plant, 200);
        assert!(ticks < 200);
        assert_eq!(f.error(plant.position()), (0, 0));
    }

    #[test]
    fn follower_survives_a_stalled_loop_without_teleporting() {
        // The loop stalled (Windows timer granularity, a slow clipboard
        // read): 40 frames of motion arrive late in one burst. The
        // corrections are capped per tick, so the cursor converges
        // smoothly instead of one giant jump — and it still catches up.
        let mut f = PositionFollower::default();
        f.enter(0, 0);
        let mut plant = Plant::new(1.0);
        for _ in 0..40 {
            let (dx, dy) = f.push(5, 0);
            plant.move_rel(dx, dy);
        }
        // But the real cursor never moved (the loop was stalled and the
        // injections above never actually reached the OS).
        plant.real = (0.0, 0.0);
        let err = f.error(plant.position());
        assert_eq!(err.0, 200);
        // First correction is bounded by the max step, not 200px.
        let (dx, _) = f.correct((0, 0)).expect("correction due");
        assert!(dx.abs() <= 32, "recovery must be capped, got {dx}");
        let ticks = converge(&mut f, &mut plant, 200);
        assert!(ticks < 200);
        assert_eq!(f.error(plant.position()), (0, 0));
    }

    #[test]
    fn follower_leave_stops_correcting_and_flush_lands_clicks() {
        let mut f = PositionFollower::default();
        f.enter(0, 0);
        assert!(f.is_active());
        let mut plant = Plant::new(1.0);
        let _ = f.push(10, 0);
        plant.move_rel(6, 0); // OS under-applied (say, one frame lost)
        // Ordering-critical event: flush the whole residual so the click
        // lands on the command point.
        let (dx, dy) = f.flush(plant.position()).expect("residual to flush");
        plant.move_rel(dx, dy);
        assert_eq!(plant.position(), (10, 0));
        assert_eq!(f.error(plant.position()), (0, 0));
        // Leaving stops all following.
        f.leave();
        assert!(!f.is_active());
        assert_eq!(f.correct((0, 0)), None);
        assert_eq!(f.flush((0, 0)), None);
    }
}