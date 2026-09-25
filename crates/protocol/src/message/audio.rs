//! Audio stream description: the format both peers agree on before any
//! samples flow.
//!
//! # Why the format is negotiated rather than fixed
//!
//! The payload is raw `s16le` PCM today. That is a deliberate choice for
//! a LAN KVM: 48 kHz stereo costs ~1.5 Mbit/s, which is nothing on a
//! wired or modern wireless LAN, and it costs **zero CPU** to encode and
//! decode — which matters far more than bandwidth on a machine that is
//! simultaneously forwarding input at sub-millisecond latency. Opus
//! would cut the bitrate by an order of magnitude and add a C dependency
//! that has to cross-compile to Windows.
//!
//! But the requirement is that this works for *everyone*, on whatever
//! machine and network they have. So the format travels on the wire
//! instead of being compiled in: both peers state what they accept, the
//! best common option wins, and adding a compressed codec later is a new
//! [`id::codecs`] constant plus an encoder — **not** a protocol break,
//! and never a silent change of what a running pair is sending.
//!
//! # Validation
//!
//! Every field is bounded at decode time. These numbers size buffers and
//! pace the jitter buffer, so a hostile or buggy peer offering a
//! 4 GHz / 65535-channel / 0 ms format must be rejected at the wire
//! boundary rather than causing an allocation or a divide-by-zero.

use crate::id;
use crate::wire::{ReadBuf, WireError, WriteBuf};

/// The format used when a peer does not care: 48 kHz stereo, 10 ms
/// frames, uncompressed.
pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_CHANNELS: u8 = 2;
pub const DEFAULT_FRAME_MS: u16 = 10;

/// Bounds a wire-supplied format must fit. The ceilings are generous
/// (192 kHz covers every consumer device that exists) and exist only to
/// keep a malformed offer from sizing a buffer absurdly.
pub const MIN_SAMPLE_RATE: u32 = 8_000;
pub const MAX_SAMPLE_RATE: u32 = 192_000;
pub const MAX_CHANNELS: u8 = 8;
/// Frame cadence bounds: 1 ms is the practical floor for packet pacing,
/// 100 ms the point where interactivity is gone.
pub const MIN_FRAME_MS: u16 = 1;
pub const MAX_FRAME_MS: u16 = 100;

/// One audio stream's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
    /// Packet cadence: how many milliseconds of audio one datagram
    /// carries. Also the jitter buffer's granularity.
    pub frame_ms: u16,
    /// Payload encoding — see [`id::codecs`].
    pub codec: u8,
}

impl Default for AudioFormat {
    fn default() -> Self {
        Self {
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            frame_ms: DEFAULT_FRAME_MS,
            codec: id::codecs::PCM_S16LE,
        }
    }
}

impl AudioFormat {
    /// Bytes one sample occupies in every codec supported today (16-bit
    /// signed little-endian). Kept next to the codec constants so a new
    /// codec has an obvious home for its own arithmetic.
    const BYTES_PER_SAMPLE: usize = 2;

    /// Bytes of payload in one packet of this format.
    ///
    /// Used to size the packetise/reassemble buffers, so it is computed
    /// from validated fields only — a decoded `AudioFormat` can never
    /// overflow this.
    pub fn bytes_per_packet(&self) -> usize {
        self.sample_rate as usize
            * self.channels as usize
            * self.frame_ms as usize
            * Self::BYTES_PER_SAMPLE
            / 1000
    }

    /// Is this a format both sides can actually implement?
    ///
    /// Checked when a format arrives (not only when one is built), so an
    /// unsupported offer is refused explicitly rather than producing
    /// silence that looks like a broken microphone.
    pub fn is_supported(&self) -> bool {
        (MIN_SAMPLE_RATE..=MAX_SAMPLE_RATE).contains(&self.sample_rate)
            && self.channels >= 1
            && self.channels <= MAX_CHANNELS
            && (MIN_FRAME_MS..=MAX_FRAME_MS).contains(&self.frame_ms)
            && self.codec == id::codecs::PCM_S16LE
    }

    /// Pick the best format both peers accept, or `None` when they have
    /// no codec in common.
    ///
    /// Preference order for `ours` is the local machine's: it is the one
    /// doing the capture, so its native rate should win when the peer can
    /// take it. The peer's list is ordered by *its* preference, which
    /// makes this a simple two-level search rather than a negotiation
    /// round trip.
    pub fn negotiate(ours: &[AudioFormat], theirs: &[AudioFormat]) -> Option<AudioFormat> {
        for mine in ours {
            if !mine.is_supported() {
                continue;
            }
            for yours in theirs {
                if yours.is_supported()
                    && yours.sample_rate == mine.sample_rate
                    && yours.codec == mine.codec
                    && yours.channels == mine.channels
                {
                    // Frame size is the sender's choice (it paces the
                    // packets); the receiver can buffer any cadence, so
                    // any valid value is acceptable.
                    return Some(AudioFormat { frame_ms: mine.frame_ms, ..*mine });
                }
            }
        }
        None
    }

    pub(crate) fn encode(&self, w: &mut WriteBuf) {
        w.put_u32(self.sample_rate);
        w.put_u8(self.channels);
        w.put_u16(self.frame_ms);
        w.put_u8(self.codec);
    }

    pub(crate) fn decode(r: &mut ReadBuf<'_>) -> Result<Self, WireError> {
        let sample_rate = r.get_u32()?;
        let channels = r.get_u8()?;
        let frame_ms = r.get_u16()?;
        let codec = r.get_u8()?;
        let fmt = Self { sample_rate, channels, frame_ms, codec };
        if !fmt.is_supported() {
            return Err(WireError { what: "unsupported audio format" });
        }
        Ok(fmt)
    }
}

/// Encode a list of formats (a peer's accepted set).
pub(crate) fn encode_list(w: &mut WriteBuf, formats: &[AudioFormat]) {
    // The count is bounded by the wire's u8 and by how many formats can
    // exist: refuse to truncate, because a silently dropped entry would
    // look like "the peer does not support this" during negotiation.
    let n = formats.len().min(u8::MAX as usize) as u8;
    w.put_u8(n);
    for f in formats.iter().take(n as usize) {
        f.encode(w);
    }
}

/// Decode a list of formats, rejecting anything malformed.
pub(crate) fn decode_list(r: &mut ReadBuf<'_>) -> Result<Vec<AudioFormat>, WireError> {
    let n = r.get_u8()? as usize;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(AudioFormat::decode(r)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_roundtrip(fmt: AudioFormat) -> AudioFormat {
        let mut w = WriteBuf::with_capacity(16);
        fmt.encode(&mut w);
        let bytes = w.finish();
        let mut r = ReadBuf::new(&bytes);
        AudioFormat::decode(&mut r).unwrap()
    }

    #[test]
    fn default_format_is_supported_and_roundtrips() {
        let fmt = AudioFormat::default();
        assert!(fmt.is_supported());
        assert_eq!(wire_roundtrip(fmt), fmt);
    }

    /// 48 kHz stereo, 10 ms: 48000 * 2 * 10 * 2 / 1000 = 1920 bytes.
    #[test]
    fn packet_size_matches_the_pcm_arithmetic() {
        assert_eq!(AudioFormat::default().bytes_per_packet(), 1920);
    }

    /// ~1.5 Mbit/s for the default format — the documented cost.
    #[test]
    fn default_bitrate_is_about_one_and_a_half_megabits() {
        let fmt = AudioFormat::default();
        let bits_per_second =
            fmt.bytes_per_packet() as u64 * 8 * 1000 / fmt.frame_ms as u64;
        assert_eq!(bits_per_second, 1_536_000);
    }

    #[test]
    fn absurd_formats_are_rejected_at_the_wire_boundary() {
        let cases = [
            AudioFormat { sample_rate: 0, ..Default::default() },
            AudioFormat { sample_rate: 4_000_000_000, ..Default::default() },
            AudioFormat { channels: 0, ..Default::default() },
            AudioFormat { channels: 255, ..Default::default() },
            AudioFormat { frame_ms: 0, ..Default::default() },
            AudioFormat { frame_ms: 60_000, ..Default::default() },
            AudioFormat { codec: 42, ..Default::default() },
        ];
        for fmt in cases {
            assert!(!fmt.is_supported(), "{fmt:?} should be unsupported");
            let mut w = WriteBuf::with_capacity(16);
            fmt.encode(&mut w);
            let bytes = w.finish();
            let mut r = ReadBuf::new(&bytes);
            assert!(AudioFormat::decode(&mut r).is_err(), "{fmt:?} should not decode");
        }
    }

    /// Negotiation picks the local preference the peer can also take.
    #[test]
    fn negotiation_prefers_the_local_order() {
        let mine = [
            AudioFormat { sample_rate: 48_000, ..Default::default() },
            AudioFormat { sample_rate: 44_100, ..Default::default() },
        ];
        let theirs = [
            AudioFormat { sample_rate: 44_100, ..Default::default() },
            AudioFormat { sample_rate: 48_000, ..Default::default() },
        ];
        let picked = AudioFormat::negotiate(&mine, &theirs).unwrap();
        assert_eq!(picked.sample_rate, 48_000, "our first preference wins");

        // Only the second choice is shared.
        let theirs = [AudioFormat { sample_rate: 44_100, ..Default::default() }];
        let picked = AudioFormat::negotiate(&mine, &theirs).unwrap();
        assert_eq!(picked.sample_rate, 44_100);
    }

    /// Mono and stereo are different streams, not negotiable into each
    /// other — a channel mismatch is not a preference, it is a bug.
    #[test]
    fn negotiation_refuses_a_channel_mismatch() {
        let mine = [AudioFormat { channels: 2, ..Default::default() }];
        let theirs = [AudioFormat { channels: 1, ..Default::default() }];
        assert_eq!(AudioFormat::negotiate(&mine, &theirs), None);
    }

    /// No common codec (or an empty list) means no stream — refused, not
    /// guessed.
    #[test]
    fn negotiation_returns_none_without_a_common_option() {
        let mine = [AudioFormat::default()];
        assert_eq!(AudioFormat::negotiate(&mine, &[]), None);
        let theirs = [AudioFormat { codec: 7, ..Default::default() }];
        assert_eq!(AudioFormat::negotiate(&mine, &theirs), None);
    }

    /// An unsupported entry in our own list is skipped rather than
    /// chosen.
    #[test]
    fn negotiation_skips_our_own_unsupported_entries() {
        let mine = [
            AudioFormat { codec: 9, ..Default::default() },
            AudioFormat::default(),
        ];
        let theirs = [AudioFormat::default()];
        let picked = AudioFormat::negotiate(&mine, &theirs).unwrap();
        assert_eq!(picked.codec, id::codecs::PCM_S16LE);
    }

    #[test]
    fn format_lists_roundtrip() {
        let list = [
            AudioFormat::default(),
            AudioFormat { sample_rate: 44_100, channels: 1, frame_ms: 20, codec: id::codecs::PCM_S16LE },
        ];
        let mut w = WriteBuf::with_capacity(32);
        encode_list(&mut w, &list);
        let bytes = w.finish();
        let mut r = ReadBuf::new(&bytes);
        let back = decode_list(&mut r).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], list[0]);
        assert_eq!(back[1], list[1]);
    }
}
