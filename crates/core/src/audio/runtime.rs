//! Wiring: the threads that move audio between a device and the socket.
//!
//! This is the only part of the audio path that owns threads, and it owns
//! nothing else — the OS objects arrive through [`AudioBackend`], the
//! control messages leave through [`AudioEvents`], and the datagrams go
//! through [`AudioSocket`]. Nothing here knows which OS it runs on, which
//! is why the same runtime serves Linux and Windows unchanged.
//!
//! # Two threads, and why exactly two
//!
//! * **Capture** reads the local output and sends datagrams. It blocks in
//!   the device.
//! * **Playout** receives datagrams and writes them to the local output.
//!   It blocks in the socket (with a timeout, so it can notice a stop).
//!
//! Both block by design: a device call has real latency and polling for it
//! would burn a core for nothing. Neither is ever the input thread, so
//! neither can stall the cursor.
//!
//! # Teardown
//!
//! Stopping clears the socket's peer **first**. That is the authoritative
//! step: once no peer is trusted, nothing is accepted and nothing can be
//! sent, whatever the threads are in the middle of. The capture thread then
//! unwinds on its own next read rather than being joined — a device read
//! can block until the device has data, and a `join` that waits on a dead
//! audio server would hang the caller (the session teardown path) for an
//! unbounded time. Losing that join is deliberate and safe: the socket is
//! already closed to the peer and the thread holds no other shared state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use kvmshare_log::{log_debug, log_warn};
use kvmshare_protocol::message::{AudioFormat, Message};

use crate::audio::activity::ActivityDetector;
use crate::audio::device::{AudioCapture, AudioPlayback};
use crate::audio::packet::{self, Receiver, Sender};
use crate::audio::transport::{AudioSocket, RECV_BUFFER};

/// How this machine wants to take part in audio.
#[derive(Debug, Clone)]
pub struct AudioOptions {
    /// Send this machine's output to the peer.
    pub send: bool,
    /// Play the peer's audio.
    pub receive: bool,
    /// Device to capture; empty or `default` means the system default
    /// output (whose loopback is captured — never a microphone).
    pub capture_device: String,
    /// Device to play to; empty or `default` means the system default.
    pub playback_device: String,
    /// Silence floor in dBFS for the "is this machine playing" answer.
    pub activity_floor_db: f32,
}

impl Default for AudioOptions {
    fn default() -> Self {
        Self {
            send: false,
            receive: false,
            capture_device: String::new(),
            playback_device: String::new(),
            activity_floor_db: -50.0,
        }
    }
}

impl AudioOptions {
    /// Whether this configuration asks for anything at all. With both
    /// directions off there is no socket to bind and no announcement to
    /// send — the feature costs nothing when unused.
    pub fn is_active(&self) -> bool {
        self.send || self.receive
    }
}

/// Opens the OS audio objects. Implemented by the app on top of
/// `kvmshare-platform`, so the core never names a platform type — the same
/// boundary shape as input capture and injection.
pub trait AudioBackend: Send + Sync {
    /// Open loopback capture at `format`, or explain why not.
    fn open_capture(
        &self,
        device: &str,
        format: AudioFormat,
    ) -> Result<Box<dyn AudioCapture>, String>;

    /// Open a playback stream (started later, once a format is known).
    fn open_playback(&self, device: &str) -> Box<dyn AudioPlayback>;
}

/// What the runtime reports back to its owner, which owns the control link.
pub trait AudioEvents: Send + Sync {
    /// Send a control message to the peer (`AudioStart`/`AudioStop`/
    /// `AudioState`).
    fn send_control(&self, message: Message);
}

/// The formats this build can actually send and receive, in preference
/// order. One place, so negotiation and the announcement can never
/// disagree about what this machine supports.
pub fn accepted_formats() -> Vec<AudioFormat> {
    vec![
        AudioFormat::default(),
        AudioFormat { sample_rate: 48_000, channels: 1, frame_ms: 20, codec: 0 },
        AudioFormat { sample_rate: 44_100, channels: 2, frame_ms: 20, codec: 0 },
        AudioFormat { sample_rate: 44_100, channels: 1, frame_ms: 20, codec: 0 },
    ]
}

/// The live audio side of one role.
pub struct AudioRuntime {
    socket: Arc<AudioSocket>,
    options: AudioOptions,
    backend: Arc<dyn AudioBackend>,
    events: Arc<dyn AudioEvents>,
    /// Where the peer wants audio delivered, from its offer.
    peer_port: Option<u16>,
    /// The format currently being streamed to the peer, when sending.
    sending: Option<AudioFormat>,
    /// The format currently being received, when playing the peer's audio.
    receiving: Option<AudioFormat>,
    /// Set when the whole runtime is being torn down. Threads also watch a
    /// per-thread flag, so one direction can stop without the other.
    stop: Arc<AtomicBool>,
    capture_thread: Option<JoinHandle<()>>,
    /// Ends the current capture thread when sending stops. Held separately
    /// from [`Self::stop`] so stopping one direction leaves the other
    /// running.
    capture_stop: Option<Arc<AtomicBool>>,
    playout_thread: Option<JoinHandle<()>>,
    /// Ends the current playout thread when the peer's stream ends.
    playout_stop: Option<Arc<AtomicBool>>,
    /// Set when capture could not start, so the GUI can say why instead of
    /// showing an enabled switch that does nothing.
    last_error: Option<String>,
}

impl AudioRuntime {
    /// Bind this machine's audio socket and prepare the runtime. No device
    /// is opened and nothing is captured until a peer offer arrives — the
    /// feature is inert until both sides have agreed.
    pub fn start(
        options: AudioOptions,
        backend: Arc<dyn AudioBackend>,
        events: Arc<dyn AudioEvents>,
    ) -> Result<Self, String> {
        let socket = Arc::new(AudioSocket::bind().map_err(|e| format!("bind audio socket: {e}"))?);
        log_debug!("audio: listening on UDP port {}", socket.local_port().unwrap_or(0));
        Ok(Self {
            socket,
            options,
            backend,
            events,
            peer_port: None,
            sending: None,
            receiving: None,
            stop: Arc::new(AtomicBool::new(false)),
            capture_thread: None,
            capture_stop: None,
            playout_thread: None,
            playout_stop: None,
            last_error: None,
        })
    }

    /// This machine's audio port, to announce to the peer.
    pub fn local_port(&self) -> Option<u16> {
        self.socket.local_port().ok()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Trust the authenticated peer's address. Called from the handshake,
    /// before any datagram is accepted.
    pub fn allow_peer(&self, ip: std::net::IpAddr) {
        self.socket.allow_peer(ip);
    }

    /// Tell the peer where we can receive and what we can receive.
    ///
    /// Sent whenever either direction is enabled, not only when receiving:
    /// the peer needs our port to send to us, and we need theirs to send to
    /// them, so one announcement covers both directions and avoids a
    /// configuration that silently does nothing because each side assumed
    /// the other would speak first.
    pub fn announce(&mut self) {
        if !self.options.is_active() {
            return;
        }
        let Some(port) = self.local_port() else {
            self.fail("audio socket has no port");
            return;
        };
        self.events.send_control(Message::AudioOffer {
            port,
            formats: accepted_formats(),
        });
    }

    /// The peer can receive audio here, in these formats. Start sending if
    /// this machine is configured to and the formats allow it.
    pub fn on_peer_offer(&mut self, port: u16, formats: Vec<AudioFormat>) {
        self.peer_port = Some(port);
        if !self.options.send {
            return;
        }
        if self.sending.is_some() {
            // Already streaming; a repeated offer is idempotent.
            return;
        }
        let Some(format) = AudioFormat::negotiate(&accepted_formats(), &formats) else {
            self.fail("no audio format in common with the peer");
            return;
        };
        self.start_sending(format);
    }

    /// The peer is sending audio; start playing it if this machine is
    /// configured to.
    pub fn on_peer_start(&mut self, _port: u16, format: AudioFormat) {
        if !self.options.receive {
            return;
        }
        if self.receiving == Some(format) {
            return;
        }
        self.stop_playout();
        let mut playback = self.backend.open_playback(&self.options.playback_device);
        if let Err(e) = playback.start(format) {
            self.fail(&format!("audio playback: {e}"));
            return;
        }
        self.receiving = Some(format);
        self.last_error = None;
        log_debug!(
            "audio: playing the peer's stream ({} Hz, {} ch)",
            format.sample_rate,
            format.channels
        );
        let own_stop = Arc::new(AtomicBool::new(false));
        self.playout_thread = Some(spawn_playout(
            Arc::clone(&self.socket),
            Arc::clone(&self.stop),
            Arc::clone(&own_stop),
            playback,
            format,
        ));
        self.playout_stop = Some(own_stop);
    }

    /// The peer stopped sending audio.
    pub fn on_peer_stop(&mut self) {
        self.stop_playout();
    }

    /// The peer went away: stop everything, but stay ready to restart when
    /// a new session announces itself.
    pub fn on_peer_gone(&mut self) {
        self.stop_playout();
        self.stop_sending();
        self.peer_port = None;
        // Nothing is trusted until the next handshake, so no late datagram
        // from a stale session can be played.
        self.socket.clear_peer();
    }

    /// Tear the runtime down for good.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Authoritative: with no trusted peer, nothing is accepted or sent
        // regardless of what the threads are doing.
        self.socket.clear_peer();
        self.stop_playout();
        self.stop_sending();
    }

    fn start_sending(&mut self, format: AudioFormat) {
        let Some(peer_port) = self.peer_port else {
            self.fail("no peer audio port to send to");
            return;
        };
        let capture = match self.backend.open_capture(&self.options.capture_device, format) {
            Ok(capture) => capture,
            Err(e) => {
                self.fail(&format!("audio capture: {e}"));
                return;
            }
        };
        let Some(local_port) = self.local_port() else {
            self.fail("audio socket has no port");
            return;
        };
        self.sending = Some(format);
        self.last_error = None;
        // Announce the stream *before* it starts, so the first datagrams
        // never race the message that explains them.
        self.events
            .send_control(Message::AudioStart { port: local_port, format });
        log_debug!(
            "audio: sending this machine's output ({} Hz, {} ch, {} ms frames)",
            format.sample_rate,
            format.channels,
            format.frame_ms
        );
        let own_stop = Arc::new(AtomicBool::new(false));
        self.capture_thread = Some(spawn_capture(
            Arc::clone(&self.socket),
            Arc::clone(&self.stop),
            Arc::clone(&own_stop),
            Arc::clone(&self.events),
            capture,
            format,
            peer_port,
            self.options.activity_floor_db,
        ));
        self.capture_stop = Some(own_stop);
    }

    fn stop_sending(&mut self) {
        if let Some(flag) = self.capture_stop.take() {
            flag.store(true, Ordering::Release);
        }
        if self.sending.take().is_some() {
            self.events.send_control(Message::AudioStop);
        }
        // The capture thread is not joined: see the module docs. It exits
        // on its next read, and it can no longer reach the peer.
        self.capture_thread = None;
    }

    fn stop_playout(&mut self) {
        self.receiving = None;
        if let Some(flag) = self.playout_stop.take() {
            flag.store(true, Ordering::Release);
        }
        if let Some(handle) = self.playout_thread.take() {
            // The playout loop wakes on its socket timeout every frame, so
            // it notices the stop promptly and can be joined safely — which
            // also guarantees the device is released before a new stream
            // opens one.
            let _ = handle.join();
        }
    }

    fn fail(&mut self, reason: &str) {
        log_warn!("audio: {reason}");
        self.last_error = Some(reason.to_string());
    }
}

impl Drop for AudioRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The capture loop: device → packets → socket.
fn spawn_capture(
    socket: Arc<AudioSocket>,
    stop: Arc<AtomicBool>,
    own_stop: Arc<AtomicBool>,
    events: Arc<dyn AudioEvents>,
    mut capture: Box<dyn AudioCapture>,
    format: AudioFormat,
    peer_port: u16,
    activity_floor_db: f32,
) -> JoinHandle<()> {
    let bytes_per_packet = format.bytes_per_packet().max(1);
    thread::Builder::new()
        .name("kvmshare-audio-capture".into())
        .spawn(move || {
            let mut sender = Sender::new(bytes_per_packet, format.frame_ms);
            let mut activity = ActivityDetector::new(activity_floor_db);
            // Room for several packets per read: a device that bursts
            // after a stall must not be forced to drop audio.
            let mut chunk = vec![0u8; bytes_per_packet * 8];
            let mut datagram = Vec::with_capacity(packet::HEADER_LEN + bytes_per_packet);
            let mut announced_playing = false;
            while !stop.load(Ordering::Acquire) && !own_stop.load(Ordering::Acquire) {
                let read = match capture.read(&mut chunk) {
                    Ok(0) => {
                        // Nothing available yet: a very short wait keeps the
                        // loop responsive to `stop` without spinning.
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        // The device is gone. Report it once and end the
                        // stream: retrying a dead device forever is the
                        // spin-loop failure mode, and the user needs to
                        // know rather than hear silence.
                        log_warn!("audio capture stopped: {e}");
                        break;
                    }
                };
                // Activity drives the media router's `last_active_source`,
                // and is reported only when it changes so the control link
                // carries a message per state change, not per packet.
                let playing = activity.feed(&chunk[..read]);
                if playing != announced_playing {
                    announced_playing = playing;
                    events.send_control(Message::AudioState { playing });
                }
                sender.push(&chunk[..read], |seq, timestamp_ms, payload| {
                    packet::encode(&mut datagram, seq, timestamp_ms, payload);
                    if let Err(e) = socket.send(&datagram, peer_port) {
                        // Loss-tolerant by design: a full send buffer or a
                        // peer that has gone away is not a reason to stop
                        // capturing. The session ending is what stops it.
                        log_debug!("audio: datagram not sent ({e})");
                    }
                });
            }
            log_debug!("audio capture thread exiting");
        })
        .expect("spawn audio capture thread")
}

/// The playout loop: socket → jitter buffer → device.
fn spawn_playout(
    socket: Arc<AudioSocket>,
    stop: Arc<AtomicBool>,
    own_stop: Arc<AtomicBool>,
    mut playback: Box<dyn AudioPlayback>,
    format: AudioFormat,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("kvmshare-audio-playout".into())
        .spawn(move || {
            // The timeout *is* the playout cadence: it wakes at roughly one
            // frame interval, so it both waits for audio and notices a stop
            // promptly.
            let timeout = Duration::from_millis(format.frame_ms.max(1) as u64);
            let _ = socket.set_read_timeout(Some(timeout));
            let mut receiver = Receiver::new();
            let mut buf = vec![0u8; RECV_BUFFER];
            while !stop.load(Ordering::Acquire) && !own_stop.load(Ordering::Acquire) {
                match socket.recv(&mut buf) {
                    Ok(Some(len)) => {
                        if let Some(packet) = packet::decode(&buf[..len]) {
                            receiver.accept(&packet);
                        }
                    }
                    Ok(None) => {} // timeout: expected between packets
                    Err(e) => {
                        log_warn!("audio playback stopped: {e}");
                        break;
                    }
                }
                // Play everything that is now in order. Writing only what
                // exists (rather than padding silence) lets the output
                // device's own buffer cover a gap — which is what it is for.
                while let Some(frame) = receiver.pop() {
                    if let Err(e) = playback.write(&frame) {
                        log_warn!("audio playback stopped: {e}");
                        return;
                    }
                }
            }
            playback.stop();
            let stats = receiver.stats();
            log_debug!(
                "audio playout thread exiting (played {}, late {}, overflow {}, underruns {})",
                stats.played,
                stats.late,
                stats.overflow,
                stats.underruns
            );
        })
        .expect("spawn audio playout thread")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A backend that records what it was asked to open and hands back
    /// devices that do nothing. Enough to test the state machine without a
    /// sound card, which is the point of the `AudioBackend` boundary.
    #[derive(Default)]
    struct FakeBackend {
        opened_capture: Mutex<Vec<String>>,
        opened_playback: Mutex<Vec<String>>,
    }

    struct SilentCapture;

    impl AudioCapture for SilentCapture {
        fn format(&self) -> Result<AudioFormat, String> {
            Ok(AudioFormat::default())
        }
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
            Err("test capture has no data".into())
        }
    }

    struct NullPlayback;

    impl AudioPlayback for NullPlayback {
        fn start(&mut self, _format: AudioFormat) -> Result<(), String> {
            Ok(())
        }
        fn write(&mut self, _samples: &[u8]) -> Result<(), String> {
            Ok(())
        }
        fn stop(&mut self) {}
    }

    impl AudioBackend for FakeBackend {
        fn open_capture(
            &self,
            device: &str,
            _format: AudioFormat,
        ) -> Result<Box<dyn AudioCapture>, String> {
            self.opened_capture.lock().unwrap().push(device.to_string());
            Ok(Box::new(SilentCapture))
        }
        fn open_playback(&self, device: &str) -> Box<dyn AudioPlayback> {
            self.opened_playback.lock().unwrap().push(device.to_string());
            Box::new(NullPlayback)
        }
    }

    #[derive(Default)]
    struct RecordedEvents(Mutex<Vec<Message>>);

    impl AudioEvents for RecordedEvents {
        fn send_control(&self, message: Message) {
            self.0.lock().unwrap().push(message);
        }
    }

    /// Build a runtime with the fake backend, plus handles to inspect it.
    fn runtime(
        options: AudioOptions,
    ) -> (AudioRuntime, Arc<FakeBackend>, Arc<RecordedEvents>) {
        let backend = Arc::new(FakeBackend::default());
        let events = Arc::new(RecordedEvents::default());
        let runtime = AudioRuntime::start(
            options,
            Arc::clone(&backend) as Arc<dyn AudioBackend>,
            Arc::clone(&events) as Arc<dyn AudioEvents>,
        )
        .expect("audio socket binds on the loopback-capable wildcard");
        (runtime, backend, events)
    }

    fn offered() -> Vec<AudioFormat> {
        vec![AudioFormat::default()]
    }

    /// Nothing is opened or announced when both directions are off: the
    /// feature must cost nothing when it is not in use.
    #[test]
    fn an_inactive_runtime_opens_and_announces_nothing() {
        let (mut runtime, backend, events) = runtime(AudioOptions::default());
        runtime.announce();
        assert!(events.0.lock().unwrap().is_empty(), "no announcement");
        assert!(backend.opened_capture.lock().unwrap().is_empty());
        assert!(backend.opened_playback.lock().unwrap().is_empty());
    }

    /// Announcing carries this machine's real port and its accepted
    /// formats, so the peer can send to the right place.
    #[test]
    fn announcing_carries_the_socket_port_and_formats() {
        let options = AudioOptions { receive: true, ..AudioOptions::default() };
        let (mut runtime, _backend, events) = runtime(options);
        runtime.announce();
        let messages = events.0.lock().unwrap();
        match messages.first() {
            Some(Message::AudioOffer { port, formats }) => {
                assert_eq!(*port, runtime.local_port().unwrap());
                assert_eq!(*formats, accepted_formats());
            }
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    /// Receiving enabled + a peer offer starts playback and announces the
    /// incoming stream — but only when the peer actually starts sending.
    #[test]
    fn a_peer_stream_starts_playback_when_receiving_is_on() {
        let options = AudioOptions { receive: true, ..AudioOptions::default() };
        let (mut runtime, backend, events) = runtime(options);
        runtime.on_peer_start(4000, AudioFormat::default());
        assert_eq!(backend.opened_playback.lock().unwrap().len(), 1);
        // No AudioStart from us: we are not sending.
        assert!(!events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, Message::AudioStart { .. })));
        runtime.on_peer_stop();
    }

    /// A peer stream is ignored when receiving is off — the switch is
    /// authoritative, not advisory.
    #[test]
    fn a_peer_stream_is_ignored_when_receiving_is_off() {
        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, backend, _events) = runtime(options);
        runtime.on_peer_start(4000, AudioFormat::default());
        assert!(backend.opened_playback.lock().unwrap().is_empty());
    }

    /// Sending enabled + a peer offer opens capture and announces the
    /// stream, because capture is what produces it.
    #[test]
    fn a_peer_offer_starts_sending_when_sending_is_on() {
        let options = AudioOptions {
            send: true,
            capture_device: "test-sink".into(),
            ..AudioOptions::default()
        };
        let (mut runtime, backend, events) = runtime(options);
        runtime.on_peer_offer(4100, offered());
        assert_eq!(backend.opened_capture.lock().unwrap().as_slice(), ["test-sink"]);
        let local_port = runtime.local_port().unwrap();
        let recorded = events.0.lock().unwrap();
        let starts: Vec<&Message> = recorded
            .iter()
            .filter(|m| matches!(m, Message::AudioStart { .. }))
            .collect();
        assert_eq!(starts.len(), 1, "exactly one stream is announced");
        match starts[0] {
            Message::AudioStart { port, format } => {
                assert_eq!(*port, local_port);
                assert_eq!(*format, AudioFormat::default());
            }
            other => panic!("expected a start, got {other:?}"),
        }
        drop(recorded);
        runtime.stop();
    }

    /// Nothing is opened when sending is off, even with a peer offer.
    #[test]
    fn a_peer_offer_does_not_start_sending_when_sending_is_off() {
        let options = AudioOptions { receive: true, ..AudioOptions::default() };
        let (mut runtime, backend, _events) = runtime(options);
        runtime.on_peer_offer(4100, offered());
        assert!(backend.opened_capture.lock().unwrap().is_empty());
    }

    /// A repeated offer must not open a second capture stream — two
    /// capture processes would double every sample.
    #[test]
    fn a_repeated_offer_does_not_start_a_second_stream() {
        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, backend, events) = runtime(options);
        runtime.on_peer_offer(4100, offered());
        runtime.on_peer_offer(4100, offered());
        assert_eq!(backend.opened_capture.lock().unwrap().len(), 1);
        let starts = events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|m| matches!(m, Message::AudioStart { .. }))
            .count();
        assert_eq!(starts, 1);
        runtime.stop();
    }

    /// No common format is a real condition (a peer that can only do a
    /// format this build cannot), and it is reported rather than silently
    /// leaving an enabled switch that produces nothing.
    #[test]
    fn no_common_format_is_reported() {
        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, backend, _events) = runtime(options);
        // A peer that accepts only a channel count we never offer.
        let incompatible = vec![AudioFormat { channels: 8, ..AudioFormat::default() }];
        runtime.on_peer_offer(4100, incompatible);
        assert!(backend.opened_capture.lock().unwrap().is_empty());
        assert!(runtime.last_error().is_some(), "the reason must be reportable");
    }

    /// Going away clears the trusted peer, so a stale session cannot keep
    /// streaming into the new one.
    #[test]
    fn a_departed_peer_stops_and_untrusts() {
        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, _backend, events) = runtime(options);
        runtime.allow_peer(std::net::IpAddr::from([127, 0, 0, 1]));
        runtime.on_peer_offer(4100, offered());
        runtime.on_peer_gone();
        // A stream that had started is stopped on the wire.
        assert!(events
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, Message::AudioStop)));
        // And no datagram can be sent any more.
        assert!(runtime.socket.send(&[0u8; 4], 4100).is_err());
    }
}
