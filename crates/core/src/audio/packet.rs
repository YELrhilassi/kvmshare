//! Turning captured bytes into audio datagrams, and back.
//!
//! # A dedicated datagram format
//!
//! Audio does **not** reuse the cursor stream's frame format, and does not
//! share its socket. The two have opposite requirements — the cursor
//! stream carries a few bytes of additive motion alongside a client id for
//! routing, while audio carries a fat, fixed-cadence payload for exactly
//! one peer — so conflating them would mean audio traffic queued behind
//! cursor traffic and a client id that means nothing on a one-way stream.
//!
//! Instead audio has its own small, self-describing datagram:
//!
//! ```text
//! [ magic: "KVMA" ][ version: u8 ][ seq: u32 BE ][ timestamp_ms: u32 BE ][ payload ]
//! ```
//!
//! The magic and version are 5 bytes out of ~1900, and they are not
//! ceremony: the audio socket is bound to a port on a shared LAN, so a
//! stray datagram from any other program must be *rejected* rather than
//! decoded as noise and played. The timestamp lets the receiver measure
//! jitter and drop packets that arrive too late to be useful.

use crate::audio::jitter::JitterBuffer;

/// Tags an audio datagram. Four bytes so a foreign packet is rejected by a
/// single integer comparison.
pub const MAGIC: [u8; 4] = *b"KVMA";

/// Bumped only for an incompatible payload change; a peer that does not
/// recognize the version drops the datagram instead of misreading it.
pub const VERSION: u8 = 1;

/// `magic(4) + version(1) + seq(4) + timestamp(4)`.
pub const HEADER_LEN: usize = 13;

/// Largest payload accepted from the wire. One 100 ms packet at the
/// protocol's maximum format (192 kHz, 8 channels, s16le) is 307 200
/// bytes, so this bounds a malicious or corrupt datagram with room to
/// spare while staying far below the IP datagram ceiling.
pub const MAX_PAYLOAD: usize = 512 * 1024;

/// One decoded audio datagram, borrowing the receive buffer.
#[derive(Debug, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    pub seq: u32,
    pub timestamp_ms: u32,
    pub payload: &'a [u8],
}

/// Write one audio datagram into `out`, which is cleared first so a caller
/// can reuse a single buffer for the lifetime of a stream (this runs at the
/// packet cadence — 100 times a second at the default format — so
/// allocating per packet would be pure waste).
pub fn encode(out: &mut Vec<u8>, seq: u32, timestamp_ms: u32, payload: &[u8]) {
    out.clear();
    out.reserve(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&timestamp_ms.to_be_bytes());
    out.extend_from_slice(payload);
}

/// Parse an audio datagram. `None` for anything that is not one of ours:
/// wrong magic, wrong version, truncated header, or an over-long payload.
///
/// Rejecting rather than guessing matters here — the socket is reachable by
/// anything on the network, and "that was not audio" must be a decision
/// this function makes, not something the speaker discovers.
pub fn decode(datagram: &[u8]) -> Option<AudioPacket<'_>> {
    if datagram.len() < HEADER_LEN {
        return None;
    }
    if datagram[..4] != MAGIC {
        return None;
    }
    if datagram[4] != VERSION {
        return None;
    }
    let seq = u32::from_be_bytes(datagram[5..9].try_into().ok()?);
    let timestamp_ms = u32::from_be_bytes(datagram[9..13].try_into().ok()?);
    let payload = &datagram[HEADER_LEN..];
    if payload.len() > MAX_PAYLOAD {
        return None;
    }
    Some(AudioPacket { seq, timestamp_ms, payload })
}

/// Turns an arbitrary stream of captured bytes into fixed-size packets.
///
/// A capture device hands back whatever it happened to have — 5 ms here,
/// 40 ms there, occasionally a ragged tail. But the wire wants one packet
/// per `frame_ms`, because the packet *is* the playout unit on the far
/// side: a jitter buffer ordering variable-sized chunks would have to
/// re-derive timing from byte counts, and one oversized chunk would stall
/// playout for exactly as long as it is long.
///
/// So capture is reframed here, in one place, identically on every
/// platform: accumulate bytes, emit whole packets, carry the remainder.
#[derive(Debug)]
pub struct Packetiser {
    /// Bytes in one packet. Never zero (the format is validated before it
    /// reaches here), so the `>=` check below cannot loop forever.
    bytes_per_packet: usize,
    /// Bytes carried over from the previous push, not yet a packet.
    pending: Vec<u8>,
}

impl Packetiser {
    pub fn new(bytes_per_packet: usize) -> Self {
        Self { bytes_per_packet: bytes_per_packet.max(1), pending: Vec::new() }
    }

    /// Add captured bytes and collect every packet they completed.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.pending.extend_from_slice(data);
        let mut out = Vec::new();
        while self.pending.len() >= self.bytes_per_packet {
            let rest = self.pending.split_off(self.bytes_per_packet);
            out.push(std::mem::replace(&mut self.pending, rest));
        }
        out
    }

    /// Bytes currently held back — visible so a stalled sender (capture too
    /// slow to fill a packet) is diagnosable rather than mysterious.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Drop any partial packet. Used when a stream restarts: a half packet
    /// from the old stream must not be glued onto the new one.
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// The sending side of one audio stream: reframes capture into packets,
/// numbers them, and hands ready-to-send datagrams to the transport.
///
/// Separated from the transport so the same logic drives Linux, Windows, or
/// a test: it never touches a socket or a device.
#[derive(Debug)]
pub struct Sender {
    packetiser: Packetiser,
    /// Next sequence number. Starts at 1 so the receiver's initial 0 counts
    /// as "not yet seen" rather than a duplicate.
    seq: u32,
    /// Elapsed stream time, in milliseconds — the capture clock the
    /// receiver uses for jitter and lateness decisions. Derived from the
    /// packet count rather than a wall clock so a stalled sender produces a
    /// consistent, monotonic timeline.
    timestamp_ms: u32,
    frame_ms: u32,
}

impl Sender {
    /// `bytes_per_packet` comes from the negotiated [`AudioFormat`], so the
    /// packet cadence and `frame_ms` always agree.
    ///
    /// [`AudioFormat`]: kvmshare_protocol::message::AudioFormat
    pub fn new(bytes_per_packet: usize, frame_ms: u16) -> Self {
        Self {
            packetiser: Packetiser::new(bytes_per_packet),
            seq: 1,
            timestamp_ms: 0,
            frame_ms: frame_ms as u32,
        }
    }

    /// Feed captured bytes; `emit` is called once per completed packet with
    /// its sequence number, timestamp, and payload.
    ///
    /// A callback rather than a returned `Vec` so the caller can encode
    /// straight into a reused buffer and send immediately — no per-packet
    /// allocation on a path that runs 100 times a second.
    pub fn push(&mut self, captured: &[u8], mut emit: impl FnMut(u32, u32, &[u8])) {
        for payload in self.packetiser.push(captured) {
            emit(self.seq, self.timestamp_ms, &payload);
            self.seq = self.seq.wrapping_add(1);
            // The timestamp advances only for audio that was actually sent,
            // so it stays a faithful map of stream time even if capture
            // stalls.
            self.timestamp_ms = self.timestamp_ms.wrapping_add(self.frame_ms);
        }
    }

    /// Restart numbering for a new stream (the receiver resets its jitter
    /// buffer in step, so old sequence numbers cannot be mistaken for new
    /// audio).
    pub fn reset(&mut self) {
        self.packetiser.reset();
        self.seq = 1;
        self.timestamp_ms = 0;
    }

    /// Sequence number the next completed packet will carry — reported so
    /// the GUI can show whether a stream is actually advancing.
    pub fn next_seq(&self) -> u32 {
        self.seq
    }
}

/// The receiving side of one audio stream: the jitter buffer plus the
/// bookkeeping needed to hand frames to a player in order.
#[derive(Debug)]
pub struct Receiver {
    jitter: JitterBuffer,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    pub fn new() -> Self {
        Self { jitter: JitterBuffer::new() }
    }

    /// Offer a decoded packet.
    pub fn accept(&mut self, packet: &AudioPacket<'_>) {
        // The timestamp is not needed for ordering (the sequence is the
        // authority) — it is measured by the caller for jitter stats. The
        // payload is copied because the receive buffer is reused for the
        // next datagram.
        self.jitter.push(packet.seq, packet.payload.to_vec());
    }

    /// The next frame to play, or `None` for a gap (silence).
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.jitter.pop()
    }

    /// A new stream is starting: forget the old timeline.
    pub fn reset(&mut self) {
        self.jitter.reset();
    }

    pub fn is_started(&self) -> bool {
        self.jitter.is_started()
    }

    pub fn depth(&self) -> usize {
        self.jitter.depth()
    }

    pub fn stats(&self) -> super::jitter::AudioStats {
        self.jitter.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_round_trips() {
        let mut buf = Vec::new();
        encode(&mut buf, 7, 1234, &[1, 2, 3, 4]);
        let packet = decode(&buf).expect("valid datagram");
        assert_eq!(packet.seq, 7);
        assert_eq!(packet.timestamp_ms, 1234);
        assert_eq!(packet.payload, &[1, 2, 3, 4]);
    }

    /// The header is exactly the documented size, so the payload offset can
    /// never drift from the documentation.
    #[test]
    fn the_header_is_the_promised_size() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 2, &[0u8; 100]);
        assert_eq!(buf.len(), HEADER_LEN + 100);
    }

    /// Encoding into a reused buffer must not leave the previous packet's
    /// bytes behind — a stale tail would play as noise.
    #[test]
    fn reusing_the_buffer_does_not_leak_the_previous_packet() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 0, &[9u8; 500]);
        encode(&mut buf, 2, 10, &[1u8; 16]);
        assert_eq!(buf.len(), HEADER_LEN + 16);
        let packet = decode(&buf).unwrap();
        assert_eq!(packet.payload, &[1u8; 16]);
    }

    /// Anything that is not one of our datagrams is refused, so a foreign
    /// packet on the socket is never played as noise.
    #[test]
    fn foreign_datagrams_are_rejected() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0u8; HEADER_LEN - 1]), None);
        // Right shape, wrong magic.
        let mut wrong_magic = Vec::new();
        encode(&mut wrong_magic, 1, 0, &[0u8; 8]);
        wrong_magic[0] = b'X';
        assert_eq!(decode(&wrong_magic), None);
        // Right magic, wrong version.
        let mut wrong_version = Vec::new();
        encode(&mut wrong_version, 1, 0, &[0u8; 8]);
        wrong_version[4] = VERSION.wrapping_add(1);
        assert_eq!(decode(&wrong_version), None);
    }

    /// An over-long payload is refused rather than allocated for.
    #[test]
    fn an_oversized_payload_is_rejected() {
        let mut buf = Vec::new();
        encode(&mut buf, 1, 0, &[0u8; 16]);
        // Forge a payload length beyond the cap by padding the datagram.
        buf.resize(HEADER_LEN + MAX_PAYLOAD + 1, 0);
        assert_eq!(decode(&buf), None);
    }

    /// An exact multiple of the packet size emits whole packets and keeps
    /// nothing back.
    #[test]
    fn packetiser_emits_whole_packets() {
        let mut p = Packetiser::new(4);
        let packets = p.push(&[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(packets, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
        assert_eq!(p.pending(), 0);
    }

    /// A ragged capture read is carried, not padded: padding would add
    /// bytes that are not audio.
    #[test]
    fn ragged_input_carries_the_remainder_forward() {
        let mut p = Packetiser::new(4);
        assert!(p.push(&[1, 2, 3]).is_empty());
        assert_eq!(p.pending(), 3);
        let packets = p.push(&[4, 5]);
        assert_eq!(packets, vec![vec![1, 2, 3, 4]]);
        assert_eq!(p.pending(), 1);
    }

    /// A capture read larger than one packet yields as many packets as it
    /// completed — a device that bursts after a stall must not be forced
    /// into oversized packets.
    #[test]
    fn a_large_read_is_split_into_packets() {
        let mut p = Packetiser::new(3);
        let packets = p.push(&[0u8; 10]);
        assert_eq!(packets.len(), 3);
        assert_eq!(packets[0].len(), 3);
        assert_eq!(p.pending(), 1);
    }

    /// Resetting drops a partial packet, so a restarted stream cannot
    /// splice the end of the old stream onto the start of the new one.
    #[test]
    fn reset_drops_the_partial_packet() {
        let mut p = Packetiser::new(4);
        p.push(&[1, 2, 3]);
        p.reset();
        assert_eq!(p.pending(), 0);
        assert_eq!(p.push(&[9, 9, 9, 9]), vec![vec![9, 9, 9, 9]]);
    }

    /// A zero packet size cannot hang the loop; it is clamped to one.
    #[test]
    fn a_zero_packet_size_does_not_spin() {
        let mut p = Packetiser::new(0);
        assert_eq!(p.push(&[1, 2]), vec![vec![1], vec![2]]);
    }

    /// The sender numbers packets from 1 and advances stream time by one
    /// frame per packet, so the receiver's timeline matches the audio.
    #[test]
    fn the_sender_numbers_and_timestamps_each_packet() {
        let mut sender = Sender::new(4, 10);
        let mut seen: Vec<(u32, u32, Vec<u8>)> = Vec::new();
        sender.push(&[0u8; 12], |seq, ts, payload| {
            seen.push((seq, ts, payload.to_vec()));
        });
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].0, 1);
        assert_eq!(seen[1].0, 2);
        assert_eq!(seen[2].0, 3);
        // 10 ms per packet.
        assert_eq!(seen[0].1, 0);
        assert_eq!(seen[1].1, 10);
        assert_eq!(seen[2].1, 20);
        assert_eq!(sender.next_seq(), 4);
    }

    /// A partial packet is not timestamped — the timeline counts audio that
    /// was actually transmitted.
    #[test]
    fn a_partial_packet_does_not_advance_the_timeline() {
        let mut sender = Sender::new(4, 10);
        let mut count = 0;
        sender.push(&[0u8; 3], |_, _, _| count += 1);
        assert_eq!(count, 0);
        assert_eq!(sender.next_seq(), 1, "nothing was sent");
    }

    /// Reset returns the sender to the start of a new stream.
    #[test]
    fn reset_restarts_a_sender() {
        let mut sender = Sender::new(4, 10);
        sender.push(&[0u8; 4], |_, _, _| {});
        assert_eq!(sender.next_seq(), 2);
        sender.reset();
        assert_eq!(sender.next_seq(), 1);
    }

    /// The receiver reassembles a stream in order, including a packet that
    /// arrives out of order.
    ///
    /// Enough packets are fed to satisfy the pre-roll: playback
    /// deliberately does not begin until the buffer can absorb jitter, so a
    /// shorter burst playing nothing is correct, not a bug.
    #[test]
    fn the_receiver_plays_packets_in_order() {
        let mut rx = Receiver::new();
        let mut buf = Vec::new();
        // Deliberately out of order.
        for (seq, byte) in [(1u32, 1u8), (3, 3), (2, 2), (4, 4)] {
            encode(&mut buf, seq, seq * 10, &[byte; 8]);
            let packet = decode(&buf).unwrap();
            rx.accept(&packet);
        }
        assert!(rx.is_started(), "pre-roll satisfied once enough audio is buffered");
        for expected in 1u8..=4 {
            assert_eq!(rx.pop(), Some(vec![expected; 8]), "packet {expected}");
        }
        assert_eq!(rx.stats().played, 4);
    }

    /// A foreign datagram must never reach the jitter buffer: decoding is
    /// the gate, and this proves the two layers agree.
    #[test]
    fn a_foreign_datagram_never_reaches_the_receiver() {
        let mut rx = Receiver::new();
        let junk = [0u8; 64];
        assert_eq!(decode(&junk), None);
        // Nothing was accepted, so nothing plays and nothing is buffered.
        assert_eq!(rx.pop(), None);
        assert_eq!(rx.depth(), 0);
        assert_eq!(rx.stats().accepted, 0);
    }
}
