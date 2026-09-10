//! X11 input capture: raw device events in, local cursor control out.
//!
//! The capture owns one X connection exclusively. It selects XI2 raw
//! events (device deltas/buttons/keys, free of warp feedback) plus
//! ordinary motion (the real, post-acceleration pointer position used as
//! a resync beacon), and it executes every engine command — grab,
//! kernel isolation, warp, cursor hide/show — on that same connection.
//! The engine never touches X directly; it sends [`CaptureCommand`]s
//! over a channel.
//!
//! Motion is coalesced at the shared cadence ([`PendingMotion`]) so the
//! wire carries whole pixels at a fixed rate instead of thousands of
//! raw frames. The real pointer position is polled on a **separate
//! thread with its own connection** so a busy X server can delay those
//! round-trips without ever stalling the cursor stream.
//!
//! While the cursor is on a client, the physical devices are handed to
//! the kernel-level [`crate::evdev`] reader (grab + forward); the X
//! capture keeps running grab-only as the fallback.
//!
//! ## Layout
//!
//! * [`events`] — XI2 event selection and raw-event decoding
//!   (multi-valuator safe), key repeat state, and the command
//!   vocabulary.
//! * [`thread`] — the capture thread: the event loop, event decoding,
//!   motion/beacon flushing, key repeats.
//! * [`commands`] — engine-command execution: warp, cursor visibility,
//!   the input grab, kernel device isolation.
//! * [`beacon`] — the pointer-poll thread that feeds position beacons
//!   from its own connection.

mod beacon;
mod commands;
mod events;
mod thread;

pub use beacon::spawn_beacon_thread;
pub use events::CaptureCommand;
pub use thread::start;
