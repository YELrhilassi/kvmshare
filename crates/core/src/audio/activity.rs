//! Deciding whether a machine is currently making sound.
//!
//! This answers the one question the media router's `last_active_source`
//! policy needs: *which machine is playing something right now?*
//!
//! It works on raw captured samples rather than any desktop media API
//! (MPRIS, GSMTC, ...). That is deliberate: every OS can answer "is there
//! sound on this machine" without knowing anything about the applications
//! producing it, so the policy behaves identically everywhere, keeps
//! working for audio no media API knows about, and adds no dependency on
//! a desktop environment being present at all.

/// Decides whether a machine is currently making sound.
///
/// Hysteresis is the whole point: a naive "is the sample above the floor"
/// check flaps on every quiet passage of a song, and each flap is a change
/// in what the media keys control. Two thresholds (a higher one to start,
/// a lower one to stop) plus a run-length requirement make the answer
/// stable across normal listening dynamics.
#[derive(Debug)]
pub struct ActivityDetector {
    /// dBFS at or above which audio counts as playing.
    start_db: f32,
    /// dBFS below which audio counts as silence. Lower than `start_db`, so
    /// a track fading down does not flicker the state.
    stop_db: f32,
    /// Consecutive windows required before a state change is believed.
    windows_to_flip: u32,
    /// Windows in a row with the level above `start_db`.
    loud_run: u32,
    /// Windows in a row with the level below `stop_db`.
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
            // 6 dB of hysteresis: a fade or a quiet bridge stays "playing"
            // rather than toggling the router's target.
            stop_db: floor_db - 6.0,
            // ~100 ms at the default 10 ms frames: fast enough to feel
            // immediate, slow enough to ignore a single click.
            windows_to_flip: 10,
            loud_run: 0,
            quiet_run: 0,
            playing: false,
        }
    }

    /// Root-mean-square level of `s16le` samples, in dBFS.
    ///
    /// Pure arithmetic over the bytes — no allocation, no format assumptions
    /// beyond the codec the protocol negotiates. An empty window is silence.
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
        self.feed_level(level)
    }

    /// Feed one window whose level has already been measured.
    ///
    /// Split out from [`feed`](Self::feed) for the callers that need the
    /// level as well as the answer — the capture loop publishes a live
    /// meter (see [`crate::audio::runtime`]) and must not measure the same
    /// window twice.
    pub fn feed_level(&mut self, level: f32) -> bool {
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

    /// The highest level this detector treats as silence, in dBFS.
    ///
    /// The meter below the floor is display noise: the GUI shows "silent"
    /// rather than a number when the reading is at or under this.
    pub fn floor_db(&self) -> f32 {
        self.start_db
    }

    /// The current answer without feeding new audio.
    pub fn is_playing(&self) -> bool {
        self.playing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full-scale square wave, so the level is deterministic.
    fn loud_packet(samples: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(samples * 2);
        for i in 0..samples {
            let v: i16 = if i % 2 == 0 { 32000 } else { -32000 };
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }

    fn silent_packet(samples: usize) -> Vec<u8> {
        vec![0u8; samples * 2]
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
        for _ in 0..20 {
            assert!(!det.feed(&silent_packet(480)));
        }
        assert!(!det.is_playing());

        for _ in 0..20 {
            det.feed(&loud_packet(480));
        }
        assert!(det.is_playing(), "sustained audio reads as playing");
    }

    /// One stray click must not flip the state — that would move the media
    /// keys to another machine for a click.
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

    /// Measuring a window once and feeding the level gives the same answer
    /// as feeding the window, so the metering path cannot disagree with the
    /// activity the media router is told about.
    #[test]
    fn feeding_a_measured_level_matches_feeding_the_window() {
        let mut by_window = ActivityDetector::new(-50.0);
        let mut by_level = ActivityDetector::new(-50.0);
        for _ in 0..20 {
            by_window.feed(&loud_packet(480));
            by_level.feed_level(ActivityDetector::rms_dbfs(&loud_packet(480)));
        }
        assert!(by_window.is_playing() && by_level.is_playing());
        assert_eq!(by_window.floor_db(), -50.0);
        for _ in 0..20 {
            by_window.feed(&silent_packet(480));
            by_level.feed_level(ActivityDetector::rms_dbfs(&silent_packet(480)));
        }
        assert!(!by_window.is_playing() && !by_level.is_playing());
    }

    /// A quieter floor makes a quieter signal count as playing, which is
    /// what a user with a noisy output needs.
    #[test]
    fn the_floor_is_configurable() {
        // About -60 dBFS.
        let mut quiet = Vec::new();
        for i in 0..480 {
            let v: i16 = if i % 2 == 0 { 33 } else { -33 };
            quiet.extend_from_slice(&v.to_le_bytes());
        }
        let level = ActivityDetector::rms_dbfs(&quiet);
        assert!(level < -50.0 && level > -70.0, "level was {level}");

        let mut strict = ActivityDetector::new(-50.0);
        let mut sensitive = ActivityDetector::new(-65.0);
        for _ in 0..20 {
            strict.feed(&quiet);
            sensitive.feed(&quiet);
        }
        assert!(!strict.is_playing(), "below the default floor");
        assert!(sensitive.is_playing(), "above the configured floor");
    }
}
