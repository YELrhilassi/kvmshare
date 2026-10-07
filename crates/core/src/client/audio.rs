//! The client's side of audio.
//!
//! Deliberately thin: the runtime, the socket, the packet format and the
//! policy all come from [`crate::audio`], and this only connects them to
//! the client's control link. A machine's audio behaviour is therefore the
//! same whether it is acting as the server or as a client, which is the
//! whole point of putting the pipeline in the core.
//!
//! Two jobs:
//!
//! * send `AudioOffer` once the session is up, so the server learns where
//!   to send audio and what this machine accepts;
//! * route the server's audio messages into the runtime and keep them out
//!   of [`crate::client::dispatch`], which knows nothing about audio.

use std::sync::mpsc::Sender;
use std::sync::Arc;

use kvmshare_log::log_warn;
use kvmshare_protocol::message::Message;

use crate::audio::runtime::{AudioBackend, AudioEvents, AudioOptions, AudioRuntime};

/// Carries the runtime's control messages to the client's TCP loop.
///
/// The loop already owns an outbox channel for app-layer traffic, but audio
/// messages must not share it: they are produced by the audio thread at its
/// own cadence, and mixing them into the app's queue would let a stalled
/// audio announcement delay a clipboard or resolution message (and vice
/// versa). A dedicated channel keeps the two independent, drained by the
/// same loop.
struct ClientAudioEvents {
    outbox: Sender<Message>,
}

impl AudioEvents for ClientAudioEvents {
    fn send_control(&self, message: Message) {
        // A closed receiver means the session ended between the audio
        // thread's last two steps; the runtime is being torn down anyway.
        let _ = self.outbox.send(message);
    }
}

/// This client's audio link.
pub struct ClientAudio {
    runtime: AudioRuntime,
}

impl ClientAudio {
    /// Bind the audio socket and prepare the runtime, trusting only the
    /// server this session authenticated against.
    ///
    /// Returns `Err` when the socket cannot be bound or the peer address is
    /// unavailable — a real failure the caller reports, rather than an
    /// audio switch that silently does nothing.
    pub fn start(
        options: AudioOptions,
        backend: Arc<dyn AudioBackend>,
        server_ip: std::net::IpAddr,
        outbox: Sender<Message>,
    ) -> Result<Self, String> {
        let events = Arc::new(ClientAudioEvents { outbox });
        let runtime = AudioRuntime::start(options, backend, events)?;
        runtime.allow_peer(server_ip);
        Ok(Self { runtime })
    }

    /// Announce this machine's audio socket to the server.
    ///
    /// Called once the session is up. A no-op when both directions are off,
    /// so a machine with audio configured off sends nothing.
    pub fn announce(&mut self) {
        self.runtime.announce();
    }

    /// Handle one inbound control message.
    ///
    /// Returns `true` when the message belonged to audio — the caller then
    /// leaves it out of the ordinary dispatch path, so the two can never
    /// both act on it.
    pub fn handle(&mut self, msg: &Message) -> bool {
        match msg {
            Message::AudioOffer { port, formats } => {
                self.runtime.on_peer_offer(*port, formats.clone());
                true
            }
            Message::AudioStart { port, format } => {
                self.runtime.on_peer_start(*port, *format);
                true
            }
            Message::AudioStop => {
                self.runtime.on_peer_stop();
                true
            }
            // The server's "I have audio playing" answer is not the
            // client's business: it feeds the *server's* routing policy,
            // and the server records it there.
            Message::AudioState { .. } => true,
            _ => false,
        }
    }

    /// Report why audio is not running, when it should be.
    pub fn last_error(&self) -> Option<&str> {
        self.runtime.last_error()
    }

    /// Tear the link down. A client's runtime lives exactly as long as one
    /// session, so there is no "stay ready for the next session" state to
    /// keep — the next session builds a new one, on a new socket.
    pub fn stop(&mut self) {
        self.runtime.stop();
    }
}

impl Drop for ClientAudio {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Log an audio failure once, in the shape the rest of the client uses.
pub fn warn_unavailable(reason: &str) {
    log_warn!("audio: {reason}");
}
