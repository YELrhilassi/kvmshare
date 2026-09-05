//! Constants used by the wire protocol.

/// Magic bytes at the start of every frame: `K V M 1`.
pub const MAGIC: [u8; 4] = *b"KVM1";

/// Message type ids. Kept as plain `u8` constants so the wire format
/// is trivial to document and debug with a hex dump.
pub mod types {
    pub const HELLO: u8 = 0x01;
    pub const WELCOME: u8 = 0x02;
    pub const SCREEN_INFO: u8 = 0x03;
    pub const LAYOUT: u8 = 0x04;
    pub const ENTER: u8 = 0x10;
    pub const LEAVE: u8 = 0x11;
    /// Client → server: the client's *real* cursor position while it is
    /// being controlled (its OS applies its own pointer acceleration to
    /// the relative motion it receives, so the real position is the only
    /// ground truth for edge crossings — the same role the server's own
    /// position beacons play on the local screen).
    pub const CURSOR_POS: u8 = 0x14;
    /// Local-only (capture → session): the user pressed the escape key
    /// while the cursor was on a client. Never sent over the wire.
    pub const ESCAPE: u8 = 0x13;
    pub const MOUSE_MOVE_ABS: u8 = 0x20;
    pub const MOUSE_MOVE_REL: u8 = 0x21;
    pub const MOUSE_BUTTON: u8 = 0x22;
    pub const MOUSE_WHEEL: u8 = 0x23;
    pub const KEY: u8 = 0x30;
    pub const CLIPBOARD: u8 = 0x40;
    pub const KEEPALIVE: u8 = 0x7e;
    pub const ERROR: u8 = 0x7f;
}

/// Frame flags.
pub mod flags {
    /// Payload is compressed (reserved; not used yet).
    pub const COMPRESSED: u8 = 0x01;
}

/// Canonical mouse button ids. The wire always uses these; each platform
/// backend maps them to its native representation.
pub mod buttons {
    pub const LEFT: u8 = 0;
    pub const MIDDLE: u8 = 1;
    pub const RIGHT: u8 = 2;
    pub const EXTRA_1: u8 = 3;
    pub const EXTRA_2: u8 = 4;
}

/// Key kinds carried by the [`super::message::Message::Key`] message.
pub mod keys {
    pub const DOWN: u8 = 0;
    pub const UP: u8 = 1;
    pub const REPEAT: u8 = 2;
}

/// Error codes for the [`super::message::Message::Error`] message.
pub mod errors {
    pub const PROTOCOL: u8 = 1;
    pub const VERSION_MISMATCH: u8 = 2;
    pub const NAME_CONFLICT: u8 = 3;
    pub const INTERNAL: u8 = 4;
}