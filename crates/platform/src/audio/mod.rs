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

use std::sync::Arc;

use kvmshare_core::audio::device::{AudioCapture, AudioPlayback};
use kvmshare_core::audio::runtime::AudioBackend;
use kvmshare_protocol::message::AudioFormat;

/// How long the virtual output stays in place after a stream stops.
///
/// A dropped session is usually a blip: the control link reconnects within
/// a few seconds and the sound resumes. Releasing the route the instant the
/// stream ends makes the sound jump back to this machine's speakers and
/// then away again — the "it plays here, then it plays there" flap. Holding
/// the route across the gap keeps the machine quiet and hands it back only
/// when sharing has really stopped.
#[cfg(target_os = "linux")]
const ROUTE_RELEASE_GRACE: std::time::Duration = std::time::Duration::from_secs(6);

/// The exclusive-send route plus the bookkeeping that debounces its release.
#[cfg(target_os = "linux")]
#[derive(Default)]
struct RouteState {
    route: Option<linux::Route>,
    /// Bumped on every begin/end. A scheduled release only fires when the
    /// generation it captured is still current, so a stream that resumes
    /// within the grace period cancels the release.
    generation: u64,
}

/// This platform's audio, in the form the core's runtime consumes.
///
/// The runtime is platform-independent and never names an OS type; this is
/// the single place that turns "open capture" into `parec` on Linux or a
/// WASAPI client on Windows. One implementation, so both roles (server and
/// client) get identical behaviour for free.
pub struct PlatformAudio {
    /// The live exclusive-send route, when this machine is currently
    /// streaming its sound elsewhere. Present only on Linux, which is the
    /// only backend that can silence the local output while still
    /// capturing it (see [`linux::route`]); other platforms capture a tap
    /// and leave the speakers alone.
    #[cfg(target_os = "linux")]
    route: Arc<std::sync::Mutex<RouteState>>,
}

impl PlatformAudio {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            route: Arc::new(std::sync::Mutex::new(RouteState::default())),
        }
    }
}

/// Repair an exclusive-output route that a process which died mid-share
/// left behind, putting this machine's real output back.
///
/// Called **once per role process, at startup** — deliberately not from
/// [`PlatformAudio::new`]. A short-lived helper also builds a backend (the
/// `--audio-test-tone` press, a device listing), and if construction
/// repaired routes it would tear down the *running* role's live route, the
/// one it mistook for a leftover. Recovery belongs to the process that owns
/// the output, not to whoever happens to open the backend.
pub fn recover_exclusive_output() {
    #[cfg(target_os = "linux")]
    linux::recover_route();
}

/// Put the real output back when the process ends: a graceful stop must not
/// leave the machine quiet waiting for a timer that no longer exists. A
/// hard kill skips this, which is what the startup recovery covers.
#[cfg(target_os = "linux")]
impl Drop for PlatformAudio {
    fn drop(&mut self) {
        if let Ok(mut state) = self.route.lock() {
            if let Some(route) = state.route.take() {
                route.release();
            }
        }
    }
}

impl Default for PlatformAudio {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBackend for PlatformAudio {
    fn open_capture(
        &self,
        device: &str,
        format: AudioFormat,
    ) -> Result<Box<dyn AudioCapture>, String> {
        crate::audio_capture(device, format)
    }

    fn open_playback(&self, device: &str) -> Box<dyn AudioPlayback> {
        crate::audio_playback(device)
    }

    fn capture_note(&self, device: &str) -> Option<String> {
        crate::audio_capture_note(device)
    }

    /// Route this machine's output through the virtual sink so the speakers
    /// fall silent while the sound is still capturable (Linux). The returned
    /// source is the virtual sink's monitor; on every other platform there
    /// is nothing to do and capture stays on `device`.
    fn begin_exclusive_send(&self, _device: &str) -> Result<Option<String>, String> {
        #[cfg(target_os = "linux")]
        {
            let mut state = self.route.lock().unwrap();
            // A stream is (re)starting: cancel any pending release and reuse
            // a route that is still in place — a reconnect must not rebuild
            // the virtual output and make the sound jump back and forth.
            state.generation = state.generation.wrapping_add(1);
            if state.route.is_none() {
                state.route = Some(linux::takeover()?);
            }
            Ok(Some(linux::capture_source()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(None)
        }
    }

    fn end_exclusive_send(&self) {
        #[cfg(target_os = "linux")]
        {
            let shared = Arc::clone(&self.route);
            let generation = {
                let mut state = shared.lock().unwrap();
                state.generation = state.generation.wrapping_add(1);
                state.generation
            };
            // Release later, not now: a control-link blip must not drag the
            // sound back to this machine's speakers for a few seconds. A
            // stream that starts again bumps the generation and cancels it.
            let _ = std::thread::Builder::new()
                .name("kvmshare-audio-route".into())
                .spawn(move || {
                    std::thread::sleep(ROUTE_RELEASE_GRACE);
                    let mut state = shared.lock().unwrap();
                    if state.generation != generation {
                        return;
                    }
                    if let Some(route) = state.route.take() {
                        route.release();
                    }
                });
        }
    }
}

/// The shared handle the app passes to the runtime.
pub fn backend() -> Arc<dyn AudioBackend> {
    Arc::new(PlatformAudio::new())
}

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
pub use linux::{capture_note, PulseCapture, PulseDevices, PulsePlayback};

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
