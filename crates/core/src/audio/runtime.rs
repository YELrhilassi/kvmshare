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
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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

/// The level a meter reports when the input is pure silence.
///
/// Digital silence has no level at all (`-inf` dBFS), which is not a value
/// a state file or a UI can carry. This floor is far below anything a person
/// can hear, so it reads unambiguously as "silent" while still being a
/// number the meter can plot.
pub const METER_FLOOR_DB: f32 = -100.0;

/// A point-in-time report of one audio runtime's state, for the app layer
/// to persist and the GUI to show.
///
/// It is deliberately plain data: the runtime and its audio threads both
/// update it, and the sink emits it only when it actually changed, so a
/// quiet link costs nothing.
///
/// The two meters — capture and receive — are what let the GUI answer "is
/// this actually working?" without guessing: a link can be *up* while
/// nothing is playing (`sending` with `capture_playing == false`), and a
/// stream can be flowing into a device that the user hears nothing from
/// (`receiving` with a receive level above the floor is proof that audio is
/// arriving).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AudioStatus {
    /// This machine is configured to send its output.
    pub send: bool,
    /// This machine is configured to play the peer's audio.
    pub receive: bool,
    /// A stream is actually flowing out right now.
    pub sending: bool,
    /// A stream is actually playing in right now.
    pub receiving: bool,
    /// The last failure, cleared when a direction starts successfully.
    pub error: Option<String>,
    /// The machine this link is paired with, as its owner names it. `None`
    /// until a link is settled (or when the owner does not know a name).
    pub peer: Option<String>,
    /// Sound is above the silence floor in what is being captured. A live
    /// stream with this off is "linked, and this machine is quiet", which
    /// is a normal state and not a fault.
    pub capture_playing: bool,
    /// The most recent captured level, in dBFS (never below
    /// [`METER_FLOOR_DB`]). `None` before the first reading.
    pub capture_level_db: Option<f32>,
    /// Sound is above the silence floor in what is being played — the proof
    /// that the peer's audio is actually arriving.
    pub receive_playing: bool,
    /// The most recent received level, in dBFS.
    pub receive_level_db: Option<f32>,
    /// A non-fatal warning about the *capture path*: "this machine's
    /// loopback is muted", "that output's recording gain is at 21%", and
    /// the like.
    ///
    /// It is deliberately not an error. The link is working exactly as
    /// configured; what is missing is sound in it, and the reason is
    /// somewhere a user can fix in seconds — if anyone tells them where to
    /// look. A muted output that only ever appears as "capture: silent" is
    /// the single most expensive silence this feature has.
    pub capture_note: Option<String>,
}

/// Receives audio state transitions.
///
/// Called from whichever thread changed the state, and only when the
/// status actually changed — so an implementation must be fast and must
/// never block (an atomic state-file write is the expected shape; a
/// network call is not).
pub trait AudioStatusSink: Send + Sync {
    fn status(&self, status: AudioStatus);
}

/// The shared status cell: both the runtime's control methods and its
/// audio threads update it, and it emits to the sink on a real change.
struct StatusReporter {
    sink: Mutex<Option<Arc<dyn AudioStatusSink>>>,
    send: bool,
    receive: bool,
    sending: AtomicBool,
    receiving: AtomicBool,
    error: Mutex<Option<String>>,
    peer: Mutex<Option<String>>,
    capture_playing: AtomicBool,
    capture_level: Mutex<Option<f32>>,
    receive_playing: AtomicBool,
    receive_level: Mutex<Option<f32>>,
    capture_note: Mutex<Option<String>>,
    last: Mutex<AudioStatus>,
}

impl StatusReporter {
    fn new(send: bool, receive: bool) -> Self {
        Self {
            sink: Mutex::new(None),
            send,
            receive,
            sending: AtomicBool::new(false),
            receiving: AtomicBool::new(false),
            error: Mutex::new(None),
            peer: Mutex::new(None),
            capture_playing: AtomicBool::new(false),
            capture_level: Mutex::new(None),
            receive_playing: AtomicBool::new(false),
            receive_level: Mutex::new(None),
            capture_note: Mutex::new(None),
            last: Mutex::new(AudioStatus { send, receive, ..AudioStatus::default() }),
        }
    }

    /// Publish (or clear) the capture path's warning. Called when a capture
    /// stream starts, so it describes the device before anyone wonders why
    /// the meter never moves.
    fn set_capture_note(&self, note: Option<String>) {
        *self.capture_note.lock().unwrap() = note;
        self.emit(true);
    }

    /// Name the machine this link is paired with, so the GUI can say *who*
    /// rather than just "linked".
    fn set_peer(&self, peer: Option<String>) {
        *self.peer.lock().unwrap() = peer;
        self.emit(true);
    }

    /// Publish a capture reading. Called from the capture thread, at the
    /// meter's cadence rather than per packet (see [`Meter`]).
    fn set_capture_meter(&self, level: f32, playing: bool) {
        *self.capture_level.lock().unwrap() = Some(level);
        self.capture_playing.store(playing, Ordering::Release);
        self.emit(true);
    }

    /// Publish a receive reading, from the playout thread.
    fn set_receive_meter(&self, level: f32, playing: bool) {
        *self.receive_level.lock().unwrap() = Some(level);
        self.receive_playing.store(playing, Ordering::Release);
        self.emit(true);
    }

    fn set_sink(&self, sink: Arc<dyn AudioStatusSink>) {
        *self.sink.lock().unwrap() = Some(sink);
        // Force one emission so a fresh link reports its configured state
        // even when nothing has started flowing yet (settle=false skips
        // the change check).
        self.emit(false);
    }

    fn set_sending(&self, sending: bool) {
        self.sending.store(sending, Ordering::Release);
        if !sending {
            // A stopped stream has no level: leaving the last reading up
            // would keep a meter dancing on a link that is gone.
            *self.capture_level.lock().unwrap() = None;
            self.capture_playing.store(false, Ordering::Release);
        }
        self.emit(true);
    }

    fn set_receiving(&self, receiving: bool) {
        self.receiving.store(receiving, Ordering::Release);
        if !receiving {
            *self.receive_level.lock().unwrap() = None;
            self.receive_playing.store(false, Ordering::Release);
        }
        self.emit(true);
    }

    fn set_error(&self, error: Option<String>) {
        *self.error.lock().unwrap() = error;
        self.emit(true);
    }

    /// Build the current status and, when `dedupe` is set, emit it only if
    /// it differs from the last one. The guard scopes are deliberate: every
    /// lock is released before the sink is called, so a sink that reads
    /// back into the runtime cannot deadlock.
    fn emit(&self, dedupe: bool) {
        let status = AudioStatus {
            send: self.send,
            receive: self.receive,
            sending: self.sending.load(Ordering::Acquire),
            receiving: self.receiving.load(Ordering::Acquire),
            error: self.error.lock().unwrap().clone(),
            peer: self.peer.lock().unwrap().clone(),
            capture_playing: self.capture_playing.load(Ordering::Acquire),
            capture_level_db: *self.capture_level.lock().unwrap(),
            receive_playing: self.receive_playing.load(Ordering::Acquire),
            receive_level_db: *self.receive_level.lock().unwrap(),
            capture_note: self.capture_note.lock().unwrap().clone(),
        };
        {
            let mut last = self.last.lock().unwrap();
            if dedupe && *last == status {
                return;
            }
            *last = status.clone();
        }
        let sink = self.sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink.status(status);
        }
    }
}

/// Publishes a level reading at a cadence a person can read.
///
/// The capture loop produces a window every few milliseconds, and the state
/// file is written on every change; publishing each window would write that
/// file ~100 times a second for a number no eye can follow, and wake the GUI
/// with every write. So a reading is published only when the level has moved
/// by a visible amount *and* the last publish is old enough to be worth
/// another — with a slower refresh for a level that is standing still, so a
/// meter that has genuinely stopped reads as a meter that is standing still
/// rather than one that froze.
struct Meter {
    /// The last level published (or the current one, before the first).
    held: f32,
    /// The last level a decision was made against.
    last_level: Option<f32>,
    last_emit: Instant,
}

impl Meter {
    /// How long between publishes while the level is moving.
    const INTERVAL: Duration = Duration::from_millis(400);
    /// How long between publishes when it is not (so "silent" stays fresh).
    const IDLE_INTERVAL: Duration = Duration::from_secs(2);
    /// A level change worth a publish, in dB. Less than this is the same
    /// picture to a person watching a meter.
    const CHANGE_DB: f32 = 0.75;

    fn new() -> Self {
        Self { held: METER_FLOOR_DB, last_level: None, last_emit: Instant::now() }
    }

    /// The clamped level this reading represents (digital silence has no
    /// level of its own).
    fn clamp(level: f32) -> f32 {
        if level.is_finite() {
            level.max(METER_FLOOR_DB)
        } else {
            METER_FLOOR_DB
        }
    }

    /// Should this reading be published? Returns the level to publish, or
    /// `None` when it is too soon or the level has not moved.
    fn publish(&mut self, level: f32) -> Option<f32> {
        let level = Self::clamp(level);
        self.held = level;
        let moved = match self.last_level {
            Some(previous) => (level - previous).abs() >= Self::CHANGE_DB,
            None => true,
        };
        let interval = if moved { Self::INTERVAL } else { Self::IDLE_INTERVAL };
        // The first reading is always published: a meter that starts blank
        // and waits half a second to say anything reads as a meter that is
        // not working.
        if self.last_level.is_some() && self.last_emit.elapsed() < interval {
            return None;
        }
        self.last_level = Some(level);
        self.last_emit = Instant::now();
        Some(level)
    }

    /// The level the meter is holding — this reading's, or the last
    /// published one. Used when something other than the cadence forces a
    /// publish (a "playing" transition, say) but the meter is not due.
    fn held(&self) -> f32 {
        self.held
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

    /// A warning about the capture path that is worth showing the user, or
    /// `None` when there is nothing to say.
    ///
    /// Not an error: capture is working as configured. The note exists for
    /// the failures that produce silence rather than a failure — a muted
    /// output (which the loopback tap sees *post*-volume, so it is digital
    /// silence), or a monitor whose recording gain was left low. Neither is
    /// detectable from the audio itself, which is exactly why it has to be
    /// asked for separately and said out loud.
    ///
    /// Default: nothing to say. Only platforms that can answer cheaply
    /// implement it.
    fn capture_note(&self, _device: &str) -> Option<String> {
        None
    }

    /// Silence this machine's own output while its sound is streamed
    /// elsewhere, and report the source to capture in its place.
    ///
    /// Capturing a loopback is a *tap*, not a redirect: without this, the
    /// sound keeps coming out of the local speakers as well, and two
    /// machines sharing one stream both make the same noise. A platform
    /// that can move the system's output (a virtual sink that becomes the
    /// default) returns the device that now carries the sound — and the
    /// machine goes quiet. One that cannot returns `None`, and `device`
    /// stands.
    ///
    /// An `Err` is deliberately not fatal at the call site: sound that is
    /// audible twice is better than no sound, so the caller warns and
    /// captures `device` as it would have.
    ///
    /// Default: nothing to do.
    fn begin_exclusive_send(&self, _device: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Undo [`Self::begin_exclusive_send`]: put this machine's real output
    /// back.
    ///
    /// The platform may defer this briefly (a short grace) so that a control
    /// link that blips and reconnects does not flap the sound back onto this
    /// machine's speakers and then away again; a resumed stream cancels the
    /// pending undo by calling [`Self::begin_exclusive_send`] again. Must be
    /// safe to call when no route is live.
    ///
    /// Default: nothing to do.
    fn end_exclusive_send(&self) {}
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
    /// Shared status, updated by this runtime and its audio threads and
    /// emitted to the sink on every real change.
    status: Arc<StatusReporter>,
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
        let status = Arc::new(StatusReporter::new(options.send, options.receive));
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
            status,
        })
    }

    /// This machine's audio port, to announce to the peer.
    pub fn local_port(&self) -> Option<u16> {
        self.socket.local_port().ok()
    }

    /// Install the sink that receives status transitions. Reports the
    /// current state immediately, so a link that is configured but idle is
    /// still visible to the GUI.
    pub fn set_status_sink(&mut self, sink: Arc<dyn AudioStatusSink>) {
        self.status.set_sink(sink);
    }

    /// Name the machine this link is paired with (the server's client name,
    /// or the server's address on a client), so the GUI can say which
    /// machine the sound is going to or coming from.
    pub fn set_peer_label(&mut self, label: &str) {
        let label = label.trim();
        self.status.set_peer(if label.is_empty() { None } else { Some(label.to_string()) });
    }

    pub fn last_error(&self) -> Option<String> {
        self.status.error.lock().unwrap().clone()
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

    /// Open the path the peer's stream will arrive on.
    ///
    /// A receiver's firewall — and any NAT in between — only admits a
    /// datagram it can match to a flow this machine itself started. A socket
    /// that has only ever *listened* therefore receives nothing: on Windows
    /// that is exactly what happens, and the symptom is the worst kind,
    /// because the link reports "playing" while not one byte arrives. So the
    /// receiving side sends one datagram to the port it is about to receive
    /// from, as soon as it knows it.
    ///
    /// The datagram is not decodable audio (see `packet::PUNCH_VERSION`), so
    /// the peer's decoder drops it and it can never be buffered as the first
    /// frame of a stream. Its only job is to exist.
    fn punch(&self, peer_port: u16) {
        let mut datagram = Vec::with_capacity(packet::HEADER_LEN);
        datagram.extend_from_slice(&packet::MAGIC);
        datagram.push(packet::PUNCH_VERSION);
        // A well-formed header, so the datagram is exactly as long as a
        // real packet's header and nothing about its shape is unusual.
        datagram.extend_from_slice(&[0u8; 8]);
        // Failure is unremarkable: the path may already be open, or the
        // peer may be gone, and audio will still flow if it can.
        if let Err(e) = self.socket.send(&datagram, peer_port) {
            log_debug!("audio: could not open the path to the peer ({e})");
        }
    }

    /// The peer can receive audio here, in these formats. Start sending if
    /// this machine is configured to and the formats allow it.
    pub fn on_peer_offer(&mut self, port: u16, formats: Vec<AudioFormat>) {
        let moved = self.peer_port != Some(port);
        self.peer_port = Some(port);
        if moved {
            // The peer's port is also the one its stream will come from, so
            // open the path now, while there is time to spare (see
            // [`Self::punch`]).
            self.punch(port);
        }
        if !self.options.send {
            return;
        }
        if let Some(format) = self.sending {
            // Already streaming. A repeated offer for the same port is
            // idempotent — but a *new* port means the peer rebuilt its
            // socket (a re-decided link on a server with several machines),
            // and the announcement naming this stream went to the old one.
            // Say it again, or the peer waits for audio that is already
            // flowing at a port nobody told it about.
            if moved {
                if let Some(local_port) = self.local_port() {
                    self.events
                        .send_control(Message::AudioStart { port: local_port, format });
                }
            }
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
        self.status.set_error(None);
        self.status.set_receiving(true);
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
            Arc::clone(&self.status),
            playback,
            format,
            self.options.activity_floor_db,
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
        // Silence this machine first: with the route up, the local speakers
        // are quiet and the sound lives only in the stream. The route also
        // decides *what* to capture — its virtual output, not the real sink
        // nothing is playing to any more.
        let (capture_device, route_note) =
            match self.backend.begin_exclusive_send(&self.options.capture_device) {
                Ok(Some(device)) => (device, None),
                Ok(None) => (self.options.capture_device.clone(), None),
                Err(e) => (
                    self.options.capture_device.clone(),
                    Some(format!(
                        "could not make this machine silent while sharing ({e}) — its speakers \
                         will still play what you send"
                    )),
                ),
            };
        let capture = match self.backend.open_capture(&capture_device, format) {
            Ok(capture) => capture,
            Err(e) => {
                // No stream, so no reason to keep the output rerouted.
                self.backend.end_exclusive_send();
                self.fail(&format!("audio capture: {e}"));
                return;
            }
        };
        let Some(local_port) = self.local_port() else {
            self.fail("audio socket has no port");
            return;
        };
        // Ask the platform whether this device can actually produce sound
        // *before* the user spends an evening on "the link is up and my
        // meter never moves". Reported, never fatal: the stream is exactly
        // what was asked for. A route that could not be established takes
        // precedence — it is the reason the speakers are not quiet.
        let note = route_note.or_else(|| self.backend.capture_note(&capture_device));
        self.status.set_capture_note(note);
        if let Some(note) = self.status.capture_note.lock().unwrap().clone() {
            log_warn!("audio: {note}");
        }
        self.sending = Some(format);
        self.status.set_error(None);
        self.status.set_sending(true);
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
            Arc::clone(&self.status),
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
        let was_sending = self.sending.take().is_some();
        if was_sending {
            self.events.send_control(Message::AudioStop);
        }
        // The note is about the capture that just stopped.
        self.status.set_capture_note(None);
        self.status.set_sending(false);
        // The capture thread is not joined: see the module docs. It exits
        // on its next read, and it can no longer reach the peer.
        self.capture_thread = None;
        // Put this machine's real output back — but only if a stream was
        // actually sending. Doing it last means the capture thread is
        // already unwinding, so nothing reads the virtual output after it
        // is gone.
        if was_sending {
            self.backend.end_exclusive_send();
        }
    }

    fn stop_playout(&mut self) {
        self.receiving = None;
        self.status.set_receiving(false);
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
        self.status.set_error(Some(reason.to_string()));
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
    status: Arc<StatusReporter>,
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
            // The live meter the GUI reads (see `Meter`): a reading per
            // window would write the state file 100 times a second.
            let mut meter = Meter::new();
            let mut reported_playing = false;
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
                        // know rather than hear silence. The peer is told
                        // too, so it stops waiting for audio that will
                        // never arrive.
                        log_warn!("audio capture stopped: {e}");
                        status.set_error(Some(format!("capture stopped: {e}")));
                        status.set_sending(false);
                        events.send_control(Message::AudioStop);
                        break;
                    }
                };
                // Activity drives the media router's `last_active_source`,
                // and is reported only when it changes so the control link
                // carries a message per state change, not per packet.
                let level = ActivityDetector::rms_dbfs(&chunk[..read]);
                let playing = activity.feed_level(level);
                if playing != announced_playing {
                    announced_playing = playing;
                    events.send_control(Message::AudioState { playing });
                }
                // The meter: on its own cadence, plus immediately on a
                // playing/silent transition — "is anything playing here"
                // is the answer the user is looking for, and it must not
                // wait for the meter to be due.
                let published = meter.publish(level);
                if published.is_some() || playing != reported_playing {
                    reported_playing = playing;
                    status.set_capture_meter(meter.held(), playing);
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
    status: Arc<StatusReporter>,
    mut playback: Box<dyn AudioPlayback>,
    format: AudioFormat,
    activity_floor_db: f32,
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
            // The same meter as the capture side, so the page can show what
            // is arriving as well as what is being sent. A receive level
            // above the floor is proof the peer's audio is not just
            // announced but actually landing.
            let mut activity = ActivityDetector::new(activity_floor_db);
            let mut meter = Meter::new();
            let mut reported_playing = false;
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
                        status.set_error(Some(format!("playback stopped: {e}")));
                        status.set_receiving(false);
                        break;
                    }
                }
                // Play everything that is now in order. Writing only what
                // exists (rather than padding silence) lets the output
                // device's own buffer cover a gap — which is what it is for.
                while let Some(frame) = receiver.pop() {
                    let level = ActivityDetector::rms_dbfs(&frame);
                    let playing = activity.feed_level(level);
                    let published = meter.publish(level);
                    if published.is_some() || playing != reported_playing {
                        reported_playing = playing;
                        status.set_receive_meter(meter.held(), playing);
                    }
                    if let Err(e) = playback.write(&frame) {
                        log_warn!("audio playback stopped: {e}");
                        status.set_error(Some(format!("playback stopped: {e}")));
                        status.set_receiving(false);
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
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    /// A backend that records what it was asked to open and hands back
    /// devices that do nothing. Enough to test the state machine without a
    /// sound card, which is the point of the `AudioBackend` boundary.
    #[derive(Default)]
    struct FakeBackend {
        opened_capture: Mutex<Vec<String>>,
        opened_playback: Mutex<Vec<String>>,
        /// What `begin_exclusive_send` reports: `Some(dev)` = the machine is
        /// silenced and capture must read `dev` instead. Empty = the
        /// platform has no route. `route_fails` makes it return an error.
        route_to: Mutex<Option<String>>,
        route_fails: AtomicBool,
        began_route: AtomicUsize,
        ended_route: AtomicUsize,
    }

    struct SilentCapture;

    impl AudioCapture for SilentCapture {
        fn format(&self) -> Result<AudioFormat, String> {
            Ok(AudioFormat::default())
        }
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, String> {
            // "Nothing available yet": a passive stream, so a test can
            // assert on the state a live capture would hold rather than
            // racing the thread's exit.
            Ok(0)
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
        fn begin_exclusive_send(&self, _device: &str) -> Result<Option<String>, String> {
            self.began_route.fetch_add(1, Ordering::SeqCst);
            if self.route_fails.load(Ordering::SeqCst) {
                return Err("no audio server".into());
            }
            Ok(self.route_to.lock().unwrap().clone())
        }
        fn end_exclusive_send(&self) {
            self.ended_route.fetch_add(1, Ordering::SeqCst);
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

    /// Sending is exclusive: the platform is asked to silence this machine,
    /// and capture moves to the device it reports. Without this, the sound
    /// keeps coming out of the local speakers while it is also streamed.
    #[test]
    fn sending_silences_this_machine_and_captures_the_routed_device() {
        let options = AudioOptions {
            send: true,
            capture_device: "test-sink".into(),
            ..AudioOptions::default()
        };
        let (mut runtime, backend, _events) = runtime(options);
        *backend.route_to.lock().unwrap() = Some("kvmshare_send.monitor".into());
        runtime.on_peer_offer(4100, offered());
        assert_eq!(backend.began_route.load(Ordering::SeqCst), 1);
        assert_eq!(
            backend.opened_capture.lock().unwrap().as_slice(),
            ["kvmshare_send.monitor"],
            "capture follows the routed output, not the configured device"
        );
        // Stopping hands the real output back — exactly once.
        runtime.stop_sending();
        assert_eq!(backend.ended_route.load(Ordering::SeqCst), 1);
        runtime.stop();
        assert_eq!(backend.ended_route.load(Ordering::SeqCst), 1, "stop is not double-released");
    }

    /// A platform that cannot route the output still sends: capture stays on
    /// the configured device, and the reason the speakers are not quiet is
    /// reported rather than silently swallowed.
    #[test]
    fn a_route_that_cannot_be_established_still_sends_and_explains_itself() {
        let options = AudioOptions {
            send: true,
            capture_device: "test-sink".into(),
            ..AudioOptions::default()
        };
        let (mut runtime, backend, _events) = runtime(options);
        backend.route_fails.store(true, Ordering::SeqCst);
        runtime.on_peer_offer(4100, offered());
        assert_eq!(backend.opened_capture.lock().unwrap().as_slice(), ["test-sink"]);
        let note = runtime.status.capture_note.lock().unwrap().clone().unwrap_or_default();
        assert!(note.contains("speakers"), "{note}");
        runtime.stop();
    }

    /// A platform with no exclusive route (`None`) captures exactly what was
    /// configured — the behaviour every non-Linux backend has.
    #[test]
    fn a_platform_with_no_route_captures_the_configured_device() {
        let options = AudioOptions {
            send: true,
            capture_device: "test-sink".into(),
            ..AudioOptions::default()
        };
        let (mut runtime, backend, _events) = runtime(options);
        runtime.on_peer_offer(4100, offered());
        assert_eq!(backend.opened_capture.lock().unwrap().as_slice(), ["test-sink"]);
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

    /// Learning the peer's audio port opens the path to it: one datagram,
    /// deliberately not decodable as audio.
    ///
    /// Without this the receiving side is a pure listener, and a firewall or
    /// NAT that admits nothing it did not see leave the machine simply drops
    /// the stream — the failure that looks like "the link says playing and
    /// no sound is heard".
    #[test]
    fn learning_the_peer_port_opens_the_path_to_it() {
        // A stand-in for the peer's audio socket.
        let listener = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let peer_port = listener.local_addr().unwrap().port();

        // Receiving only: this machine has nothing to send, and must still
        // open the path its incoming stream will use.
        let options = AudioOptions { receive: true, ..AudioOptions::default() };
        let (mut runtime, _backend, _events) = runtime(options);
        runtime.allow_peer(std::net::IpAddr::from([127, 0, 0, 1]));
        runtime.on_peer_offer(peer_port, offered());

        let mut buf = [0u8; packet::HEADER_LEN];
        let (len, _from) = listener.recv_from(&mut buf).expect("the path was opened");
        assert_eq!(len, packet::HEADER_LEN, "a full header, nothing more");
        assert_eq!(&buf[..4], &packet::MAGIC);
        assert_eq!(buf[4], packet::PUNCH_VERSION);
        assert_eq!(packet::decode(&buf[..len]), None, "a punch is never buffered as audio");
        runtime.stop();
    }

    /// A peer that moves to a new port has not been told about this
    /// machine's stream — the announcement went to the old socket — so it is
    /// re-announced. Without this, a server that re-decides which machine it
    /// listens to would sit silent while the sender streamed into a port
    /// nobody had named.
    #[test]
    fn a_new_peer_port_re_announces_the_stream() {
        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, _backend, events) = runtime(options);
        runtime.on_peer_offer(4100, offered());
        let local_port = runtime.local_port().unwrap();
        assert_eq!(starts(&events), 1, "the first offer announces once");
        // The same port again changes nothing.
        runtime.on_peer_offer(4100, offered());
        assert_eq!(starts(&events), 1);
        // A new port is a new socket over there: announce to it.
        runtime.on_peer_offer(5000, offered());
        assert_eq!(starts(&events), 2, "a moved peer must be told again");
        match events.0.lock().unwrap().last().cloned() {
            Some(Message::AudioStart { port, .. }) => assert_eq!(port, local_port),
            other => panic!("expected a start, got {other:?}"),
        }
        runtime.stop();
    }

    /// Count the `AudioStart` announcements a runtime has produced.
    fn starts(events: &Arc<RecordedEvents>) -> usize {
        events
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|m| matches!(m, Message::AudioStart { .. }))
            .count()
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

    /// The meter publishes a first reading at once, then holds its tongue
    /// until the level has moved and enough time has passed — that pacing is
    /// what keeps a state file write from becoming a per-packet cost.
    #[test]
    fn the_meter_publishes_once_and_then_waits() {
        let mut meter = Meter::new();
        assert_eq!(meter.publish(-20.0), Some(-20.0), "the first reading is published");
        // No time has passed and the level did not move: nothing to say.
        assert_eq!(meter.publish(-20.0), None);
        // Time still has not passed, so even a move waits its turn — but the
        // meter holds the new reading for whoever asks next.
        assert_eq!(meter.publish(-3.0), None);
        assert_eq!(meter.held(), -3.0);
    }

    /// Digital silence has no level of its own; the meter clamps it to the
    /// floor rather than handing the UI a `-inf` to render.
    #[test]
    fn the_meter_clamps_silence_to_the_floor() {
        let mut meter = Meter::new();
        assert_eq!(meter.publish(f32::NEG_INFINITY), Some(METER_FLOOR_DB));
        assert_eq!(meter.held(), METER_FLOOR_DB);
    }

    /// A stopped direction reports no level at all — a meter left dancing on
    /// a link that is gone is exactly the kind of stale reading the page must
    /// not show.
    #[test]
    fn stopping_a_direction_clears_its_meter() {
        #[derive(Default)]
        struct Recording(Mutex<Vec<AudioStatus>>);
        impl AudioStatusSink for Recording {
            fn status(&self, status: AudioStatus) {
                self.0.lock().unwrap().push(status);
            }
        }

        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let (mut runtime, _backend, _events) = runtime(options);
        let sink = Arc::new(Recording::default());
        runtime.set_status_sink(sink.clone());
        runtime.status.set_capture_meter(-12.0, true);
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.capture_level_db, Some(-12.0));
        assert!(last.capture_playing);

        runtime.stop_sending();
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.capture_level_db, None);
        assert!(!last.capture_playing);
    }

    /// Naming the peer reaches the sink, so the GUI can say which machine the
    /// sound is going to instead of only that a link exists.
    #[test]
    fn a_peer_label_is_reported() {
        #[derive(Default)]
        struct Recording(Mutex<Vec<AudioStatus>>);
        impl AudioStatusSink for Recording {
            fn status(&self, status: AudioStatus) {
                self.0.lock().unwrap().push(status);
            }
        }

        let (mut runtime, _backend, _events) = runtime(AudioOptions::default());
        let sink = Arc::new(Recording::default());
        runtime.set_status_sink(sink.clone());
        runtime.set_peer_label("hp");
        assert_eq!(sink.0.lock().unwrap().last().unwrap().peer.as_deref(), Some("hp"));
        // A blank label is "no peer", not a peer with an empty name.
        runtime.set_peer_label("  ");
        assert_eq!(sink.0.lock().unwrap().last().unwrap().peer, None);
    }

    /// A warning about the capture path reaches the sink, so the page can
    /// say *why* a healthy-looking stream is silent — and goes away with the
    /// stream it described, because it was about that capture.
    #[test]
    fn a_capture_note_is_reported_and_cleared() {
        #[derive(Default)]
        struct Recording(Mutex<Vec<AudioStatus>>);
        impl AudioStatusSink for Recording {
            fn status(&self, status: AudioStatus) {
                self.0.lock().unwrap().push(status);
            }
        }

        /// A backend that always has something to say about capturing.
        struct NotedBackend;
        impl AudioBackend for NotedBackend {
            fn open_capture(
                &self,
                _device: &str,
                _format: AudioFormat,
            ) -> Result<Box<dyn AudioCapture>, String> {
                Ok(Box::new(SilentCapture))
            }
            fn open_playback(&self, _device: &str) -> Box<dyn AudioPlayback> {
                Box::new(NullPlayback)
            }
            fn capture_note(&self, _device: &str) -> Option<String> {
                Some("the output is muted".into())
            }
        }

        let options = AudioOptions { send: true, ..AudioOptions::default() };
        let events = Arc::new(RecordedEvents::default());
        let mut runtime = AudioRuntime::start(
            options,
            Arc::new(NotedBackend) as Arc<dyn AudioBackend>,
            Arc::clone(&events) as Arc<dyn AudioEvents>,
        )
        .expect("audio socket binds");
        let sink = Arc::new(Recording::default());
        runtime.set_status_sink(sink.clone());
        // Nothing to say before a capture starts: the note describes a
        // stream, and there is none yet.
        assert_eq!(sink.0.lock().unwrap().last().unwrap().capture_note, None);

        runtime.on_peer_offer(4100, offered());
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.capture_note.as_deref(), Some("the output is muted"));

        runtime.stop_sending();
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.capture_note, None, "the note goes with its stream");
        runtime.stop();
    }

    /// The status sink sees every real transition and nothing else: the
    /// configured state on install, each direction starting, and both
    /// directions off at teardown.
    #[test]
    fn status_transitions_reach_the_sink() {
        #[derive(Default)]
        struct Recording(Mutex<Vec<AudioStatus>>);
        impl AudioStatusSink for Recording {
            fn status(&self, status: AudioStatus) {
                self.0.lock().unwrap().push(status);
            }
        }

        let options = AudioOptions { send: true, receive: true, ..AudioOptions::default() };
        let (mut runtime, _backend, _events) = runtime(options);
        let sink = Arc::new(Recording::default());
        runtime.set_status_sink(sink.clone());

        // Installing the sink reports the configured-but-idle state.
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert!(last.send && last.receive);
        assert!(!last.sending && !last.receiving);

        runtime.on_peer_offer(4100, offered());
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert!(last.sending, "starting a stream must be reported");

        runtime.on_peer_start(4100, AudioFormat::default());
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert!(last.receiving, "playback must be reported");

        runtime.stop();
        let last = sink.0.lock().unwrap().last().cloned().unwrap();
        assert!(!last.sending && !last.receiving, "teardown reports both directions off");
    }
}
