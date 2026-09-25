//! Audio streaming: the platform-independent half.
//!
//! # The shape of the problem
//!
//! Audio flows one way per stream: a sender captures *its own output*,
//! packets it, and the peer plays it. Direction is independent, so both
//! machines can send at once — and because playback goes to the ordinary
//! output device, each OS's own mixer combines the remote stream with local
//! sound. There is no custom mixer, and nothing here needs to know what the
//! audio *is*.
//!
//! # Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`device`] | The OS boundary: capture/playback/list-devices traits |
//! | [`packet`] | Capture bytes → numbered datagrams, and the stream ends that do it |
//! | [`jitter`] | Received datagrams → in-order, on-time frames |
//! | [`activity`] | Is this machine making sound? (drives the media router) |
//! | [`transport`] | The dedicated UDP socket audio rides on |
//! | [`runtime`] | Wiring: one capture thread and one playout thread per role |
//!
//! Each is small, single-purpose, and testable on its own; the runtime is
//! the only piece that owns threads, and it owns nothing else.
//!
//! # Audio gets its own socket
//!
//! Audio does **not** share the cursor stream's UDP socket, and its
//! datagrams do not use the control protocol's frame format. The two have
//! opposite requirements — a few bytes of additive motion routed by client
//! id, versus a fat fixed-cadence payload for one peer — and sharing a
//! socket would put audio behind cursor traffic in the same queue. See
//! [`transport`] and [`packet`].

pub mod activity;
pub mod device;
pub mod jitter;
pub mod packet;
pub mod runtime;
pub mod transport;

pub use activity::ActivityDetector;
pub use device::{AudioCapture, AudioDevices, AudioPlayback};
pub use jitter::{AudioStats, JitterBuffer, PushOutcome};
pub use packet::{Packetiser, Receiver, Sender};
pub use transport::AudioSocket;
