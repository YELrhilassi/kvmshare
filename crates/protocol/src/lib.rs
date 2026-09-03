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

/// Current protocol version. Bump on any breaking wire change.
pub const VERSION: u16 = 2;

/// Maximum payload we will accept on the wire. Guards against
/// corrupt length fields allocating absurd buffers.
pub const MAX_PAYLOAD: u32 = 8 * 1024 * 1024; // 8 MiB (clipboard payloads)