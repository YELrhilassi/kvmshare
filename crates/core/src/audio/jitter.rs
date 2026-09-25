//! Reordering and pacing received audio packets.
//!
//! The network delivers audio late, out of order, twice, or not at all.
//! Playback wants it *in order, on time, exactly once*. This module is the
//! only thing that bridges those two views, so the rest of the audio path
//! can treat a `pop()` result as "the next 10 ms of sound, or silence".

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

/// Reorders and paces received audio packets.
///
/// Packets carry a 32-bit sequence. The buffer holds them in unwrapped
/// 64-bit order, hands them out strictly in sequence, and absorbs a bounded
/// amount of reordering.
///
/// Two deliberate behaviours, both of which look like "losing data" and are
/// not:
///
/// * **Late packets are dropped.** In an interactive stream, audio that
///   arrives after its play time is noise, not information.
/// * **Overflow resynchronises forward.** If the buffer fills, the oldest
///   packets are discarded so latency returns to the target instead of
///   growing without bound. A stream that stays behind by a growing margin
///   is worse than one that skips a syllable.
///
/// It is deliberately indifferent to where the audio came from: it knows
/// only sequence numbers and payloads, so it is reused unchanged by every
/// platform and is exhaustively testable without a sound card.
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

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    /// A buffer sized for the default format: enough pre-roll to absorb a
    /// few tens of milliseconds of jitter, and a capacity that bounds added
    /// latency to well under a quarter second.
    pub fn new() -> Self {
        Self::with_limits(4, 24)
    }

    /// `pre_roll` and `capacity` are in packets, not milliseconds: the
    /// packet size is the format's `frame_ms`, so a 10 ms frame makes 4
    /// packets ≈ 40 ms of pre-roll and 24 ≈ 240 ms of maximum buffering. A
    /// pre-roll larger than the capacity would never start playback, so it
    /// is clamped.
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
    /// This is what makes a long-lived stream survive the counter wrapping:
    /// a peer that has sent 2³² packets keeps working, and a packet
    /// arriving slightly out of order is placed *next to* the ones around
    /// it rather than 4 billion slots away.
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

    /// Take the next packet in sequence, or `None` when it is not there yet
    /// (still pre-rolling, or an underrun).
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
                // A gap that outlives the buffer means the stream is gone or
                // badly broken; resynchronise to what we do have so playback
                // recovers instead of counting underruns forever.
                if !self.pending.is_empty() {
                    self.next = Some(*self.pending.keys().next().expect("non-empty"));
                }
                None
            }
        }
    }

    /// Discard everything and go back to pre-rolling — used when a stream
    /// restarts, where old sequence numbers describe a different stream
    /// entirely.
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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// A packet older than what already played is dropped, not replayed: in
    /// an interactive stream, stale audio is noise.
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

    /// Overflow resynchronises forward instead of growing latency without
    /// bound.
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
}
