//! UDP datagrams for the cursor stream.
//!
//! Most of a KVM link is **relative mouse motion** (server → client) and
//! **real-cursor beacons** (client → server). Both are *additive and
//! loss-tolerant*: a dropped delta just means the cursor travels a few
//! pixels less on that frame, and the next frame continues from wherever
//! it actually is — there is nothing to retransmit. Sending them over TCP
//! couples the cursor's latency to the reliable stream's buffering and
//! backpressure (a download, a busy peer, a stalled read turns smooth
//! motion into clumps), so they ride **UDP** instead. Everything that
//! must not be lost or reordered — handshake, Enter/Leave, buttons, keys,
//! wheel, clipboard, layout — stays on TCP (see [`crate::transport`]).
//!
//! Every datagram carries one encoded protocol frame inside a small
//! envelope:
//!
//! ```text
//! [ client id: u8 ] [ seq: u32 BE ] [ frame bytes ]
//! ```
//!
//! The **client id** lets the server route a datagram to the right
//! client; the **sequence number** lets the receiver discard stale and
//! duplicate datagrams. For additive motion a reordered datagram is
//! simply older traffic the peer already moved past — dropping it is
//! exactly equivalent to losing it, which the stream tolerates. For
//! beacons it matters more: a stale "at the wall" report replayed late
//! could arm a crossing the user never pushed for, so beacons must never
//! be applied out of order.
//!
//! Sequence numbers live in a 32-bit space and may wrap; [`is_newer`]
//! uses wrapping comparison so a peer that has sent 2³² frames keeps
//! working.

use kvmshare_protocol::message::Message;

/// Bytes of the envelope that precede the encoded frame.
pub const ENVELOPE_LEN: usize = 1 + 4;

/// Pack one message into a datagram for client `id`.
pub fn pack(id: u8, seq: u32, msg: &Message) -> Vec<u8> {
    let frame = msg.encode();
    let mut out = Vec::with_capacity(ENVELOPE_LEN + frame.len());
    out.push(id);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&frame);
    out
}

/// One decoded datagram.
pub struct Datagram {
    /// The client this datagram belongs to (sender, or intended
    /// receiver).
    pub id: u8,
    /// The sender's sequence number for this stream.
    pub seq: u32,
    pub msg: Message,
}

/// Unpack a datagram. `None` when the bytes are malformed (truncated
/// envelope, bad frame, trailing garbage).
pub fn unpack(bytes: &[u8]) -> Option<Datagram> {
    if bytes.len() < ENVELOPE_LEN {
        return None;
    }
    let id = bytes[0];
    let seq = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
    let msg = Message::decode(&bytes[ENVELOPE_LEN..]).ok()?;
    Some(Datagram { id, seq, msg })
}

/// Is `seq` newer than `last`, in a wrapping 32-bit space? Handles the
/// counter wrapping past u32::MAX (a peer sending frames for years) and
/// treats equal as not-newer (the same frame must never apply twice).
pub fn is_newer(seq: u32, last: u32) -> bool {
    seq != last && seq.wrapping_sub(last) < (1 << 31)
}

#[cfg(test)]
#[path = "udp_tests.rs"]
mod tests;
