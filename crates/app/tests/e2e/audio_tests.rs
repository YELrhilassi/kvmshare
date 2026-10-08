//! Audio end to end: a real [`Server`] and a real [`Client`] over real
//! TCP/UDP, with a **mock audio backend** standing in for the sound card.
//!
//! This is the audio counterpart of `session_tests`: it proves the whole
//! pipeline — announcement and negotiation over the control link, the
//! dedicated UDP audio socket, the jitter buffer, and playback — actually
//! carries this machine's output to the other machine. The `AudioBackend`
//! boundary is what makes that testable without a sound card: capture
//! produces a known ascending byte pattern, and playback records exactly
//! what it was asked to play.
//!
//! Both directions are exercised, because either machine can be the sender
//! (the pipeline is deliberately symmetric).

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use kvmshare_core::audio::runtime::{AudioBackend, AudioOptions};
use kvmshare_core::audio::{AudioCapture, AudioPlayback};
use kvmshare_core::client::AudioSetup;
use kvmshare_core::media::MediaPrefs;
use kvmshare_core::server::audio::ServerAudio;
use kvmshare_protocol::message::AudioFormat;

/// A capture device that emits an ascending byte pattern, one packet per
/// read, paced at the negotiated frame cadence so the stream is real-time.
struct MockCapture {
    next: u8,
    format: AudioFormat,
}

impl AudioCapture for MockCapture {
    fn format(&self) -> Result<AudioFormat, String> {
        Ok(self.format)
    }

    /// Fill exactly one packet with the next run of the pattern and sleep
    /// for one frame — so the sender produces a believable, contiguous
    /// stream rather than spinning as fast as the socket allows.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let n = buf.len().min(self.format.bytes_per_packet().max(1));
        for byte in &mut buf[..n] {
            *byte = self.next;
            self.next = self.next.wrapping_add(1);
        }
        std::thread::sleep(Duration::from_millis(self.format.frame_ms.max(1) as u64));
        Ok(n)
    }
}

/// A playback device that records every byte it was asked to play.
struct MockPlayback {
    played: Arc<Mutex<Vec<u8>>>,
}

impl AudioPlayback for MockPlayback {
    fn start(&mut self, _format: AudioFormat) -> Result<(), String> {
        Ok(())
    }
    fn write(&mut self, samples: &[u8]) -> Result<(), String> {
        self.played.lock().unwrap().extend_from_slice(samples);
        Ok(())
    }
    fn stop(&mut self) {}
}

/// The mock backend shared by both roles: capture opens are counted, and
/// every playback appends to one recorder.
struct MockBackend {
    played: Arc<Mutex<Vec<u8>>>,
    capture_opens: Arc<AtomicUsize>,
}

impl AudioBackend for MockBackend {
    fn open_capture(
        &self,
        _device: &str,
        format: AudioFormat,
    ) -> Result<Box<dyn AudioCapture>, String> {
        self.capture_opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(MockCapture { next: 0, format }))
    }
    fn open_playback(&self, _device: &str) -> Box<dyn AudioPlayback> {
        Box::new(MockPlayback { played: self.played.clone() })
    }
}

/// Start a server with audio configured, connect one audio-enabled client,
/// and hand back what the *player* recorded plus the capture-open count.
///
/// `server_sends` picks the direction: `true` = the server shares its
/// output with the client; `false` = the client shares its output with the
/// server. The other direction is always the receiver.
fn start_audio_link(server_sends: bool) -> (Harness, Arc<Mutex<Vec<u8>>>, Arc<AtomicUsize>) {
    let played = Arc::new(Mutex::new(Vec::new()));
    let capture_opens = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(MockBackend {
        played: played.clone(),
        capture_opens: capture_opens.clone(),
    });

    let h = start_server_with_audio(
        MediaPrefs::default(),
        Some(Arc::new(ServerAudio {
            backend: backend.clone() as Arc<dyn AudioBackend>,
            options: AudioOptions {
                send: server_sends,
                receive: !server_sends,
                ..AudioOptions::default()
            },
            peer_machine_id: None,
            status: None,
        })),
    );

    let (client, injector, _calls, out_rx) = connect_client(h.port);
    let client = client.with_audio(AudioSetup {
        options: AudioOptions {
            send: !server_sends,
            receive: server_sends,
            ..AudioOptions::default()
        },
        backend: backend.clone() as Arc<dyn AudioBackend>,
        status: None,
    });
    thread::spawn(move || client.run(Box::new(injector), Box::new(NoClipboard), &out_rx, None).unwrap());
    h.wait_for_clients(1);
    (h, played, capture_opens)
}

/// Wait until at least `bytes` have been played, or the deadline passes.
fn wait_for_played(played: &Arc<Mutex<Vec<u8>>>, bytes: usize) -> Vec<u8> {
    for _ in 0..300 {
        let got = played.lock().unwrap().clone();
        if got.len() >= bytes {
            return got;
        }
        thread::sleep(Duration::from_millis(20));
    }
    played.lock().unwrap().clone()
}

/// The stream must arrive in order. The sender emits an ascending byte
/// pattern; the jitter buffer hands packets out strictly in sequence, so a
/// byte that does not follow its predecessor means reordering or a loss
/// the buffer should have absorbed (or surfaced).
fn assert_stream_is_contiguous(bytes: &[u8]) {
    for (i, w) in bytes.windows(2).enumerate() {
        assert_eq!(
            w[1],
            w[0].wrapping_add(1),
            "audio arrived out of order at byte {i}: {} then {}",
            w[0],
            w[1]
        );
    }
}

#[test]
fn audio_flows_from_the_server_to_a_client() {
    let (_h, played, capture_opens) = start_audio_link(true);
    let packet = AudioFormat::default().bytes_per_packet();

    // Pre-roll is four packets, so wait past it to be sure playback began.
    let got = wait_for_played(&played, packet * 4);
    assert!(
        got.len() >= packet,
        "expected the client to play the server's audio, got {} bytes",
        got.len()
    );
    assert!(
        capture_opens.load(Ordering::SeqCst) >= 1,
        "the sending side must have opened capture"
    );
    assert_stream_is_contiguous(&got);
}

#[test]
fn audio_flows_from_a_client_to_the_server() {
    let (_h, played, capture_opens) = start_audio_link(false);
    let packet = AudioFormat::default().bytes_per_packet();

    let got = wait_for_played(&played, packet * 4);
    assert!(
        got.len() >= packet,
        "expected the server to play the client's audio, got {} bytes",
        got.len()
    );
    assert!(
        capture_opens.load(Ordering::SeqCst) >= 1,
        "the sending side must have opened capture"
    );
    assert_stream_is_contiguous(&got);
}
