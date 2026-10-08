//! `--audio-test-tone`: play a short tone on this machine's output.
//!
//! The Media page's **Test** button answers "is this actually working?" in
//! two halves, and this is the audible one. The page plays a tone through
//! the machine's *configured* playback device, then reads what the running
//! role's capture meter saw while it played (see `gui/audio.go`). A tone
//! that is heard but never captured means the output path works and the
//! loopback does not; a tone that is captured but never heard on the other
//! machine means the link is broken, not the device.
//!
//! It lives here rather than in the GUI for exactly the reason
//! `--audio-devices` does: the audio backend is `kvmshare-platform`, which a
//! Go program cannot link. So the GUI asks the role binary — the one
//! implementation of "play something", used by the picker and the runtime
//! alike.
//!
//! The output is one JSON object on stdout, and the exit status stays 0 for
//! a failure too: "the device refused to open" is a *result* the page must
//! render, not a crash.

use std::f32::consts::PI;

use kvmshare_core::audio::runtime::AudioBackend;
use kvmshare_protocol::message::AudioFormat;

/// The tone's frequency. A pure 440 Hz is unmistakable as a *test* rather
/// than a stuck note, and it survives a laptop speaker that would mangle a
/// sweep.
const TONE_HZ: f32 = 440.0;

/// Well below full scale: clearly audible, and not startling with the
/// volume already up.
const TONE_DBFS: f32 = -12.0;

/// Ramp length at each end, in frames (5 ms at 48 kHz). Without it, starting
/// and stopping a sine mid-cycle is a click — a confusing thing to hear from
/// a feature whose whole job is to be reassuring.
const FADE_FRAMES: usize = 240;

/// Play a tone on `device` for `seconds` and report the outcome as JSON.
///
/// `device` follows the runtime's convention: empty or `default` means the
/// system default output, so the test exercises the same device a link
/// would.
pub fn audio_test_tone_json(device: &str, seconds: f32) -> String {
    // Bounded, because this is called with a value from a command line: a
    // typo'd duration must not hold the output device for an hour.
    let seconds = seconds.clamp(0.1, 10.0);
    let backend = kvmshare_platform::audio::backend();
    let outcome = play_tone(backend.as_ref(), device, seconds);
    result_json(device, seconds, &outcome)
}

/// The JSON contract the GUI parses. Split out so the shape is pinned by a
/// test without a sound card.
fn result_json(device: &str, seconds: f32, outcome: &Result<AudioFormat, String>) -> String {
    match outcome {
        Ok(format) => serde_json::json!({
            "ok": true,
            "device": device,
            "seconds": seconds,
            "sampleRate": format.sample_rate,
            "channels": format.channels,
        })
        .to_string(),
        Err(error) => serde_json::json!({
            "ok": false,
            "device": device,
            "seconds": seconds,
            "error": error,
        })
        .to_string(),
    }
}

fn play_tone(
    backend: &dyn AudioBackend,
    device: &str,
    seconds: f32,
) -> Result<AudioFormat, String> {
    let format = AudioFormat::default();
    let mut playback = backend.open_playback(device);
    playback.start(format)?;
    // One buffer for the whole tone, written in packets: the device paces
    // the writes (the trait forbids buffering without bound), so the tone
    // plays in real time and a capture meter sees it while it happens.
    let tone = tone_bytes(format, seconds);
    let per_packet = format.bytes_per_packet().max(2);
    let written = (|| {
        for packet in tone.chunks(per_packet) {
            playback.write(packet)?;
        }
        Ok(())
    })();
    // Always released, including on a failure part-way through: leaving the
    // device open would silence the machine that just asked for a sound.
    playback.stop();
    written.map(|()| format)
}

/// The tone as raw `s16le`, at `format`'s rate and channel count.
fn tone_bytes(format: AudioFormat, seconds: f32) -> Vec<u8> {
    let rate = format.sample_rate.max(1) as f32;
    let channels = format.channels.max(1) as usize;
    let frames = (rate * seconds).max(1.0) as usize;
    let amplitude = 32_767.0 * 10f32.powf(TONE_DBFS / 20.0);
    let mut out = Vec::with_capacity(frames * channels * 2);
    for frame in 0..frames {
        let value =
            ((2.0 * PI * TONE_HZ * frame as f32) / rate).sin() * amplitude * fade_gain(frame, frames);
        let sample = value.round() as i16;
        for _ in 0..channels {
            out.extend_from_slice(&sample.to_le_bytes());
        }
    }
    out
}

/// Gain at `frame`: a short ramp in and out, `1.0` in between. A tone
/// shorter than two fades is all ramp, which is the quietest honest answer
/// rather than a click.
fn fade_gain(frame: usize, frames: usize) -> f32 {
    let fade = FADE_FRAMES.min(frames / 2);
    if fade == 0 {
        return 1.0;
    }
    if frame < fade {
        return frame as f32 / fade as f32;
    }
    let from_end = frames.saturating_sub(1 + frame);
    if from_end < fade {
        return from_end as f32 / fade as f32;
    }
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
    }

    /// The tone is the negotiated format, the requested length, and a real
    /// signal — the three things that make it audible at all.
    #[test]
    fn the_tone_is_a_real_signal_of_the_negotiated_length() {
        let format = AudioFormat::default();
        let one_second = tone_bytes(format, 1.0);
        let expected = format.sample_rate as usize * format.channels as usize * 2;
        assert_eq!(one_second.len(), expected, "one second, at the negotiated format");

        let values = samples(&one_second);
        let peak = values.iter().map(|v| v.unsigned_abs()).max().unwrap();
        // -12 dBFS is about 8200; allow for the fade and rounding.
        assert!(peak > 7_000, "the tone must be audible, peak was {peak}");
        assert!(peak < 12_000, "and must not be startling, peak was {peak}");
    }

    /// Both ends ramp from silence: a tone that starts at full amplitude
    /// clicks, which sounds like a fault.
    #[test]
    fn the_tone_starts_and_ends_at_silence() {
        let bytes = tone_bytes(AudioFormat::default(), 0.5);
        let values = samples(&bytes);
        assert_eq!(values[0], 0);
        assert_eq!(*values.last().unwrap(), 0);
        // And it is at full amplitude by the end of the fade.
        let full = &values[FADE_FRAMES * 2..FADE_FRAMES * 2 + 200];
        assert!(full.iter().any(|v| v.unsigned_abs() > 7_000));
    }

    /// A tone shorter than two fades is all ramp rather than a click, and a
    /// zero-length request still yields one frame instead of an empty write
    /// loop the device would never hear.
    #[test]
    fn a_very_short_tone_is_all_fade() {
        assert_eq!(tone_bytes(AudioFormat::default(), 0.0).len(), 4);
        assert!(fade_gain(0, 1) <= 1.0);
        assert_eq!(fade_gain(0, 0), 1.0, "no frames, no fade to apply");
    }

    /// The failure half of the contract: a device that will not open is a
    /// result with a reason, not a silent success and not a crash.
    #[test]
    fn a_failure_reports_a_reason() {
        let json = result_json("no-such-device", 1.5, &Err("no such device".into()));
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["error"], "no such device");
        assert_eq!(value["device"], "no-such-device");

        let ok = result_json("default", 1.5, &Ok(AudioFormat::default()));
        let value: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(value["ok"], true);
        assert_eq!(value["sampleRate"], 48_000);
        assert_eq!(value["channels"], 2);
    }
}
