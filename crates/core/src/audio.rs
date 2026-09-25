//! Audio streaming: the platform-independent half.
//!
//! # The shape of the problem
//!
//! Audio flows one way per stream: a sender captures *its own output*,
//! packets it, and the peer plays it. Direction is independent, so both
//! machines can send at once — and because playback goes to the ordinary
//! output device, each OS's own mixer combines the remote stream with
//! local sound. There is no custom mixer, and nothing here needs to know
//! what the audio *is*.
//!
//! This module owns everything that can be decided without touching an
//! audio device:
//!
//! * [`JitterBuffer`] — reorder packets, absorb network jitter, drop what
//!   arrives too late to be useful.
//! * [`ActivityDetector`] — decide whether a machine is actually playing
//!   something, which is what drives the media router's
//!   `last_active_source` policy. It deliberately works on raw samples
//!   rather than any desktop media API, so it behaves identically on
//!   every OS and keeps working for audio no media API knows about.
//! * [`AudioCapture`] / [`AudioPlayback`] — the two traits the platform
//!   crate implements (see `docs/11-media-and-audio.md`).
//!
//! The actual device work — PipeWire/PulseAudio on Linux, WASAPI on
//! Windows — lives in `kvmshare-platform`, exactly like input capture and
//! cursor injection.

use kvmshare_protocol::message::AudioFormat;

/// Where a pushed packet ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Buffered for playback.
    Accepted,
    /// Already played, or older than something already played — the
    /// stream moved past it, so replaying it would be worse than losing
    /// it.
    Late,
    /// The buffer was full: the network delivered faster than playback
    /// drains (or playback stalled). The buffer is drained forward to
    /// catch up, because staying behind forever costs latency and never
    /// recovers.
    Overflow,
}

/// Reorders and paces received audio packets.
///
/// Packets carry a 32-bit sequence and a sender timestamp. The buffer
/// holds them in unwrapped 64-bit order, hands them out strictly in
/// sequence, and absorbs a bounded amount of reordering.
///
/// Two deliberate behaviours, both of which look like "losing data" and
/// are not:
///
/// * **Late packets are dropped.** In an interactive stream, audio that
///   arrives after its play time is noise, not information.
/// * **Overflow resynchronises forward.** If the buffer fills, the oldest
///   packets are discarded so latency returns to the target instead of
///   growing without bound. A stream that stays behind by a growing
///   margin is worse than one that skips a syllable.
#[derive(Debug)]
pub struct JitterBuffer {
    /// Packets to accumulate before playback starts, absorbing the first
    /// burst of jitter. Without pre-roll, the first packet plays
    /// immediately and every following packet risks an underrun.
    pre_roll: usize,
    /// Maximum packets held. Bounds both memory and added latency.
    capacity: usize,
    /// Next sequence to play, in unwrapped space. `None` until the first
    /// packet arrives (the starting point is wherever the sender is).
    next: Option<u64>,
    /// Buffered packets, keyed by unwrapped sequence.
    pending: std::collections::BTreeMap<u64, Vec<u8>>,
    /// True once pre-roll is satisfied and playback has begun.
    started: bool,
    stats: AudioStats,
}

/// Counters for observability — the GUI's "audio is actually flowing"
/// evidence, and the first thing to look at when something sounds wrong.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AudioStats {
    /// Packets accepted into the buffer.
    pub accepted: u64,
    /// Packets dropped for arriving after their play time or twice.
    pub late: u64,
    /// Packets dropped because the buffer filled.
    pub overflow: u64,
    /// Times playback was asked for a packet and the buffer was empty.
    pub underruns: u64,
    /// Packets actually handed to the player.
    pub played: u64,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    /// A buffer sized for the default format: enough pre-roll to absorb a
    /// few tens of milliseconds of jitter, and a capacity that bounds
    /// added latency to well under a quarter second.
    pub fn new() -> Self {
        Self::with_limits(4, 24)
    }

    /// `pre_roll` and `capacity` are in packets, not milliseconds: the
    /// packet size is the format's `frame_ms`, so a 10 ms frame makes 4
    /// packets ≈ 40 ms of pre-roll and 24 ≈ 240 ms of maximum buffering.
    /// A pre-roll larger than the capacity would never start playback, so
    /// it is clamped.
    pub fn with_limits(pre_roll: usize, capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            pre_roll: pre_roll.min(capacity),
            capacity,
            next: None,
            pending: std::collections::BTreeMap::new(),
            started: false,
            stats: AudioStats::default(),
        }
    }

    /// Extend a 32-bit wire sequence into 64-bit space, picking the value
    /// nearest `reference`.
    ///
    /// This is what makes a long-lived stream survive the counter
    /// wrapping: a peer that has sent 2³² packets keeps working, and a
    /// packet arriving slightly out of order is placed *next to* the ones
    /// around it rather than 4 billion slots away.
    fn unwrap(seq: u32, reference: u64) -> u64 {
        let candidate = (reference & !0xffff_ffff) | seq as u64;
        let mut best = candidate;
        let mut best_distance = (candidate as i128 - reference as i128).abs();
        for alternative in [
            candidate.wrapping_sub(1u64 << 32),
            candidate.wrapping_add(1u64 << 32),
        ] {
            let distance = (alternative as i128 - reference as i128).abs();
            if distance < best_distance {
                best = alternative;
                best_distance = distance;
            }
        }
        best
    }

    /// Offer a received packet. `payload` is only stored when accepted.
    pub fn push(&mut self, seq: u32, payload: Vec<u8>) -> PushOutcome {
        let reference = self
            .next
            .or_else(|| self.pending.keys().next().copied())
            .unwrap_or(seq as u64);
        let seq = Self::unwrap(seq, reference);

        // Already played, or older than what is queued to play next: this
        // packet's time has passed.
        if let Some(next) = self.next {
            if seq < next {
                self.stats.late += 1;
                return PushOutcome::Late;
            }
        }
        if self.pending.contains_key(&seq) {
            self.stats.late += 1;
            return PushOutcome::Late;
        }

        self.pending.insert(seq, payload);
        self.stats.accepted += 1;

        if self.pending.len() > self.capacity {
            // Drop from the front — the oldest audio — so latency returns
            // to target instead of growing.
            while self.pending.len() > self.capacity {
                let oldest = *self.pending.keys().next().expect("non-empty");
                self.pending.remove(&oldest);
                self.stats.overflow += 1;
                // The stream has moved past whatever was lost: continue
                // from here rather than waiting for the dropped sequence.
                self.next = Some(oldest + 1);
            }
            self.started = true;
            return PushOutcome::Overflow;
        }

        if !self.started && self.pending.len() >= self.pre_roll {
            self.started = true;
        }
        PushOutcome::Accepted
    }

    /// Take the next packet in sequence, or `None` when it is not there
    /// yet (still pre-rolling, or an underrun).
    ///
    /// Returning `None` rather than the newest packet is deliberate: the
    /// player should emit silence for the gap. Skipping ahead to whatever
    /// arrived last would turn a loss into a stutter *plus* a reorder.
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        if !self.started {
            return None;
        }
        let next = self.next.or_else(|| self.pending.keys().next().copied())?;
        match self.pending.remove(&next) {
            Some(payload) => {
                self.next = Some(next + 1);
                self.stats.played += 1;
                Some(payload)
            }
            None => {
                self.stats.underruns += 1;
                // A gap that outlives the buffer means the stream is gone
                // or badly broken; resynchronise to what we do have so
                // playback recovers instead of counting underruns forever.
                if !self.pending.is_empty() {
                    self.next = Some(*self.pending.keys().next().expect("non-empty"));
                }
                None
            }
        }
    }

    /// Discard everything and go back to pre-rolling — used when a stream
    /// restarts (a new `AudioStart`), where old sequence numbers describe
    /// a different stream entirely.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.next = None;
        self.started = false;
    }

    /// How many packets are waiting to play.
    pub fn depth(&self) -> usize {
        self.pending.len()
    }

    /// Whether playback has begun (pre-roll satisfied).
    pub fn is_started(&self) -> bool {
        self.started
    }

    pub fn stats(&self) -> AudioStats {
        self.stats
    }
}

/// Decides whether a machine is currently making sound.
///
/// Feeds on captured PCM and answers one question — *is something
/// playing?* — which is what the media router needs to know. Works on
/// raw samples, so it is OS-neutral and does not care which application
/// is producing the audio.
///
/// Hysteresis is the whole point: a naive "is the sample above the floor"
/// check flaps on every quiet passage of a song, and each flap is a
/// change in what the media keys control. Two thresholds (a higher one to
/// start, a lower one to stop) plus a run-length requirement make the
/// answer stable across normal listening dynamics.
#[derive(Debug)]
pub struct ActivityDetector {
    /// dBFS at or above which audio counts as playing.
    start_db: f32,
    /// dBFS below which audio counts as silence. Lower than `start_db`,
    /// so a track fading down does not flicker the state.
    stop_db: f32,
    /// Consecutive windows required before a state change is believed.
    windows_to_flip: u32,
    /// Windows in a row with the level above `start_db` / below `stop_db`.
    loud_run: u32,
    quiet_run: u32,
    playing: bool,
}

impl Default for ActivityDetector {
    fn default() -> Self {
        Self::new(-50.0)
    }
}

impl ActivityDetector {
    /// The floor is the level below which a window counts as silence.
    /// −50 dBFS sits under normal playback and above the noise floor of a
    /// quiet output, so "paused" reads as silent and "playing quietly"
    /// still reads as playing.
    pub fn new(floor_db: f32) -> Self {
        Self {
            start_db: floor_db,
            // 6 dB of hysteresis: a fade or a quiet bridge stays
            // "playing" rather than toggling the router's target.
            stop_db: floor_db - 6.0,
            // ~100 ms at the default 10 ms frames: fast enough to feel
            // immediate, slow enough to ignore a single click or a
            // zero-crossing gap.
            windows_to_flip: 10,
            loud_run: 0,
            quiet_run: 0,
            playing: false,
        }
    }

    /// Root-mean-square level of `s16le` samples, in dBFS.
    ///
    /// Pure arithmetic over the bytes — no allocation, no format
    /// assumptions beyond the codec the protocol negotiates. An empty
    /// window is silence.
    pub fn rms_dbfs(samples: &[u8]) -> f32 {
        if samples.len() < 2 {
            return f32::NEG_INFINITY;
        }
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for chunk in samples.chunks_exact(2) {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f64;
            sum += sample * sample;
            count += 1;
        }
        if count == 0 {
            return f32::NEG_INFINITY;
        }
        let rms = (sum / count as f64).sqrt();
        if rms <= 0.0 {
            return f32::NEG_INFINITY;
        }
        // Full scale for 16-bit signed is 32768.
        (20.0 * (rms / 32768.0).log10()) as f32
    }

    /// Feed one window of captured audio; returns the current answer.
    pub fn feed(&mut self, samples: &[u8]) -> bool {
        let level = Self::rms_dbfs(samples);
        if level >= self.start_db {
            self.loud_run += 1;
            self.quiet_run = 0;
            if self.loud_run >= self.windows_to_flip {
                self.playing = true;
            }
        } else if level < self.stop_db {
            self.quiet_run += 1;
            self.loud_run = 0;
            if self.quiet_run >= self.windows_to_flip {
                self.playing = false;
            }
        } else {
            // In the hysteresis band: hold the current answer.
            self.loud_run = 0;
            self.quiet_run = 0;
        }
        self.playing
    }

    /// The current answer without feeding new audio.
    pub fn is_playing(&self) -> bool {
        self.playing
    }
}

/// Turns an arbitrary stream of captured bytes into fixed-size packets.
///
/// A capture device hands back whatever it happened to have — 5 ms here,
/// 40 ms there, occasionally a ragged tail. But the wire wants one packet
/// per `frame_ms`, because the packet *is* the playout unit on the far
/// side: a jitter buffer ordering variable-sized chunks would have to
/// re-derive timing from byte counts, and one oversized chunk would
/// stall playout for exactly as long as it is long.
///
/// So capture is reframed here, in one place, identically on every
/// platform: accumulate bytes, emit whole packets, carry the remainder.
#[derive(Debug)]
pub struct Packetiser {
    /// Bytes in one packet. Never zero (the format is validated before it
    /// reaches here), so the `>=` checks below cannot loop forever.
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

    /// Bytes currently held back — visible so a stalled sender (capture
    /// too slow to fill a packet) is diagnosable rather than mysterious.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Drop any partial packet. Used when a stream restarts: a half
    /// packet from the old stream must not be glued onto the new one.
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// A capture device: this machine's audio output, as a stream of PCM.
///
/// The platform crate implements this. Implementations must be
/// **non-blocking on start** and must report failure honestly — a
/// rejected capture permission is a fact the user needs to see, not
/// something to hide behind silence.
pub trait AudioCapture: Send {
    /// The format the device is actually capturing in, or an error
    /// explaining why it cannot.
    fn format(&self) -> Result<AudioFormat, String>;

    /// Read up to `buf.len()` bytes of captured audio, returning how many
    /// were written. `Ok(0)` means "nothing available yet", not EOF —
    /// capture is a continuous stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

/// A playback device: plays received PCM into the local output.
pub trait AudioPlayback: Send {
    /// Start playback at `format`.
    fn start(&mut self, format: AudioFormat) -> Result<(), String>;

    /// Play one packet. Implementations may block until there is room;
    /// they must not buffer without bound.
    fn write(&mut self, samples: &[u8]) -> Result<(), String>;

    /// Stop playback and release the device.
    fn stop(&mut self);
}

/// Lists this machine's capture and playback devices, for the GUI's
/// device pickers. Names are opaque strings the platform understands
/// (and `default` is always valid on every platform).
pub trait AudioDevices: Send {
    /// Output devices whose audio can be captured (loopback/monitor).
    fn capture_devices(&self) -> Vec<String>;
    /// Output devices audio can be played to.
    fn playback_devices(&self) -> Vec<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a packet whose payload is silence of the given sample count.
    fn silent_packet(samples: usize) -> Vec<u8> {
        vec![0u8; samples * 2]
    }

    /// Full-scale square wave, so the level is deterministic.
    fn loud_packet(samples: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(samples * 2);
        for i in 0..samples {
            let v: i16 = if i % 2 == 0 { 32000 } else { -32000 };
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    #[test]
    fn in_order_packets_play_in_order_after_pre_roll() {
        let mut jb = JitterBuffer::with_limits(3, 16);
        assert_eq!(jb.push(1, vec![1]), PushOutcome::Accepted);
        assert_eq!(jb.push(2, vec![2]), PushOutcome::Accepted);
        // Pre-roll not satisfied: nothing plays yet.
        assert!(!jb.is_started());
        assert_eq!(jb.pop(), None);
        assert_eq!(jb.push(3, vec![3]), PushOutcome::Accepted);
        assert!(jb.is_started());
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    /// Reordering inside the buffer is the case the buffer exists for.
    #[test]
    fn reordered_packets_are_played_in_sequence() {
        let mut jb = JitterBuffer::with_limits(2, 16);
        jb.push(1, vec![1]);
        jb.push(3, vec![3]);
        jb.push(2, vec![2]);
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]), "the late arrival still plays in order");
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    /// A packet older than what already played is dropped, not replayed:
    /// in an interactive stream, stale audio is noise.
    #[test]
    fn packets_older_than_playback_are_dropped() {
        let mut jb = JitterBuffer::with_limits(1, 16);
        jb.push(5, vec![5]);
        assert_eq!(jb.pop(), Some(vec![5]));
        assert_eq!(jb.push(5, vec![5]), PushOutcome::Late, "replay is not a duplicate");
        assert_eq!(jb.push(4, vec![4]), PushOutcome::Late);
        assert_eq!(jb.stats().late, 2);
    }

    /// Feeding the same packet twice must not play it twice.
    #[test]
    fn duplicates_are_dropped() {
        let mut jb = JitterBuffer::with_limits(2, 16);
        jb.push(1, vec![1]);
        assert_eq!(jb.push(1, vec![1]), PushOutcome::Late);
        jb.push(2, vec![2]);
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
    }

    /// Overflow resynchronises forward instead of growing latency
    /// without bound.
    #[test]
    fn overflow_drops_the_oldest_and_catches_up() {
        let mut jb = JitterBuffer::with_limits(2, 4);
        for seq in 1..=4 {
            jb.push(seq, vec![seq as u8]);
        }
        assert_eq!(jb.depth(), 4);
        // The fifth packet forces a drop of the oldest.
        assert_eq!(jb.push(5, vec![5]), PushOutcome::Overflow);
        assert!(jb.depth() <= 4, "capacity is respected: {}", jb.depth());
        assert_eq!(jb.stats().overflow, 1);
        // Playback continues from where the stream actually is — the
        // dropped packet is skipped, not waited for.
        assert!(jb.pop().is_some(), "playback resumes after a drop");
    }

    #[test]
    fn an_empty_buffer_reports_underrun_and_stays_empty() {
        let mut jb = JitterBuffer::with_limits(1, 8);
        jb.push(1, vec![1]);
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), None);
        assert_eq!(jb.stats().underruns, 1);
    }

    #[test]
    fn reset_returns_to_pre_roll_for_a_new_stream() {
        let mut jb = JitterBuffer::with_limits(2, 8);
        jb.push(1, vec![1]);
        jb.push(2, vec![2]);
        assert!(jb.is_started());
        jb.reset();
        assert!(!jb.is_started());
        assert_eq!(jb.depth(), 0);
        // A restarted stream's sequence numbers describe a new stream and
        // must be accepted, not treated as stale.
        assert_eq!(jb.push(1, vec![9]), PushOutcome::Accepted);
    }

    /// The sequence counter wraps after 2³² packets; a long-lived stream
    /// must keep playing straight through it.
    #[test]
    fn sequence_wrapping_does_not_break_playback() {
        let mut jb = JitterBuffer::with_limits(2, 8);
        let start = u32::MAX - 2;
        for i in 0..6u32 {
            jb.push(start.wrapping_add(i), vec![i as u8]);
        }
        // Play all six in order, across the wrap.
        for i in 0..6u32 {
            assert_eq!(jb.pop(), Some(vec![i as u8]), "packet {i}");
        }
    }

    /// A packet that arrives out of order *across the wrap* is placed by
    /// the nearest-unwrap rule, not 4 billion slots away.
    #[test]
    fn reordering_across_the_wrap_still_orders_correctly() {
        let mut jb = JitterBuffer::with_limits(2, 8);
        jb.push(u32::MAX, vec![1]);
        jb.push(1, vec![3]); // wrapped past zero
        jb.push(0, vec![2]); // late, but still in the buffer window
        assert_eq!(jb.pop(), Some(vec![1]));
        assert_eq!(jb.pop(), Some(vec![2]));
        assert_eq!(jb.pop(), Some(vec![3]));
    }

    /// Silence measures as silence, and full scale measures near 0 dBFS.
    #[test]
    fn rms_measures_known_levels() {
        assert_eq!(ActivityDetector::rms_dbfs(&[]), f32::NEG_INFINITY);
        assert_eq!(ActivityDetector::rms_dbfs(&[0, 0]), f32::NEG_INFINITY);
        assert_eq!(ActivityDetector::rms_dbfs(&silent_packet(480)), f32::NEG_INFINITY);

        let full = ActivityDetector::rms_dbfs(&loud_packet(480));
        assert!(full > -1.0 && full <= 0.0, "full scale is ~0 dBFS, got {full}");
    }

    /// A quiet-but-real signal reads as playing; a paused output reads as
    /// silent. This is the entire input to `last_active_source`.
    #[test]
    fn activity_distinguishes_playing_from_silence() {
        let mut det = ActivityDetector::new(-50.0);
        // 480 samples of silence ~ 10 ms at 48 kHz.
        for _ in 0..20 {
            assert!(!det.feed(&silent_packet(480)));
        }
        assert!(!det.is_playing());

        for _ in 0..20 {
            det.feed(&loud_packet(480));
        }
        assert!(det.is_playing(), "sustained audio reads as playing");
    }

    /// One stray click must not flip the state — that would move the
    /// media keys to another machine for a click.
    #[test]
    fn a_single_click_does_not_flip_activity() {
        let mut det = ActivityDetector::new(-50.0);
        det.feed(&loud_packet(480));
        assert!(!det.is_playing(), "one window is not evidence");
    }

    /// Once playing, a brief quiet passage (a fade, a bridge) must not
    /// immediately hand control elsewhere.
    #[test]
    fn brief_gaps_do_not_flip_a_playing_stream() {
        let mut det = ActivityDetector::new(-50.0);
        for _ in 0..20 {
            det.feed(&loud_packet(480));
        }
        assert!(det.is_playing());
        // Two silent windows: not enough to be believed.
        det.feed(&silent_packet(480));
        det.feed(&silent_packet(480));
        assert!(det.is_playing(), "a short gap holds the state");
        // Sustained silence does flip it.
        for _ in 0..20 {
            det.feed(&silent_packet(480));
        }
        assert!(!det.is_playing());
    }

    /// A fresh detector is not playing — it has no evidence, and guessing
    /// "playing" would steal media keys from the local machine.
    #[test]
    fn a_fresh_detector_is_not_playing() {
        assert!(!ActivityDetector::new(-50.0).is_playing());
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
        // The next read completes the packet, in order.
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
}
