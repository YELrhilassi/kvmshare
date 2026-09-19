//! # kvmshare-protocol
//!
//! The binary wire protocol used between a kvmshare **server** (the machine
//! whose keyboard/mouse is shared) and **clients** (the machines being
//! controlled).
//!
//! Design goals:
//!
//! * **Fast** — plain binary, no serialization framework, no allocation
//!   churn in the hot path (mouse moves are `i32` pairs).
//! * **Simple** — one frame type, length-prefixed payloads, hand-written
//!   encode/decode that fits in your head.
//! * **Fault-tolerant** — a 4-byte magic in every frame lets a receiver
//!   detect desync and resynchronize instead of hanging.
//!
//! ## Frame layout
//!
//! ```text
//! +--------+---------+--------+------------+-----------------+
//! | magic  |  type   | flags  |  length    |  payload        |
//! | 4 bytes| 1 byte  | 1 byte | u32 BE     | length bytes    |
//! +--------+---------+--------+------------+-----------------+
//! ```
//!
//! Every message fits in a single frame. [`frame::Frame`] owns the raw
//! payload bytes; [`message::Message`] is the typed, decoded form.
//!
//! ## Conventions
//!
//! * Integers are big-endian on the wire.
//! * Strings are length-prefixed (`u32` bytes) UTF-8.
//! * Coordinates are in screen pixels; `+y` is *down* (screen convention).
//! * The server is the authority on the layout and always initiates
//!   enter/leave; clients only report their screen shape.

pub mod frame;
pub mod id;
pub mod message;
pub mod wire;

pub use frame::{Frame, HEADER_LEN, LEN_OFFSET};
pub use message::{KeyKind, Message, ScreenInfo};

/// The wire protocol version this build speaks. Bump on any breaking
/// wire change.
pub const VERSION: u16 = 3;

/// The oldest wire protocol this build can interoperate with. A peer
/// is compatible when its version is in `[MIN_PROTOCOL, MAX_PROTOCOL]`.
/// The range is what makes upgrades and downgrades survivable: a new
/// build speaks to an old one as long as nothing each side actually
/// sends has changed meaning, and the handshake refuses honestly (with
/// the exact versions on both ends in the error text) when they cannot.
///
/// Today the wire has never broken (VERSION 3 since the first release
/// with versioning), so the floor equals the current version. When a
/// breaking change lands: bump VERSION, set MIN_PROTOCOL to the oldest
/// version whose messages this build still reads correctly, and teach
/// the encoders to speak `min(peer, VERSION)` if the floor is below
/// the peer's version.
pub const MIN_PROTOCOL: u16 = 3;

/// The newest wire protocol this build can interoperate with. Equals
/// VERSION except while a future build stages an unreleased protocol
/// (it would advertise a higher MAX before flipping VERSION).
pub const MAX_PROTOCOL: u16 = VERSION;

/// Are two protocol versions interoperable under this build's range?
pub fn compatible(peer_version: u16) -> bool {
    (MIN_PROTOCOL..=MAX_PROTOCOL).contains(&peer_version)
}

/// Maximum payload we will accept on the wire. Guards against
/// corrupt length fields allocating absurd buffers.
pub const MAX_PAYLOAD: u32 = 8 * 1024 * 1024; // 8 MiB (clipboard payloads)