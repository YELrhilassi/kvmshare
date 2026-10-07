//! Windows audio, through WASAPI.
//!
//! # Capture is loopback, never a microphone
//!
//! WASAPI's loopback mode captures an **output** endpoint's stream — the
//! exact analogue of the Linux backend's monitor source. So "send this
//! machine's audio to the other machine" cannot accidentally stream the
//! room, on either platform, by construction rather than by convention.
//!
//! # Format conversion is delegated
//!
//! Devices run at whatever rate they like (44.1 kHz is common, 48 kHz is
//! common, some are stranger). Rather than resampling ourselves,
//! `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` asks WASAPI to accept *our*
//! negotiated format and convert internally, on the audio engine's own
//! optimized path. That is the same trick the Linux backend gets for free
//! by handing `parec` a rate: the recipient of the negotiated format is
//! always the system audio stack, never our own DSP.
//!
//! # Why the `windows` crate (and only here)
//!
//! WASAPI is COM, and `windows-sys` — the binding used everywhere else in
//! this crate — ships no COM interfaces at all. The choice was hand-rolled
//! vtables (hundreds of lines of unsafe, unverifiable on a non-Windows
//! host) or the component bindings. This module takes the bindings, and
//! confines them to audio: every other Win32 call in the crate stays raw.
//!
//! # Threading
//!
//! COM is per-thread, so each thread that touches these objects
//! initializes it once through [`ensure_com`] and uninitializes when it
//! exits. Capture and playback each live on their own audio thread, which
//! is exactly why they can block without stalling input.

use std::ffi::c_void;
use std::time::Duration;

use kvmshare_core::audio::{AudioCapture, AudioDevices, AudioPlayback};
use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::AudioFormat;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator, IAudioCaptureClient,
    IAudioClient, IAudioRenderClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, DEVICE_STATE_ACTIVE, MMDeviceEnumerator, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::{
    PropVariantClear, PropVariantToStringAlloc, PROPVARIANT,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    STGM_READ,
};

/// The audio engine's buffer depth, in 100-nanosecond units (40 ms).
///
/// This is the dominant term in end-to-end latency — far more than the
/// network — so it is chosen for feel, not for throughput: 40 ms is below
/// noticing for music and keeps a volume change immediate.
const LATENCY_HNS: i64 = 40 * 10_000;

/// Bytes per sample in the only codec the protocol negotiates (s16le).
const BYTES_PER_SAMPLE: usize = 2;

/// Wait between polls when a WASAPI call reports nothing available.
/// Bounded and short: the audio thread is dedicated to this, so a sleep is
/// strictly better than a spin (which is what pinned a core in the GUI's
/// discovery bug).
const IDLE_POLL: Duration = Duration::from_millis(2);

/// Initialize COM for this thread, exactly once, and uninitialize it when
/// the thread ends.
///
/// COM apartments are per-thread and must be balanced. A `thread_local`
/// guard is the only correct shape here: a process-wide `Once` would
/// initialize whichever thread happened to run first and leave the
/// audio thread uninitialized, which shows up as an obscure
/// `CO_E_NOTINITIALIZED` at capture start.
fn ensure_com() {
    thread_local! {
        static GUARD: ComGuard = ComGuard::new();
    }
    GUARD.with(|_| ());
}

struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    fn new() -> Self {
        // Free-threaded: these objects are used from one audio thread and
        // never marshalled between apartments.
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Self { initialized }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balanced with the successful CoInitializeEx above,
            // and runs on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The WAVEFORMATEX for a negotiated format: 16-bit PCM, `cbSize` 0.
///
/// `wFormatTag` is the literal `WAVE_FORMAT_PCM` (1) rather than an
/// imported constant, so this compiles against any feature selection of
/// the bindings — the value is fixed by the Windows SDK and never moves.
fn wave_format(format: AudioFormat) -> WAVEFORMATEX {
    let block_align = format.channels as u16 * BYTES_PER_SAMPLE as u16;
    WAVEFORMATEX {
        wFormatTag: 1, // WAVE_FORMAT_PCM
        nChannels: format.channels as u16,
        nSamplesPerSec: format.sample_rate,
        nAvgBytesPerSec: format.sample_rate * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: (BYTES_PER_SAMPLE * 8) as u16,
        cbSize: 0,
    }
}

/// Find the device to use: the named one when `device` names it, else the
/// system default output.
///
/// A configured name is matched case-insensitively against the device's
/// friendly name — the same string the GUI's picker shows and the user
/// sees in Windows' own sound settings.
fn find_device(
    enumerator: &IMMDeviceEnumerator,
    device: &str,
) -> Result<IMMDevice, String> {
    if device.is_empty() || device == "default" {
        // SAFETY: live enumerator; the returned device is an owned ref.
        return unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .map_err(|e| format!("WASAPI default output: {e}"));
    }
    for (name, dev) in list_render_devices(enumerator)? {
        if name.eq_ignore_ascii_case(device) {
            return Ok(dev);
        }
    }
    Err(format!(
        "audio device {device:?} not found (check the name in Windows sound settings)"
    ))
}

/// Every active output device, with its friendly name — used both by the
/// picker and by name matching.
fn list_render_devices(
    enumerator: &IMMDeviceEnumerator,
) -> Result<Vec<(String, IMMDevice)>, String> {
    // SAFETY: live enumerator; DEVICE_STATE_ACTIVE filters to usable
    // endpoints, so a disabled device is never offered or chosen.
    let collection: IMMDeviceCollection =
        unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
            .map_err(|e| format!("WASAPI enumerate outputs: {e}"))?;
    let count = unsafe { collection.GetCount() }.map_err(|e| format!("WASAPI count: {e}"))?;
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: index is bounded by the count just read.
        let Ok(device) = (unsafe { collection.Item(index) }) else {
            continue;
        };
        // A device that cannot produce a name is still usable — fall back
        // to its endpoint id rather than dropping it from the list.
        let name = friendly_name(&device).unwrap_or_else(|| device_id(&device));
        out.push((name, device));
    }
    Ok(out)
}

/// The device's friendly name, as Windows' sound settings show it.
fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: live device; STGM_READ is the documented access mode. The
    // store is released with `device` at the end of this scope.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    let key = windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    // SAFETY: `key` is a static PROPERTYKEY; GetValue returns an owned
    // PROPVARIANT that must be cleared, which happens below on every path.
    let mut value: PROPVARIANT = unsafe { store.GetValue(&key) }.ok()?;
    // SAFETY: an initialized PROPVARIANT; the string is allocated by the
    // shell and freed with CoTaskMemFree.
    let text = unsafe { PropVariantToStringAlloc(&value) }.ok().map(|pwstr| {
        // SAFETY: `pwstr` is a NUL-terminated string owned by us; reading
        // it and then freeing it is the documented contract of
        // PropVariantToStringAlloc.
        let s = unsafe { pwstr.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(pwstr.0 as *const c_void)) };
        s
    });
    // SAFETY: `value` was produced by GetValue and is cleared exactly
    // once here — clearing the original, not a copy, is what releases the
    // string's storage.
    unsafe {
        let _ = PropVariantClear(&mut value);
    }
    text
}

/// The endpoint id — an opaque but stable string, used when a device will
/// not hand over a friendly name.
fn device_id(device: &IMMDevice) -> String {
    // SAFETY: live device; the returned string is task-allocated.
    match unsafe { device.GetId() } {
        Ok(pwstr) => {
            // SAFETY: `pwstr` is a NUL-terminated string owned by us;
            // read, then freed — GetId's documented contract.
            let s = unsafe { pwstr.to_string() }.unwrap_or_else(|_| "unknown device".into());
            unsafe { CoTaskMemFree(Some(pwstr.0 as *const c_void)) };
            s
        }
        Err(_) => "unknown device".into(),
    }
}

/// Build a shared-mode `IAudioClient` on `device`, in `streamflags` mode,
/// converting between our negotiated format and the device's own.
fn open_client(
    device: &IMMDevice,
    format: AudioFormat,
    streamflags: u32,
) -> Result<(IAudioClient, u32), String> {
    // SAFETY: live device; the activation parameters are not needed for
    // an audio client.
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .map_err(|e| format!("WASAPI activate: {e}"))?;
    let wf = wave_format(format);
    // SAFETY: `wf` is a fully-initialized WAVEFORMATEX that outlives the
    // call; AUTOCONVERTPCM is what lets the shared-mode engine accept a
    // format that is not the device's mix format.
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                streamflags | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                LATENCY_HNS,
                0,
                &wf,
                None,
            )
            .map_err(|e| format!("WASAPI initialize (format unsupported?): {e}"))?;
    }
    // SAFETY: initialized client.
    let buffer_frames = unsafe { client.GetBufferSize() }
        .map_err(|e| format!("WASAPI buffer size: {e}"))?;
    Ok((client, buffer_frames))
}

/// Captures this machine's output (loopback).
pub struct WasapiCapture {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    format: AudioFormat,
    block_align: usize,
}

impl WasapiCapture {
    pub fn new(device: &str, format: AudioFormat) -> Result<Self, String> {
        ensure_com();
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("WASAPI enumerator: {e}"))?;
        let device = find_device(&enumerator, device)?;
        let (client, _buffer_frames) = open_client(
            &device,
            format,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
        )?;
        // SAFETY: initialized client; the capture service is available in
        // shared mode (and required for loopback).
        let capture: IAudioCaptureClient = unsafe { client.GetService() }
            .map_err(|e| format!("WASAPI capture service: {e}"))?;
        // SAFETY: initialized client with a capture service.
        unsafe { client.Start() }.map_err(|e| format!("WASAPI start capture: {e}"))?;
        log_debug!(
            "audio capture: WASAPI loopback at {} Hz, {} ch, {} ms frames",
            format.sample_rate,
            format.channels,
            format.frame_ms
        );
        Ok(Self {
            client,
            capture,
            format,
            block_align: format.channels as usize * BYTES_PER_SAMPLE,
        })
    }
}

// SAFETY: the audio traits require `Send`. The COM objects held here are
// not `Send` in the bindings because `IUnknown` wraps a raw pointer, but
// they are safe to move between threads **given this module's usage**:
// COM is initialized `COINIT_MULTITHREADED` (free-threaded, no apartment
// marshalling), WASAPI objects are documented as agile, and each of these
// values is in practice owned by a single dedicated audio thread that
// never shares it. The alternative — re-creating the stream per thread —
// would be strictly worse (a new 40 ms engine buffer each time).
unsafe impl Send for WasapiCapture {}

impl AudioCapture for WasapiCapture {
    fn format(&self) -> Result<AudioFormat, String> {
        Ok(self.format)
    }

    /// Blocking read, per the trait: the caller owns a dedicated audio
    /// thread, so waiting here is what keeps the thread from spinning.
    ///
    /// `AUDCLNT_BUFFERFLAGS_SILENT` is honoured by writing zeros: the flag
    /// means the buffer contents are undefined, and copying them would
    /// inject noise into the stream.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        ensure_com();
        loop {
            // SAFETY: live capture client.
            let available = unsafe { self.capture.GetNextPacketSize() }
                .map_err(|e| format!("WASAPI packet size: {e}"))?;
            if available > 0 {
                break;
            }
            std::thread::sleep(IDLE_POLL);
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        // SAFETY: out-params are valid; device/qpc positions are not
        // requested (loopback does not report them meaningfully).
        unsafe {
            self.capture
                .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                .map_err(|e| format!("WASAPI get buffer: {e}"))?;
        }
        let bytes = frames as usize * self.block_align;
        let n = bytes.min(buf.len());
        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
            buf[..n].fill(0);
        } else if !data.is_null() {
            // SAFETY: the engine guarantees `frames * block_align` bytes
            // at `data` until ReleaseBuffer, and `n <= bytes`.
            unsafe { std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), n) };
        }
        // SAFETY: exactly the frames handed out above, released once.
        unsafe {
            self.capture
                .ReleaseBuffer(frames)
                .map_err(|e| format!("WASAPI release buffer: {e}"))?;
        }
        if bytes > buf.len() {
            // The caller's buffer is smaller than one packet, so audio is
            // being discarded. Worth saying out loud: it presents as
            // choppy playback with no error anywhere.
            log_warn!(
                "audio capture: packet of {bytes} bytes exceeds the {} byte buffer — audio dropped",
                buf.len()
            );
        }
        Ok(n)
    }
}

impl Drop for WasapiCapture {
    fn drop(&mut self) {
        // SAFETY: stopping an initialized stream; ignoring the result is
        // correct during teardown.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Plays the peer's audio into this machine's output.
pub struct WasapiPlayback {
    /// Configured output name; empty or `default` = the system default.
    device: String,
    client: Option<IAudioClient>,
    render: Option<IAudioRenderClient>,
    buffer_frames: u32,
    block_align: usize,
}

impl WasapiPlayback {
    pub fn new(device: &str) -> Self {
        Self {
            device: device.to_string(),
            client: None,
            render: None,
            buffer_frames: 0,
            block_align: 1,
        }
    }
}

// SAFETY: same reasoning as [`WasapiCapture`] — free-threaded COM,
// owned by one dedicated audio thread.
unsafe impl Send for WasapiPlayback {}

impl AudioPlayback for WasapiPlayback {
    fn start(&mut self, format: AudioFormat) -> Result<(), String> {
        self.stop();
        ensure_com();
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .map_err(|e| format!("WASAPI enumerator: {e}"))?;
        let device = find_device(&enumerator, &self.device)?;
        // No LOOPBACK flag: this is a render stream.
        let (client, buffer_frames) = open_client(&device, format, 0)?;
        // SAFETY: initialized render client.
        let render: IAudioRenderClient = unsafe { client.GetService() }
            .map_err(|e| format!("WASAPI render service: {e}"))?;
        // SAFETY: initialized client with a render service.
        unsafe { client.Start() }.map_err(|e| format!("WASAPI start playback: {e}"))?;
        log_debug!(
            "audio playback: WASAPI at {} Hz, {} ch, {buffer_frames} frame buffer",
            format.sample_rate,
            format.channels
        );
        self.block_align = format.channels as usize * BYTES_PER_SAMPLE;
        self.buffer_frames = buffer_frames;
        self.client = Some(client);
        self.render = Some(render);
        Ok(())
    }

    /// Write one packet, waiting for engine room.
    ///
    /// The wait is real backpressure, not a workaround: the engine's
    /// buffer holds ~40 ms, so filling it faster than it drains would mean
    /// the audio plays later and later. Blocking here means the caller's
    /// pacing *is* playback pacing.
    fn write(&mut self, samples: &[u8]) -> Result<(), String> {
        ensure_com();
        let (Some(client), Some(render)) = (self.client.as_ref(), self.render.as_ref()) else {
            return Err("playback not started (call start() first)".to_string());
        };
        let mut written = 0usize;
        while written < samples.len() {
            // SAFETY: live client.
            let padding = unsafe { client.GetCurrentPadding() }
                .map_err(|e| format!("WASAPI padding: {e}"))?;
            let free = self.buffer_frames.saturating_sub(padding) as usize;
            let wanted = (samples.len() - written) / self.block_align;
            let frames = free.min(wanted);
            if frames == 0 {
                // The engine is full: let it drain. This is the audio
                // thread's whole job, so sleeping costs nothing.
                std::thread::sleep(IDLE_POLL);
                continue;
            }
            // SAFETY: live render client; `frames` is within the free
            // space just computed from the engine's own accounting.
            let dst = unsafe { render.GetBuffer(frames as u32) }
                .map_err(|e| format!("WASAPI get render buffer: {e}"))?;
            let byte_count = frames * self.block_align;
            if !dst.is_null() {
                // SAFETY: GetBuffer guarantees `frames * block_align`
                // writable bytes at `dst`.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        samples[written..].as_ptr(),
                        dst,
                        byte_count,
                    )
                };
            }
            // SAFETY: releasing exactly the frames just requested, with no
            // flags (0 = "these frames are valid audio").
            unsafe {
                render
                    .ReleaseBuffer(frames as u32, 0)
                    .map_err(|e| format!("WASAPI release render buffer: {e}"))?;
            }
            written += byte_count;
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(client) = self.client.take() {
            // SAFETY: stopping an initialized stream during teardown.
            unsafe {
                let _ = client.Stop();
            }
        }
        self.render = None;
    }
}

impl Drop for WasapiPlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Lists this machine's audio devices for the GUI's pickers.
pub struct WasapiDevices;

impl WasapiDevices {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WasapiDevices {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevices for WasapiDevices {
    /// Capture devices are **outputs**, because capture is loopback: the
    /// list offers what can be listened to, never microphones.
    fn capture_devices(&self) -> Vec<String> {
        render_device_names()
    }

    fn playback_devices(&self) -> Vec<String> {
        render_device_names()
    }
}

fn render_device_names() -> Vec<String> {
    ensure_com();
    let enumerator: IMMDeviceEnumerator =
        match unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) } {
            Ok(e) => e,
            Err(e) => {
                log_warn!("audio: cannot enumerate Windows output devices: {e}");
                return Vec::new();
            }
        };
    match list_render_devices(&enumerator) {
        Ok(devices) => devices.into_iter().map(|(name, _)| name).collect(),
        Err(e) => {
            log_warn!("audio: cannot list Windows output devices: {e}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The WAVEFORMATEX must describe exactly what the wire carries:
    /// 16-bit PCM at the negotiated rate and channel count. Getting these
    /// wrong is silent corruption — the stream plays, at the wrong speed
    /// or with the channels misaligned. (The struct is `#[repr(packed)]`
    /// in the bindings, so every field is read by copy — a reference to a
    /// packed field is unaligned and rejected by the compiler.)
    #[test]
    fn the_wave_format_describes_the_negotiated_pcm() {
        let fmt = AudioFormat::default();
        let wf = wave_format(fmt);
        assert_eq!({ wf.wFormatTag }, 1, "WAVE_FORMAT_PCM");
        assert_eq!({ wf.nChannels }, 2);
        assert_eq!({ wf.nSamplesPerSec }, 48_000);
        assert_eq!({ wf.wBitsPerSample }, 16);
        assert_eq!({ wf.nBlockAlign }, 4, "2 channels x 2 bytes");
        assert_eq!({ wf.nAvgBytesPerSec }, 48_000 * 4, "rate x block align");
        assert_eq!({ wf.cbSize }, 0, "plain WAVEFORMATEX, no extension");
    }

    /// Mono and a different rate are carried through unchanged — the
    /// negotiated format is what the device is asked for.
    #[test]
    fn a_negotiated_format_is_described_faithfully() {
        let fmt = AudioFormat { sample_rate: 44_100, channels: 1, frame_ms: 20, codec: 0 };
        let wf = wave_format(fmt);
        assert_eq!({ wf.nChannels }, 1);
        assert_eq!({ wf.nSamplesPerSec }, 44_100);
        assert_eq!({ wf.nBlockAlign }, 2);
        assert_eq!({ wf.nAvgBytesPerSec }, 44_100 * 2);
    }

    /// The block align used to size reads and writes must agree with the
    /// format the engine was initialized with, or every copy is skewed.
    #[test]
    fn block_align_matches_the_wave_format() {
        for channels in [1u8, 2, 4] {
            let fmt = AudioFormat { channels, ..Default::default() };
            let wf = wave_format(fmt);
            assert_eq!(wf.nBlockAlign as usize, channels as usize * BYTES_PER_SAMPLE);
        }
    }
}
