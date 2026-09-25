//! OS audio backends: capture this machine's output, and play the peer's.
//!
//! Three implementations sit behind two traits from
//! [`kvmshare_core::audio`]:
//!
//! * **Linux** — the PulseAudio client tools (`parec`/`pacat`/`pactl`).
//!   PipeWire's `pipewire-pulse` provides all three, so one backend
//!   covers both audio servers, and it is the same "shell out to the
//!   session's own tool" approach the X11 wheel daemon already uses.
//! * **Windows** — WASAPI loopback capture and render, through
//!   `windows-sys` directly.
//! * **Anything else** — a backend that reports honestly that it cannot
//!   do audio, rather than pretending and producing silence.
//!
//! The shared, testable logic (device naming and the parsing of the
//! Linux tools' output) lives here, so it can be tested on any host.

/// The **monitor** (loopback) source that carries a sink's output.
///
/// PulseAudio and PipeWire name the loopback of a sink by appending
/// `.monitor`, and that is what makes "stream what this machine is
/// playing" possible without any mixing: we capture the sink's own
/// monitor, which is by definition everything being played to it.
pub fn monitor_of(sink: &str) -> String {
    format!("{sink}.monitor")
}

/// Is this source name a loopback monitor rather than a real input?
pub fn is_monitor(source: &str) -> bool {
    source.ends_with(".monitor")
}

/// Parse the device names out of `pactl list short sinks` /
/// `pactl list short sources` output.
///
/// Those commands print one record per line as
/// `index<TAB>name<TAB>driver<TAB>spec...`, so the name is the second
/// tab-separated field. Anything shorter (a header, a blank line, an
/// error message) is skipped rather than guessing.
pub fn parse_pactl_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{PulseCapture, PulseDevices, PulsePlayback};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{WasapiCapture, WasapiDevices, WasapiPlayback};

// Platforms with no backend yet fail loudly rather than pretending (see
// the module docs): macOS needs a CoreAudio pair implementing the same
// two traits, and nothing above this layer is platform-specific.
#[cfg(not(any(target_os = "linux", windows)))]
mod unsupported;
#[cfg(not(any(target_os = "linux", windows)))]
pub use unsupported::{UnsupportedCapture, UnsupportedDevices, UnsupportedPlayback};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_sources_are_named_by_appending_to_the_sink() {
        assert_eq!(
            monitor_of("alsa_output.pci-0000_00_1f.3.analog-stereo"),
            "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"
        );
    }

    /// A real input must never be mistaken for a loopback: capturing a
    /// microphone and sending it to another machine's speakers would be
    /// both surprising and a privacy problem.
    #[test]
    fn microphones_are_not_mistaken_for_monitors() {
        assert!(is_monitor("alsa_output.pci-0000_00_1f.3.analog-stereo.monitor"));
        assert!(!is_monitor("alsa_input.pci-0000_00_1f.3.analog-stereo"));
    }

    /// The real shape of `pactl list short sources` output on a machine
    /// with a monitor and a microphone (tab-separated).
    #[test]
    fn pactl_output_parses_into_device_names() {
        let output = "0\talsa_output.pci-0000_00_1f.3.analog-stereo.monitor\tmodule-alsa-card.c\ts16le 2ch 44100Hz\tSUSPENDED\n\
                      1\talsa_input.pci-0000_00_1f.3.analog-stereo\tmodule-alsa-card.c\ts16le 2ch 44100Hz\tSUSPENDED\n";
        let names = parse_pactl_names(output);
        assert_eq!(names, vec![
            "alsa_output.pci-0000_00_1f.3.analog-stereo.monitor".to_string(),
            "alsa_input.pci-0000_00_1f.3.analog-stereo".to_string(),
        ]);
        // Exactly one of them is a monitor — the one audio is captured
        // from.
        let monitors: Vec<&String> = names.iter().filter(|n| is_monitor(n)).collect();
        assert_eq!(monitors.len(), 1);
    }

    /// Malformed or empty output yields no devices instead of a bogus
    /// one — a wrong device name would make capture fail confusingly.
    #[test]
    fn pactl_output_that_is_not_a_device_list_yields_nothing() {
        assert!(parse_pactl_names("").is_empty());
        assert!(parse_pactl_names("Failure: No such entity\n").is_empty());
        assert!(parse_pactl_names("0\t\n").is_empty());
    }
}
