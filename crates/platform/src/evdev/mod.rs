//! Linux input capture straight from `/dev/input` (evdev) with **kernel
//! device isolation**.
//!
//! Why a second capture path? Apps that read XI2 *raw* events — browsers,
//! smooth-scroll terminals (kitty), TUIs with mouse support — receive
//! them **regardless of any X grab**: core pointer grabs only suppress
//! core delivery, and XI2 device grabs do not suppress raw delivery
//! either (verified empirically on a live X server). The only way to
//! make the local desktop *literally see nothing* while the cursor is on
//! a client is to stop the devices at the kernel with `EVIOCGRAB`.
//!
//! While the cursor is on a client this module:
//!
//! 1. grabs every physical pointer and keyboard (`EVIOCGRAB`), so X
//!    receives zero events — raw included — and no local app can scroll,
//!    hover, or react to the very input being forwarded;
//! 2. reads the devices directly and forwards the same protocol
//!    [`Message`]s the X capture would (motion coalesced at the shared
//!    cadence, wheel one notch per event, buttons, keys, and the Scroll
//!    Lock escape).
//!
//! On return home the grab is released and the X capture resumes. Kernel
//! grabs are released automatically when the process dies, so a crash
//! can never leave the desktop input-dead (unlike an X-side device
//! disable, which persists until re-enabled).
//!
//! ## Permissions and hot-plug
//!
//! Reading `/dev/input` needs the `input` group (or a udev rule). The
//! reader is **always running** and re-enumerates on a cadence in both
//! modes, so access granted later (installer udev rule, group change,
//! device plugged in) is picked up live — no restart, no user steps.
//! Until devices are readable the server runs grab-only: degraded but
//! functional, and logged once on the state change.
//!
//! ## No event replay across a boundary
//!
//! A key or button physically held when the cursor crosses a boundary
//! was pressed on the *other* capture path — the client must never see
//! its stream replayed here. The reader therefore tracks which presses
//! it forwarded and suppresses kernel auto-repeats and releases for
//! anything it did not press. The client releases whatever it does hold
//! when control leaves it (its own `leave` path).
//!
//! ## Portability
//!
//! The module is Linux-only but **X-free**: it speaks only `/dev/input`
//! and the protocol channel, so the same reader slots into a future
//! Wayland capture unchanged.
//!
//! ## Layout
//!
//! * [`device`] — opening, classifying and absorbing devices; kernel
//!   event → protocol message translation.
//! * [`reader`] — the reader thread and the grab/release lifecycle.
//! * [`hotplug`] — the enumerator thread: inotify-driven device scans.

mod device;
mod hotplug;
mod reader;

pub use reader::EvdevReader;