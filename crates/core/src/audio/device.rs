//! The two traits that keep audio OS-independent.
//!
//! Everything above these — packetising, jitter, activity detection,
//! negotiation, the runtime — is platform-independent and lives in
//! `kvmshare-core`. Everything below them is `<audio server>` on Linux and
//! WASAPI on Windows, in `kvmshare-platform`. That is the same split the
//! crate already uses for input capture (`Engine`) and injection
//! (`Injector`), which is why adding a platform means implementing traits
//! rather than touching the pipeline.
//!
//! # The contract
//!
//! * **Fail loudly.** A rejected capture permission or a missing audio
//!   server is returned as an error naming the reason. Returning silence
//!   instead would present as "audio connects but nothing is heard", which
//!   is the hardest possible thing for a user to diagnose.
//! * **Never capture a microphone.** Capture is loopback only — a monitor
//!   source on Linux, an output endpoint on Windows. It is not a
//!   convention each backend is trusted to follow; it is enforced by what
//!   the backend is asked to capture, and by the device lists each backend
//!   offers.
//! * **Block on the audio thread, never the control thread.** `read` and
//!   `write` may block; the runtime calls them from a dedicated thread.
//!   A blocking device call must never be able to stall the cursor.

use kvmshare_protocol::message::AudioFormat;

/// A capture device: this machine's audio output, as a stream of PCM.
pub trait AudioCapture: Send {
    /// The format the device is actually capturing in, or an error
    /// explaining why it cannot.
    fn format(&self) -> Result<AudioFormat, String>;

    /// Read up to `buf.len()` bytes of captured audio, returning how many
    /// were written. `Ok(0)` means "nothing available yet", not EOF —
    /// capture is a continuous stream. `Err` means the stream is over (the
    /// device or audio server went away) and the caller should surface it
    /// rather than retrying in a loop.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String>;
}

/// A playback device: plays received PCM into the local output.
pub trait AudioPlayback: Send {
    /// Start playback at `format`. Replaces any stream already running.
    fn start(&mut self, format: AudioFormat) -> Result<(), String>;

    /// Play one packet. Implementations may block until there is room; they
    /// must not buffer without bound.
    fn write(&mut self, samples: &[u8]) -> Result<(), String>;

    /// Stop playback and release the device.
    fn stop(&mut self);
}

/// Lists this machine's audio devices, for the GUI's pickers.
///
/// Names are opaque strings the platform understands; `default` is always
/// valid everywhere. Capture names are **outputs** (their loopback is what
/// is captured), never microphones — offering a microphone here would
/// invite streaming the room by accident.
pub trait AudioDevices: Send {
    /// Outputs whose audio can be captured.
    fn capture_devices(&self) -> Vec<String>;
    /// Outputs audio can be played to.
    fn playback_devices(&self) -> Vec<String>;
}
