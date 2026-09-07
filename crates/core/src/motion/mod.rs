//! Motion accumulation and cursor steering shared by the capture (server)
//! and injection (client) halves.
//!
//! Device input arrives as raw deltas at device rate (up to 1000 Hz for a
//! modern mouse). Forwarding one message per raw event floods the wire
//! with ~17-byte frames and turns any network load into visible cursor
//! jitter. [`pending::PendingMotion`] (the server's capture) merges
//! deltas and emits whole pixels at a fixed cadence ([`MOTION_PERIOD`]) —
//! the total motion is identical, but the frame rate is capped well above
//! what the eye can follow.
//!
//! The client half is a **closed loop** ([`follower::PositionFollower`]):
//! received frames advance a commanded position and are injected verbatim,
//! and every tick the real cursor is corrected toward the command with a
//! damped move. The client OS's own pointer transform (acceleration)
//! therefore cannot make the cursor run away from the hand — an
//! over-moving OS is corrected back, a lost frame is pushed forward, and
//! no replay queue exists for a backlog to form in.
//!
//! The other two pieces are diagnostics and calibration: [`gain::GainTracker`]
//! measures the server's own px-per-count transform, and [`probe::MotionProbe`]
//! makes the requested-vs-actual cursor feel visible in the logs.

mod follower;
mod gain;
mod pending;
mod probe;

pub use follower::PositionFollower;
pub use gain::GainTracker;
pub use pending::PendingMotion;
pub use probe::MotionProbe;

/// Minimum gap between forwarded motion messages. ~250 Hz keeps motion
/// perfectly smooth while cutting frame count 4x (and far more for very
/// fast devices). The total delta is preserved — only the message rate
/// changes.
pub const MOTION_PERIOD: std::time::Duration = std::time::Duration::from_millis(4);