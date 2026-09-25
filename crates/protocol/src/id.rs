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
    /// Server → client: an operational command (disconnect, reconnect,
    /// restart). See [`super::message::Message::Control`].
    pub const CONTROL: u8 = 0x05;
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
    /// A semantic media-control request (play/pause/volume/...),
    /// classified at the capture edge and routed by policy rather than
    /// by cursor focus. See [`crate::message::MediaCommand`].
    pub const MEDIA_CONTROL: u8 = 0x31;
    pub const CLIPBOARD: u8 = 0x40;
    /// "My audio socket is on this port, and these are the formats I can
    /// receive." Each side announces independently, so audio can flow in
    /// one direction, the other, or both.
    pub const AUDIO_OFFER: u8 = 0x50;
    /// "I am now sending you audio, from my port, in this format." Sent
    /// only once the sender has the peer's offer and a common format.
    pub const AUDIO_START: u8 = 0x51;
    /// The audio stream is ending.
    pub const AUDIO_STOP: u8 = 0x52;
    /// Whether this machine currently has something playing — the input to
    /// the media router's `last_active_source` policy.
    pub const AUDIO_STATE: u8 = 0x53;
    pub const KEEPALIVE: u8 = 0x7e;
    pub const ERROR: u8 = 0x7f;
}

/// Media-control command ids carried by
/// [`crate::message::Message::MediaControl`]. Plain `u8` constants, like
/// every other wire id, so a hex dump stays readable.
pub mod media {
    pub const PLAY_PAUSE: u8 = 0;
    pub const PLAY: u8 = 1;
    pub const PAUSE: u8 = 2;
    pub const STOP: u8 = 3;
    pub const NEXT: u8 = 4;
    pub const PREVIOUS: u8 = 5;
    pub const SEEK_FORWARD: u8 = 6;
    pub const SEEK_BACKWARD: u8 = 7;
    pub const VOLUME_UP: u8 = 8;
    pub const VOLUME_DOWN: u8 = 9;
    pub const MUTE: u8 = 10;
}

/// Payload encodings for [`crate::message::AudioFormat`].
///
/// Unknown values are refused at decode time, so this list is the honest
/// answer to "what can a peer send me" — adding a codec means adding a
/// constant here *and* an implementation, in one change.
pub mod codecs {
    /// 16-bit signed little-endian PCM, interleaved. The baseline: no
    /// dependency, no CPU, ~1.5 Mbit/s at 48 kHz stereo.
    pub const PCM_S16LE: u8 = 0;
}

/// Frame flags.
pub mod flags {
    /// Payload is compressed. **Defined but not implemented**: no peer
    /// sets it, and the decoders reject it so a future implementation
    /// cannot silently send frames older peers misread as raw payload.
    pub const COMPRESSED: u8 = 0x01;

    /// Every flag bit any decoder accepts today — **none**, while
    /// compression above is unimplemented. Accepting the COMPRESSED bit
    /// while every decoder ignores it would let a future sender's
    /// "payload is transformed" frames be silently decoded as raw
    /// bytes; rejecting it keeps the semantics honest. When compression
    /// lands, this becomes `COMPRESSED` and the decoders grow the
    /// decompress step in the same change.
    pub const KNOWN: u8 = 0x00;
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
    /// The client's name is not in the server's layout and its machine id
    /// is not trusted — the server's connection policy refused it.
    pub const NOT_ALLOWED: u8 = 5;
    /// The peer is outside the allowed network (the server only accepts
    /// connections from its local network).
    pub const NOT_LOCAL: u8 = 6;
    /// The peer's machine id has been explicitly revoked by this machine.
    /// A hard deny: it outranks the allowlist, the trusted-ids list and a
    /// matching layout screen, so revoking a machine always refuses it.
    pub const REVOKED: u8 = 7;
}

/// Commands carried by the [`super::message::Message::Control`]
/// message. Kept as plain `u8` constants so the wire format is trivial
/// to document and debug.
pub mod control {
    /// End the session and **do not** reconnect — the client process
    /// exits its reconnect loop (the operator must start it again).
    pub const DISCONNECT: u8 = 1;
    /// End the session and reconnect immediately (fresh handshake).
    pub const RECONNECT: u8 = 2;
    /// End the session and reconnect immediately — a session-level
    /// restart of the controlled link.
    pub const RESTART: u8 = 3;
}