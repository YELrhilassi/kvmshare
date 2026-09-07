//! The client's closed-loop cursor follower.
//!
//! The shared cursor on the client is steered by a **closed loop**: every
//! received motion frame advances a commanded position and is injected
//! verbatim (zero added latency), and each loop tick the real cursor
//! position is compared against the command — any residual error
//! (the OS's pointer acceleration over-moving, a lost frame
//! under-moving, a backlog that formed while the loop stalled) is
//! corrected with a damped injection.
//!
//! This replaces the old open-loop design — queue the frames, replay one
//! per tick — which was only as good as the tick cadence: when the loop
//! woke late (Windows timer granularity, a clipboard read stalling), the
//! queue became a backlog and the visible cursor crawled behind the hand,
//! kept moving in the old direction after a reversal, and then jumped to
//! catch up. A closed loop has no queue and no backlog: the command is
//! wherever the hand is *now*, and the damped correction converges on it
//! every tick regardless of how the wire or the OS behaved.

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

/// The closed-loop cursor follower (see the module docs).
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
    /// This machine's screen bounds (width, height), when known. The
    /// commanded position is clamped into them: while the OS pins the
    /// visible cursor at an edge, an unclamped command would keep
    /// running off-screen and the whole overshoot would have to be
    /// walked back before a reversal moved the cursor again. Clamping
    /// keeps the command where the cursor can actually be, so reversing
    /// at an edge moves immediately. `None` until the first screen
    /// geometry is known — the command is then unclamped rather than
    /// mis-clamped to the origin.
    bounds: Option<(u32, u32)>,
}

impl Default for PositionFollower {
    fn default() -> Self {
        Self { tx: 0.0, ty: 0.0, active: false, fx: 0.0, fy: 0.0, bounds: None }
    }
}

impl PositionFollower {
    /// Set (or update) this machine's screen bounds and clamp the
    /// current command into them — a resolution shrink can leave the
    /// command outside the new screen, and the command must never sit
    /// where the visible cursor cannot.
    pub fn set_bounds(&mut self, w: u32, h: u32) {
        self.bounds = Some((w, h));
        self.clamp_command();
    }

    /// Clamp the commanded position into the screen bounds, if known.
    /// A command beyond an edge is exactly the "virtual cursor stuck
    /// past the boundary" failure: the OS pins the visible cursor at
    /// the edge while the command runs off-screen, so a reversal has to
    /// eat the whole overshoot before the cursor moves. Clamped, the
    /// command is already at the edge, and reversing moves immediately.
    /// Degenerate (zero-size) bounds clamp to (0, 0) instead of
    /// panicking (`f64::clamp` requires min <= max).
    fn clamp_command(&mut self) {
        if let Some((w, h)) = self.bounds {
            let max_x = (w as i64 - 1).max(0) as f64;
            let max_y = (h as i64 - 1).max(0) as f64;
            self.tx = self.tx.clamp(0.0, max_x);
            self.ty = self.ty.clamp(0.0, max_y);
        }
    }

    /// Control entered at `(x, y)`: the command starts there.
    pub fn enter(&mut self, x: i32, y: i32) {
        self.tx = x as f64;
        self.ty = y as f64;
        self.active = true;
        self.fx = 0.0;
        self.fy = 0.0;
        self.clamp_command();
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
        self.clamp_command();
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
    /// Used by absolute-placement backends ([`super::super::client::Injector::absolute_motion`])
    /// where the caller places the cursor at the whole command itself —
    /// the command stays the single source of truth for telemetry and
    /// ordering flushes, but no feedforward fraction is carved out (the
    /// placement is exact, so there is no remainder to feed forward).
    pub fn advance(&mut self, dx: i32, dy: i32) {
        self.tx += dx as f64;
        self.ty += dy as f64;
        self.clamp_command();
    }

    /// Absolute re-anchor (defensive placement): the command and the
    /// real cursor both move to `(x, y)`.
    pub fn reanchor(&mut self, x: i32, y: i32) {
        self.tx = x as f64;
        self.ty = y as f64;
        self.fx = 0.0;
        self.fy = 0.0;
        self.clamp_command();
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

#[cfg(test)]
#[path = "follower_tests.rs"]
mod follower_tests;