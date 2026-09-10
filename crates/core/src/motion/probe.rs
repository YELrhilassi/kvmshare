/// Diagnostic: makes the requested-vs-actual cursor feel visible in the
/// logs. The probe compares the counts the server forwarded with the
/// position the client's cursor actually reached, over fixed windows,
/// so a transform mismatch or a laggy leg shows up as a measurable
/// error rather than a vague "it feels off".
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
    pub fn sample(
        &mut self,
        real: (i32, i32),
        emit: &mut dyn FnMut(i64, i64, i64, i64, i64, i64, i64, i64),
    ) {
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
mod tests;