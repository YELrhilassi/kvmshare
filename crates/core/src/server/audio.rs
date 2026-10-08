//! The server's side of audio: which client to pair with, and the glue
//! between that client's control link and its audio runtime.
//!
//! # Which client does the server share audio with?
//!
//! A server may have several clients, but an audio link is inherently
//! pairwise, so the choice must be **explicit rather than guessed**. In
//! order:
//!
//! 1. If `[audio] peer` names a machine id, that machine is the peer — and
//!    no other ever is.
//! 2. Otherwise, with exactly one client connected there is nothing to
//!    choose, so that client is the peer.
//! 3. Otherwise (several connected, none pinned) the two directions are
//!    answered differently, because they are not the same kind of question:
//!
//!    * **sending** — "send this machine's output to a machine the user
//!      chose" has no defensible answer when there are three machines, and
//!      silently picking one (first connected, lowest id, ...) would stream
//!      a user's audio somewhere they never chose. The link stops, and the
//!      Media page offers the connected machines to pick from, which writes
//!      `peer` and settles it.
//!    * **receiving** — which machine the user wants to *listen to* is not
//!      guessed either, but neither is it unknowable: each client says
//!      whether it has something playing (the same `AudioState` answer the
//!      media router follows). A machine whose sound is playing is a fact
//!      observed from the wire, not an inference, so the link follows it —
//!      see [`on_peer_activity`]. Pinning still outranks it, and a machine
//!      that stops playing hands the link back.
//!
//! Rule 3's sending half is the part worth being careful about: refusing is
//! the honest behaviour, and setting `peer` is the fix.

use std::sync::Arc;

use kvmshare_log::log_info;
use kvmshare_protocol::message::Message;

use crate::audio::runtime::{
    AudioBackend, AudioEvents, AudioOptions, AudioRuntime, AudioStatusSink,
};
use crate::server::client::{route, Client, ClientCtx, Outbound};

/// The server's audio configuration, shared by every connection.
pub struct ServerAudio {
    pub backend: Arc<dyn AudioBackend>,
    pub options: AudioOptions,
    /// The machine intended as the audio peer. `None` means "the only
    /// connected client" (see the module docs).
    pub peer_machine_id: Option<String>,
    /// Where status transitions go (the app layer persists them for the
    /// GUI). `None` means the GUI cannot see live audio state.
    pub status: Option<Arc<dyn AudioStatusSink>>,
}

/// Hand-written: the backend is a trait object (see the same reasoning on
/// the client's `AudioSetup`), and what a log line or a debug dump wants
/// from a setup is the options and the chosen peer.
impl std::fmt::Debug for ServerAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerAudio")
            .field("options", &self.options)
            .field("peer", &self.peer_machine_id)
            .finish_non_exhaustive()
    }
}

/// The peer an `[audio] peer` value names, or `None` when it names none.
///
/// A blank value means "unset", not "a machine id that matches nothing" —
/// the latter would silently disable audio for someone who left the default
/// line in their config, which is the kind of quiet failure this feature is
/// supposed to avoid. One definition, used by both the decision and the
/// teardown rule so they cannot disagree.
pub fn configured_peer(peer: Option<&str>) -> Option<&str> {
    peer.map(str::trim).filter(|s| !s.is_empty())
}

impl ServerAudio {
    /// The peer this server's configuration names, if any.
    pub fn configured_peer(&self) -> Option<&str> {
        configured_peer(self.peer_machine_id.as_deref())
    }

    /// Whether a connection should host the audio link.
    ///
    /// Pure, so the rule is testable without a server, a socket, or a
    /// machine — the same reason [`crate::media::resolve`] is pure.
    ///
    /// `others` is how many *other* clients were connected when this one
    /// arrived.
    pub fn should_host(&self, machine_id: &str, others: usize) -> bool {
        match self.configured_peer() {
            // A configured peer is authoritative: it matches, or nobody
            // does. Short ids are accepted as a prefix, the same forgiveness
            // the trust policy already applies to machine ids (users paste
            // whatever they copied).
            Some(wanted) => {
                machine_id == wanted
                    || machine_id.starts_with(wanted)
                    || wanted.starts_with(machine_id)
            }
            // No peer configured: only unambiguous when alone.
            None => others == 0,
        }
    }
}

/// Carries the runtime's control messages to one client.
///
/// It holds the client's outbound queue rather than the client itself: the
/// client owns the runtime, so a strong `Arc<Client>` here would be a
/// reference cycle and the client would never be freed.
pub struct ClientAudioEvents {
    pub out: std::sync::mpsc::SyncSender<Outbound>,
}

impl AudioEvents for ClientAudioEvents {
    fn send_control(&self, message: Message) {
        // `try_send`: a full queue means the link is already struggling,
        // and blocking the audio thread on it would stall capture. Audio
        // is loss-tolerant by design; dropping the announcement is safer
        // than wedging the thread that produces it.
        let _ = self.out.try_send(route(message));
    }
}

/// Settle the audio link for the client set as it now stands: attach this
/// client when it is the peer, and drop any link that is no longer valid.
///
/// One entry point, so the two halves of the rule — "the configured peer is
/// the peer" and "being alone is not a peer choice" — can never disagree.
/// A new client arriving is exactly the event that can invalidate an
/// existing link, which is why both decisions happen here together.
///
/// Returns whether audio is now running for `client`.
pub fn reconcile(
    ctx: &ClientCtx,
    client: &Client,
    peer_ip: std::net::IpAddr,
    others: &[Arc<Client>],
) -> bool {
    let Some(setup) = ctx.audio.as_ref() else {
        return false;
    };
    if !setup.options.is_active() {
        return false;
    }
    let configured = setup.configured_peer();

    if setup.should_host(&client.machine_id, others.len()) {
        let attached = attach(ctx, client, peer_ip);
        if attached {
            // The configured peer has connected: whoever held the link
            // before is no longer the peer.
            for other in others {
                detach(other, "the configured audio peer connected");
            }
        }
        return attached;
    }

    if configured.is_none() {
        // No peer was named and the machine is no longer alone, so the link
        // has become ambiguous. What that costs depends on the direction
        // (see the module docs): the server's own sound must never be sent
        // to a machine the user did not choose, while *incoming* sound is
        // re-decided per machine by [`on_peer_activity`] and is left alone
        // here.
        if let Some(reason) = ambiguity_reason(setup.options.send) {
            for other in others {
                detach(other, reason);
            }
        }
    }
    false
}

/// Why an unpinned link stops when the client set grows — or `None` when
/// growing is not what breaks it.
///
/// Pure, so the rule that "sending is refused when it is ambiguous, and
/// receiving is not" is pinned by a test rather than by prose: a server
/// whose own sound was quietly streamed to a machine the user never chose
/// is the failure this exists to prevent.
pub fn ambiguity_reason(send: bool) -> Option<&'static str> {
    if send {
        Some("a second machine connected; choose which one shares audio")
    } else {
        None
    }
}

/// Re-decide the link when a client answers "do you have audio playing?".
///
/// Only the *receiving* direction is decided this way, and only while no
/// machine is pinned: the answer comes from the wire, so it is evidence
/// rather than a guess (see the module docs). The rules are deliberately
/// sticky, because a link that follows activity naively flaps between
/// machines whenever two of them are playing:
///
/// * a machine that starts playing takes the link **only when no other
///   machine holds it** — an established link is not interrupted;
/// * a machine that stops playing releases it **only if it holds it**, and
///   nothing takes its place until some machine says it is playing.
///
/// Both halves are driven by the same event, so there is one place to look
/// when the link is on a machine the user did not expect.
pub fn on_peer_activity(ctx: &ClientCtx, client: &Client, playing: bool) {
    let Some(setup) = ctx.audio.as_ref() else {
        return;
    };
    if !setup.options.is_active() {
        return;
    }
    // A pinned machine is the user's decision: activity never overrules it.
    if setup.configured_peer().is_some() {
        return;
    }
    // Choosing where to *send* this machine's sound is never inferred.
    if !setup.options.receive {
        return;
    }

    if !playing {
        if client.audio.lock().unwrap().is_some() {
            detach(client, "the machine that was playing went quiet");
        }
        return;
    }
    if client.audio.lock().unwrap().is_some() {
        return;
    }
    // Somebody else already holds the link, and it is not this machine's
    // turn to take it away: only a release hands it on.
    let held_by_another = ctx
        .peers
        .lock()
        .unwrap()
        .all()
        .iter()
        .any(|c| c.id != client.id && c.audio.lock().unwrap().is_some());
    if held_by_another {
        return;
    }
    // The address the handshake came from, which is the only one trusted —
    // the same rule the connect path applies.
    let Some(ip) = ctx.peers.lock().unwrap().tcp_ip_of(client.id) else {
        return;
    };
    attach(ctx, client, ip);
}

/// Attach an audio runtime to `client` when the rules say it is the peer.
///
/// Start the link. Called only by [`reconcile`], which owns the decision —
/// so this does not re-check `should_host` and the two can never disagree.
///
/// Returns whether audio was started, so the caller can log the decision
/// once, next to the connect line.
fn attach(ctx: &ClientCtx, client: &Client, peer_ip: std::net::IpAddr) -> bool {
    let Some(setup) = ctx.audio.as_ref() else {
        return false;
    };
    if !setup.options.is_active() {
        return false;
    }

    let events = Arc::new(ClientAudioEvents { out: client.out.clone() });
    let mut runtime = match AudioRuntime::start(
        setup.options.clone(),
        Arc::clone(&setup.backend),
        events,
    ) {
        Ok(runtime) => runtime,
        Err(e) => {
            // A machine with no bindable socket is a real failure the user
            // must see, not a silently disabled feature.
            kvmshare_log::log_warn!("audio: cannot start for client {}: {e}", client.name);
            return false;
        }
    };
    // Trust only the address the handshake came from — the same rule the
    // cursor stream follows.
    runtime.allow_peer(peer_ip);
    if let Some(sink) = &setup.status {
        runtime.set_status_sink(Arc::clone(sink));
    }
    // Name the peer so the GUI (and the log) says *who* the sound is going
    // to or coming from, not merely that a link exists.
    runtime.set_peer_label(&client.name);
    runtime.announce();
    log_info!(
        "audio: linked with client {} (send {}, receive {})",
        client.name,
        setup.options.send,
        setup.options.receive
    );
    *client.audio.lock().unwrap() = Some(runtime);
    true
}

/// Stop the audio link on `client` if it has one.
///
/// Called when a second client joins and no peer was configured: the link
/// was only ever valid because the machine was alone.
pub fn detach(client: &Client, reason: &str) {
    let had = client.audio.lock().unwrap().is_some();
    if had {
        // Dropping the runtime stops both threads and clears the socket's
        // peer, so no further datagram is sent or accepted.
        *client.audio.lock().unwrap() = None;
        log_info!("audio: unlinked from client {} ({reason})", client.name);
    }
}

/// Route one inbound control message to the client's audio runtime.
///
/// Returns `true` when the message belonged to audio and must not be
/// handled as anything else.
pub fn handle_message(client: &Client, msg: &Message) -> bool {
    let mut guard = client.audio.lock().unwrap();
    let Some(runtime) = guard.as_mut() else {
        // No audio link: a stray audio message is ignored (an older or
        // misconfigured peer), never an error the user has to see.
        return matches!(
            msg,
            Message::AudioOffer { .. }
                | Message::AudioStart { .. }
                | Message::AudioStop
                | Message::AudioState { .. }
        );
    };
    match msg {
        Message::AudioOffer { port, formats } => {
            runtime.on_peer_offer(*port, formats.clone());
            true
        }
        Message::AudioStart { port, format } => {
            runtime.on_peer_start(*port, *format);
            true
        }
        Message::AudioStop => {
            runtime.on_peer_stop();
            true
        }
        // The peer's "something is playing" answer feeds the media
        // router's `last_active_source` policy — recorded by the session,
        // which is where routing decisions are made.
        Message::AudioState { .. } => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the setup under test. The backend and options are never
    /// touched by `should_host`, so a trivial configuration is enough — the
    /// decision is pure.
    fn setup(peer: &str) -> ServerAudio {
        ServerAudio {
            backend: Arc::new(NoBackend),
            options: AudioOptions::default(),
            peer_machine_id: Some(peer.to_string()),
            status: None,
        }
    }

    struct NoBackend;

    impl AudioBackend for NoBackend {
        fn open_capture(
            &self,
            _device: &str,
            _format: kvmshare_protocol::message::AudioFormat,
        ) -> Result<Box<dyn crate::audio::device::AudioCapture>, String> {
            Err("not used by these tests".into())
        }

        fn open_playback(&self, _device: &str) -> Box<dyn crate::audio::device::AudioPlayback> {
            unreachable!("not used by these tests")
        }
    }

    /// A configured peer is authoritative: it matches or nothing does.
    #[test]
    fn a_configured_peer_is_the_only_accepted_peer() {
        let audio = setup("hp-machine-id");
        assert!(audio.should_host("hp-machine-id", 0));
        assert!(audio.should_host("hp-machine-id", 3));
        assert!(!audio.should_host("some-other-machine", 0));
        assert!(
            !audio.should_host("some-other-machine", 3),
            "a configured peer must never be overridden by being alone"
        );
    }

    /// Machine ids are long and users paste prefixes, so a prefix matches
    /// in either direction — the same forgiveness the trust policy has.
    #[test]
    fn machine_ids_match_by_prefix() {
        assert!(setup("98980a4d").should_host("98980a4d9afac273", 0));
        assert!(setup("98980a4d9afac273").should_host("98980a4d", 0));
    }

    /// With no peer configured, audio runs only while the machine is
    /// alone. A second client makes the link ambiguous, so it stops rather
    /// than streaming to an unchosen machine.
    #[test]
    fn an_unconfigured_peer_requires_being_alone() {
        let audio = setup("");
        assert_eq!(audio.configured_peer(), None);
        assert!(audio.should_host("anything", 0));
        assert!(!audio.should_host("anything", 1));
        assert!(!audio.should_host("anything", 2));
    }

    /// A blank `peer` value is treated as unset rather than as a machine id
    /// that matches nothing (which would silently disable audio).
    #[test]
    fn an_empty_configured_peer_behaves_as_unset() {
        assert!(setup("").should_host("anything", 0));
        assert!(setup("   ").should_host("anything", 0));
        assert!(!setup("   ").should_host("anything", 1));
    }

    /// With several machines connected and nothing pinned, only the sending
    /// direction is refused. Receiving is re-decided from each machine's own
    /// "I am playing something" answer (see [`super::on_peer_activity`]), so
    /// it must not be torn down the moment a third machine appears.
    #[test]
    fn several_machines_only_stop_an_unpinned_link_that_sends() {
        assert!(
            ambiguity_reason(true).is_some(),
            "this machine's sound must never be sent to a machine nobody chose"
        );
        assert_eq!(
            ambiguity_reason(false),
            None,
            "receiving follows the machine that is actually playing"
        );
    }
}
