//! The audio link's live state file.
//!
//! The role process owns the audio runtime (see
//! [`kvmshare_core::audio::runtime`]); the GUI is a separate process and
//! cannot call into it. So the runtime's [`AudioStatusSink`] is fulfilled
//! here: every real transition is written to `<state_dir>/audio.state`,
//! which the GUI reads to show whether audio is linked, which direction is
//! flowing, and — when something broke — why.
//!
//! It also carries the two **meters** (captured and received level, each
//! with its own "above the silence floor" flag) and the peer's name. That is
//! what turns the page from "a switch is on" into "sound is being captured
//! here, and it is arriving there": a link can be up while nothing is
//! playing, and a stream can be announced while nothing lands.
//!
//! The format is the same deliberately trivial key=value shape as
//! `client.state` and `control.state`, written atomically (tmp + rename) so
//! a crash never leaves a torn file. The file exists only while a role's
//! audio engine is up; a missing file means "no audio", and the GUI
//! reconciles it against the running role the same way it reconciles
//! `client.state`.
//!
//! Nothing here is on a hot path: the sink is called only when the status
//! *changes*, never per packet, so a file write per transition is cheap.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kvmshare_core::audio::runtime::{AudioStatus, AudioStatusSink, METER_FLOOR_DB};

/// Name of the state file, relative to the state dir.
const FILE: &str = "audio.state";

/// A [`AudioStatusSink`] that persists transitions to `<state_dir>/audio.state`.
pub struct AudioStateWriter {
    dir: PathBuf,
}

impl AudioStateWriter {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }
}

impl AudioStatusSink for AudioStateWriter {
    fn status(&self, status: AudioStatus) {
        write_audio_state(&self.dir, &status);
    }
}

/// A sink that writes audio state under `dir`, ready to hand to a runtime.
pub fn audio_status_sink(dir: PathBuf) -> Arc<dyn AudioStatusSink> {
    Arc::new(AudioStateWriter::new(dir))
}

/// Write the audio state atomically. Best-effort: a state file is
/// observability, never a critical path, so a failed write is swallowed —
/// the GUI simply keeps the previous state until the next transition.
pub fn write_audio_state(dir: &Path, status: &AudioStatus) {
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let file = dir.join(FILE);
    // A failure reason is prose: it must never inject a newline (or a
    // stray `=` is fine, but a newline would forge a key=value line).
    let error = status
        .error
        .as_deref()
        .map(|e| e.replace(['\n', '\r'], " "))
        .unwrap_or_default();
    // A peer name is prose too (a client picks its own name), and a newline
    // in it would forge a key=value line exactly as an error's would. The
    // capture note is prose by definition — it is a sentence telling the
    // user which setting to change.
    let one_line = |text: &str| text.replace(['\n', '\r'], " ");
    let peer = status.peer.as_deref().map(one_line).unwrap_or_default();
    let note = status.capture_note.as_deref().map(one_line).unwrap_or_default();
    // A level is written with one decimal — enough for a meter, and short
    // enough that the file stays a file a person can read. Digital silence
    // arrives as `-inf`, which no parser wants, so it is clamped to the
    // meter's floor; an absent reading is an empty value.
    let level = |db: Option<f32>| match db {
        Some(db) if db.is_finite() => format!("{db:.1}"),
        Some(_) => format!("{METER_FLOOR_DB:.1}"),
        None => String::new(),
    };
    let body = format!(
        "send={}\nreceive={}\nsending={}\nreceiving={}\nerror={error}\n\
         peer={peer}\ncapture_playing={}\ncapture_level={}\n\
         receive_playing={}\nreceive_level={}\ncapture_note={note}\n",
        u8::from(status.send),
        u8::from(status.receive),
        u8::from(status.sending),
        u8::from(status.receiving),
        u8::from(status.capture_playing),
        level(status.capture_level_db),
        u8::from(status.receive_playing),
        level(status.receive_level_db),
    );
    let tmp = file.with_extension("state.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &file);
    }
}

/// Remove a stale state file. Called when a role starts, so a file left by
/// the previous run can never be read as the current one.
pub fn clear_audio_state(dir: &Path) {
    let _ = fs::remove_file(dir.join(FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("kvmshare-audio-state-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn writes_every_field() {
        let dir = scratch_dir("fields");
        let status = AudioStatus {
            send: true,
            receive: false,
            sending: true,
            receiving: false,
            error: None,
            peer: Some("hp".into()),
            capture_playing: true,
            capture_level_db: Some(-23.44),
            receive_playing: false,
            receive_level_db: None,
            capture_note: None,
        };
        write_audio_state(&dir, &status);
        let body = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(body.contains("send=1"));
        assert!(body.contains("receive=0"));
        assert!(body.contains("sending=1"));
        assert!(body.contains("receiving=0"));
        assert!(body.contains("error=\n"), "an empty error is a present, empty line");
        assert!(body.contains("peer=hp\n"));
        assert!(body.contains("capture_playing=1\n"));
        assert!(body.contains("capture_level=-23.4\n"), "one decimal is a meter's worth");
        assert!(body.contains("receive_playing=0\n"));
        assert!(body.contains("receive_level=\n"), "no reading is an empty value");
        assert!(body.contains("capture_note=\n"), "no warning is an empty value");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The capture warning is a sentence telling the user which setting to
    /// change; like the failure reason, it must stay on one line or it would
    /// forge the next key=value line.
    #[test]
    fn a_capture_note_stays_on_one_line() {
        let dir = scratch_dir("note");
        let status = AudioStatus {
            send: true,
            capture_note: Some("sink is muted\nunmute it".into()),
            ..AudioStatus::default()
        };
        write_audio_state(&dir, &status);
        let body = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(body.contains("capture_note=sink is muted unmute it\n"), "body was:\n{body}");
        assert_eq!(body.lines().count(), 11, "eleven keys, one line each");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Digital silence has no level; the file carries the meter's floor
    /// rather than an `-inf` no parser and no UI wants to see.
    #[test]
    fn digital_silence_is_written_as_the_floor() {
        let dir = scratch_dir("silence");
        let status = AudioStatus {
            capture_level_db: Some(f32::NEG_INFINITY),
            ..AudioStatus::default()
        };
        write_audio_state(&dir, &status);
        let body = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(body.contains("capture_level=-100.0\n"), "body was:\n{body}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A reason is prose and must stay on one line: a newline would forge a
    /// second key=value line in the file the GUI parses.
    #[test]
    fn error_reasons_stay_on_one_line() {
        let dir = scratch_dir("error");
        let status = AudioStatus {
            send: true,
            sending: false,
            error: Some("capture stopped: device gone\nsecond line".into()),
            ..AudioStatus::default()
        };
        write_audio_state(&dir, &status);
        let body = fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(body.contains("error=capture stopped: device gone second line"));
        assert_eq!(body.lines().count(), 11, "eleven keys, one line each");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_the_file() {
        let dir = scratch_dir("clear");
        write_audio_state(&dir, &AudioStatus::default());
        assert!(dir.join(FILE).exists());
        clear_audio_state(&dir);
        assert!(!dir.join(FILE).exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
