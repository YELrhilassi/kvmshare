//! Audio on a platform that has no backend yet.
//!
//! Follows the crate-wide rule for unsupported platforms (see
//! [`crate::unsupported`]): **fail visibly, never silently**. A capture
//! or playback that pretends to work would present as a stream that
//! connects and carries nothing, which is far harder to diagnose than a
//! refusal that names the reason.
//!
//! This is the macOS seam: nothing in the protocol, the router, the
//! config or the GUI is platform-specific, so adding a backend is a
//! CoreAudio capture/render pair implementing the same two traits.

use kvmshare_core::audio::{AudioCapture, AudioPlayback, AudioDevices};
use kvmshare_protocol::message::AudioFormat;

/// The one explanation every operation here returns.
const NO_BACKEND: &str = "audio streaming is not implemented on this platform";

pub struct UnsupportedCapture;

impl AudioCapture for UnsupportedCapture {
    fn format(&self) -> Result<AudioFormat, String> {
        Err(NO_BACKEND.to_string())
    }

    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
        Err(NO_BACKEND.to_string())
    }
}

pub struct UnsupportedPlayback;

impl AudioPlayback for UnsupportedPlayback {
    fn start(&mut self, _format: AudioFormat) -> Result<(), String> {
        Err(NO_BACKEND.to_string())
    }

    fn write(&mut self, _samples: &[u8]) -> Result<(), String> {
        Err(NO_BACKEND.to_string())
    }

    fn stop(&mut self) {}
}

pub struct UnsupportedDevices;

impl AudioDevices for UnsupportedDevices {
    /// No devices rather than invented ones: the GUI then shows an empty
    /// picker next to the reason, instead of offering choices that cannot
    /// work.
    fn capture_devices(&self) -> Vec<String> {
        Vec::new()
    }

    fn playback_devices(&self) -> Vec<String> {
        Vec::new()
    }
}
