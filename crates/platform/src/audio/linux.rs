//! Linux audio, through the session's own PulseAudio client tools.
//!
//! # Why the client tools and not a linked library
//!
//! `parec`/`pacat`/`pactl` are present on every machine running
//! PulseAudio **or** PipeWire (`pipewire-pulse` ships the same tools),
//! which covers the overwhelming majority of Linux desktops with no build
//! dependency, no FFI, and no version coupling. It is also the pattern
//! this crate already uses for the X11 wheel daemon.
//!
//! The trade-off, stated plainly: it depends on those tools existing. The
//! failure is honest and immediate (`parec` is not found → the GUI says
//! so) rather than a silent stream of nothing, and the alternative —
//! linking `libpulse` — would need cross-compiled headers for the
//! Windows builds and would still not work on a PipeWire-only system
//! without the compat layer.
//!
//! # Capture is always from a *monitor*
//!
//! Capture never touches a microphone. It opens the default sink's
//! `.monitor` source, which is exactly the audio being played to that
//! sink — so "send this machine's audio to the other machine" cannot
//! accidentally stream the room.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};

use kvmshare_core::audio::{AudioCapture, AudioPlayback, AudioDevices};
use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::AudioFormat;

use super::{is_monitor, monitor_of, parse_pactl_names};

/// Latency requested from the audio server, in milliseconds. This is the
/// *server-side* buffer, which dominates end-to-end delay far more than
/// the network does: 40 ms is comfortably below noticing for music, and
/// low enough that volume changes feel immediate. It is also the reason
/// this backend competes with the input stream for CPU gracefully.
const LATENCY_MS: u32 = 40;

/// The PulseAudio/PipeWire tools this backend drives.
const PAREC: &str = "parec";
const PACAT: &str = "pacat";
const PACTL: &str = "pactl";

/// Run a PulseAudio tool and return its stdout, or an error naming the
/// tool and its complaint. Never panics: a missing tool is a normal,
/// reportable condition.
fn run(tool: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(tool)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{tool}: {e}"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{tool} failed: {}", err.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The name of the current default sink (the output audio is played to
/// and captured from). Falls back to the first sink when the running
/// `pactl` predates `get-default-sink`, so an older PulseAudio still
/// works.
fn default_sink() -> Result<String, String> {
    if let Ok(name) = run(PACTL, &["get-default-sink"]) {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return Ok(name);
        }
    }
    let sinks = parse_pactl_names(&run(PACTL, &["list", "short", "sinks"])?);
    sinks
        .into_iter()
        .next()
        .ok_or_else(|| "no audio output devices found".to_string())
}

/// Argument list for the capture process.
///
/// Split out from spawning so the exact flags are testable without an
/// audio server — the flags are the contract with `parec`, and a typo
/// there would show up as an empty stream at runtime.
fn capture_args(device: &str, fmt: AudioFormat) -> Vec<String> {
    vec![
        // `--raw` is what makes this raw PCM on stdout instead of a
        // stream-wrapped format we would have to demux.
        format!("--device={device}"),
        "--raw".to_string(),
        "--format=s16le".to_string(),
        format!("--rate={}", fmt.sample_rate),
        format!("--channels={}", fmt.channels),
        format!("--latency-msec={LATENCY_MS}"),
        // Shows up in `pavucontrol` / `pactl list clients`, so a user
        // can see *what* is capturing their audio and kill it.
        "--client-name=kvmshare".into(),
    ]
}

/// Argument list for the playback process.
fn playback_args(device: &str, fmt: AudioFormat) -> Vec<String> {
    vec![
        "--playback".to_string(),
        format!("--device={device}"),
        "--raw".to_string(),
        "--format=s16le".to_string(),
        format!("--rate={}", fmt.sample_rate),
        format!("--channels={}", fmt.channels),
        format!("--latency-msec={LATENCY_MS}"),
        "--client-name=kvmshare".into(),
    ]
}

/// Lists the devices the GUI offers.
pub struct PulseDevices;

impl PulseDevices {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PulseDevices {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevices for PulseDevices {
    fn capture_devices(&self) -> Vec<String> {
        // Only monitors: those are the "output as an input" devices that
        // carry what this machine is playing. Real inputs belong to the
        // microphone, and must not be streamed anywhere by accident.
        match run(PACTL, &["list", "short", "sources"]) {
            Ok(out) => parse_pactl_names(&out)
                .into_iter()
                .filter(|name| is_monitor(name))
                .collect(),
            Err(e) => {
                log_warn!("audio: cannot list capture devices: {e}");
                Vec::new()
            }
        }
    }

    fn playback_devices(&self) -> Vec<String> {
        match run(PACTL, &["list", "short", "sinks"]) {
            Ok(out) => parse_pactl_names(&out),
            Err(e) => {
                log_warn!("audio: cannot list playback devices: {e}");
                Vec::new()
            }
        }
    }
}

/// A running capture process, read as a PCM stream.
pub struct PulseCapture {
    child: Child,
    format: AudioFormat,
}

impl PulseCapture {
    /// Start capturing. `device` empty or `default` means "the default
    /// sink's monitor" — the common case, and the one that follows the
    /// user's audio when they switch outputs.
    pub fn new(device: &str, format: AudioFormat) -> Result<Self, String> {
        let device = if device.is_empty() || device == "default" {
            monitor_of(&default_sink()?)
        } else {
            // A configured device that is a plain sink name is still
            // meant as its monitor: capture is always loopback, so the
            // user does not have to know the `.monitor` convention.
            if is_monitor(device) {
                device.to_string()
            } else {
                monitor_of(device)
            }
        };
        let args = capture_args(&device, format);
        log_debug!("audio capture: {PAREC} {}", args.join(" "));
        let child = Command::new(PAREC)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            // The tool's own complaints go to our log: a rejected device
            // name is the most likely failure and this is where it is
            // explained.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{PAREC}: {e} (is pulseaudio-utils installed?)"))?;
        Ok(Self { child, format })
    }
}

impl AudioCapture for PulseCapture {
    fn format(&self) -> Result<AudioFormat, String> {
        Ok(self.format)
    }

    /// Blocking read, per the trait: the caller owns a dedicated audio
    /// thread, and blocking here is exactly what keeps the thread from
    /// spinning. Returns `Err` when the capture process has died, which
    /// is the signal to restart it (and which the caller surfaces rather
    /// than looping forever on a dead device).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let stdout = self
            .child
            .stdout
            .as_mut()
            .ok_or_else(|| "capture process has no stdout".to_string())?;
        match stdout.read(buf) {
            Ok(0) => {
                // EOF: the process ended. Report why, so a bad device or
                // a killed audio server is not a silent dead stream.
                match self.child.try_wait() {
                    Ok(Some(status)) => Err(format!("{PAREC} exited ({status})")),
                    Ok(None) => Err(format!("{PAREC} closed its output")),
                    Err(e) => Err(format!("{PAREC} status: {e}")),
                }
            }
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(0),
            Err(e) => Err(format!("{PAREC} read: {e}")),
        }
    }
}

impl Drop for PulseCapture {
    fn drop(&mut self) {
        // The capture process is a child of ours; if we do not reap it,
        // it keeps capturing (and keeps a "kvmshare is recording" entry
        // in the audio mixer) after the stream ended.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A running playback process, fed the peer's PCM.
pub struct PulsePlayback {
    /// `None` until [`AudioPlayback::start`]. Playback is started by the
    /// audio runtime once a format is negotiated, so an instance exists
    /// before there is a process to own.
    child: Option<Child>,
    format: Option<AudioFormat>,
}

impl PulsePlayback {
    pub fn new() -> Self {
        Self { child: None, format: None }
    }
}

impl Default for PulsePlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayback for PulsePlayback {
    fn start(&mut self, format: AudioFormat) -> Result<(), String> {
        // Restarting replaces any running process (a format change is a
        // new stream, and the old process is playing the old one).
        self.stop();
        let args = playback_args("default", format);
        log_debug!("audio playback: {PACAT} {}", args.join(" "));
        let child = Command::new(PACAT)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{PACAT}: {e} (is pulseaudio-utils installed?)"))?;
        self.child = Some(child);
        self.format = Some(format);
        Ok(())
    }

    /// Write one packet. `pacat`'s pipe applies backpressure when the
    /// audio server is behind, which is the correct behaviour: it paces
    /// us to real time instead of building an unbounded buffer of audio
    /// that would arrive later and later.
    fn write(&mut self, samples: &[u8]) -> Result<(), String> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| "playback not started (call start() first)".to_string())?;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "playback process has no stdin".to_string())?;
        stdin.write_all(samples).map_err(|e| format!("{PACAT} write: {e}"))
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        // Closing stdin is a clean end-of-stream for `pacat`, which lets
        // it drain what it has instead of truncating the tail.
        drop(child.stdin.take());
        let _ = child.wait();
    }
}

impl Drop for PulsePlayback {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> AudioFormat {
        AudioFormat::default()
    }

    /// The capture flags are the contract with `parec`: raw PCM, the
    /// negotiated rate/channels, and a bounded latency.
    #[test]
    fn capture_arguments_request_raw_pcm_at_the_negotiated_format() {
        let args = capture_args("sink.monitor", fmt());
        assert!(args.contains(&"--raw".to_string()));
        assert!(args.contains(&"--device=sink.monitor".to_string()));
        assert!(args.contains(&"--format=s16le".to_string()));
        assert!(args.contains(&"--rate=48000".to_string()));
        assert!(args.contains(&"--channels=2".to_string()));
        assert!(args.contains(&"--latency-msec=40".to_string()));
    }

    /// Playback must be in playback mode; without `--playback`, `pacat`
    /// would try to *record* to a device.
    #[test]
    fn playback_arguments_are_in_playback_mode() {
        let args = playback_args("default", fmt());
        assert!(args.contains(&"--playback".to_string()));
        assert!(args.contains(&"--raw".to_string()));
        assert!(args.contains(&"--rate=48000".to_string()));
    }

    /// A non-default format is passed through verbatim — this is what
    /// makes negotiation meaningful rather than decorative.
    #[test]
    fn a_negotiated_format_is_honoured() {
        let custom = AudioFormat { sample_rate: 44_100, channels: 1, frame_ms: 20, codec: 0 };
        let args = capture_args("sink.monitor", custom);
        assert!(args.contains(&"--rate=44100".to_string()));
        assert!(args.contains(&"--channels=1".to_string()));
    }

    /// Both client processes identify themselves, so the user can see in
    /// their mixer what is capturing and what is playing.
    #[test]
    fn both_processes_are_identifiable_in_the_mixer() {
        assert!(capture_args("s.monitor", fmt()).contains(&"--client-name=kvmshare".to_string()));
        assert!(playback_args("default", fmt()).contains(&"--client-name=kvmshare".to_string()));
    }
}
