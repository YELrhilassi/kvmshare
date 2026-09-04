//! Motion accumulation, shared with the core crate.
//!
//! The accumulator lives in `kvmshare-core` because the *client*'s
//! injection pacing needs it too (relative motion arrives over the
//! network in clumps; re-spreading it at the fixed cadence keeps the
//! visible cursor smooth and the client OS's acceleration profile
//! honest). This module is kept as the single import point for the
//! platform's capture backends.

pub use kvmshare_core::motion::{PendingMotion, MOTION_PERIOD};