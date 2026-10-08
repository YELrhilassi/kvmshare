//! The `--audio-devices` JSON contract.
//!
//! `kvmshare-server --audio-devices` prints this machine's audio devices and
//! exits; the GUI's device pickers parse it (see `gui/audio.go`). The shape
//! is a cross-language contract, so it is produced by one function here and
//! pinned by a test rather than inlined in the binary — a rename on either
//! side then breaks a test instead of the picker.
//!
//! `capture` lists outputs whose *loopback* can be captured (never a
//! microphone); `playback` lists outputs audio can be played to. Both are
//! always arrays, even when empty, so the GUI never has to handle `null`.

/// Snapshot the platform's audio devices as the JSON the GUI parses.
pub fn audio_devices_json() -> String {
    let devices = kvmshare_platform::audio_devices();
    serde_json::json!({
        "capture": devices.capture_devices(),
        "playback": devices.playback_devices(),
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the Go parser requires: an object with two array keys.
    /// (The contents are machine-dependent — possibly empty — so only the
    /// keys and their types are asserted.)
    #[test]
    fn the_json_has_both_arrays() {
        let value: serde_json::Value = serde_json::from_str(&audio_devices_json()).unwrap();
        assert!(value.get("capture").and_then(|v| v.as_array()).is_some());
        assert!(value.get("playback").and_then(|v| v.as_array()).is_some());
    }
}
