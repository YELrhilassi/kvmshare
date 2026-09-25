//! The server's media routing: where a captured media key goes.
//!
//! # What happens to a media key
//!
//! A media key never travels as an ordinary key. It is classified at the
//! capture edge, resolved by policy (see [`crate::media`]), and then one of
//! three things happens:
//!
//! | Resolution | Action |
//! |---|---|
//! | `Remote(id)` | send `MediaControl` to that client |
//! | `Local` | perform it on this machine |
//! | `Drop` | nothing |
//!
//! All three **consume** the key: it is never forwarded as a normal
//! keystroke as well, which is what stops a routed key from also arriving
//! as a stray keypress on a client's focused window.
//!
//! # Why the local case needs help
//!
//! While routing is enabled the media keys are **grabbed** (see
//! [`arm_capture`]), so the local OS does *not* see them. That is what
//! makes `Remote` possible: without it, pressing play while the music is on
//! the other machine would toggle playback on *both*. The price is that the
//! local case must be performed explicitly, which is why `Local` re-injects
//! the command here instead of assuming the OS handled it.
//!
//! Arming the grab and consuming every media key are therefore one
//! indivisible decision: any state where keys are consumed but the grab is
//! off would swallow them, and any state where the grab is on but keys are
//! forwarded would double-act.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kvmshare_log::{log_debug, log_info, log_warn};
use kvmshare_protocol::message::{MediaCommand, Message};

use crate::media::{resolve, MediaContext, MediaPrefs, ResolvedTarget};
use crate::server::client::{route, Client};
use crate::server::Engine;
use crate::session::Session;

/// What the router did with a key, for logging and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routed {
    /// Sent to a client (the screen id).
    Remote(u8),
    /// Performed on this machine.
    Local,
    /// Deliberately discarded.
    Dropped,
}

/// The live pieces a routing decision reads.
///
/// Borrowed rather than taken from a `ClientCtx`: routing happens on the
/// server's input path, which owns these directly, and a decision that
/// only *reads* state should not require a connection-scoped handle.
pub struct MediaRoute<'a> {
    pub prefs: &'a MediaPrefs,
    pub session: &'a Arc<Mutex<Session>>,
    pub clients: &'a Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    pub engine: &'a Mutex<Box<dyn Engine>>,
}

/// Decide and dispatch one media command.
///
/// Pure in its decision (it delegates to [`resolve`]) and explicit in its
/// effects, so a test can drive it with a fake engine and a client map.
pub fn route_command(rt: &MediaRoute<'_>, command: MediaCommand) -> Routed {
    let (focus, last_active) = {
        let session = rt.session.lock().unwrap();
        (session.focus(), session.last_active())
    };
    let prefs = rt.prefs.clone();

    // The decision is made under the clients lock (the pinned target needs
    // the machine-id lookup), and the lock is released before anything is
    // sent or any engine call is made. Both of those can take other locks,
    // and a routing decision must never be able to deadlock the input path.
    let resolved = {
        let clients = rt.clients.lock().unwrap();
        let screen_of = |machine_id: &str| -> Option<u8> {
            clients
                .values()
                .find(|c| {
                    let id = c.machine_id.as_str();
                    id == machine_id || id.starts_with(machine_id) || machine_id.starts_with(id)
                })
                .map(|c| c.id)
        };
        let context = MediaContext {
            focus,
            last_active,
            screen_of: &screen_of,
        };
        resolve(&prefs, command, &context)
    };

    match resolved {
        ResolvedTarget::Remote(id) => {
            // A client that vanished between the decision and here is a
            // miss, not an error — the key is still consumed, because it was
            // consumed the moment routing was on.
            let sender = rt
                .clients
                .lock()
                .unwrap()
                .get(&id)
                .map(|client| client.out.clone());
            match sender {
                Some(sender)
                    if sender.try_send(route(Message::MediaControl { command })).is_ok() =>
                {
                    log_debug!("media: {command:?} routed to client {id}");
                    Routed::Remote(id)
                }
                _ => {
                    log_debug!("media: {command:?} target client {id} was not reachable");
                    Routed::Dropped
                }
            }
        }
        ResolvedTarget::Local => {
            // Always performed locally when the grab is armed (it ate the
            // key); when it is not armed the OS already acted and this is a
            // harmless no-op that also cannot double-act, because the local
            // engine's media injection is only ever called from here.
            if let Ok(mut engine) = rt.engine.lock() {
                engine.media(command);
            }
            Routed::Local
        }
        ResolvedTarget::Drop => Routed::Dropped,
    }
}

/// Arm or disarm the media-key grab on this machine.
///
/// Called from the running loop's startup and whenever the policy changes.
/// Failure is reported and non-fatal: a machine whose window system refuses
/// the grab still routes keys (with the local side acting twice), which is
/// strictly better than refusing to switch the mouse.
pub fn arm_capture(engine: &Arc<std::sync::Mutex<Box<dyn crate::server::Engine>>>, active: bool) {
    let Ok(mut engine) = engine.lock() else {
        return;
    };
    if engine.media_capture_active() == active {
        return;
    }
    match engine.set_media_capture(active) {
        Ok(()) if active => log_info!("media: routing keys through kvmshare (local media keys grabbed)"),
        Ok(()) => log_info!("media: media keys left to the local system"),
        Err(e) => log_warn!(
            "media: could not {} the media-key grab ({e}); media keys will also act on this machine",
            if active { "arm" } else { "disarm" }
        ),
    }
}

/// Whether the policy asks for the grab: routing enabled means keys are
/// consumed, which is only coherent with the grab armed.
pub fn wants_capture(prefs: &MediaPrefs) -> bool {
    prefs.route_media_keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{MediaPrefs, MediaTarget};

    /// The default policy is behaviour-preserving, so its decision falls
    /// out of the same `resolve` the pure tests cover — this only pins the
    /// wiring: routing on means the grab is wanted.
    #[test]
    fn the_default_policy_wants_the_grab() {
        assert!(wants_capture(&MediaPrefs::default()));
    }

    /// Routing off must not grab: with no interception the media keys are
    /// the OS's, exactly as before the feature existed.
    #[test]
    fn routing_off_does_not_want_the_grab() {
        let prefs = MediaPrefs { route_media_keys: false, ..MediaPrefs::default() };
        assert!(!wants_capture(&prefs));
    }

    /// A pinned target that no connected machine matches resolves to the
    /// fallback, i.e. local — never to a screen id nobody occupies.
    #[test]
    fn a_pinned_target_resolves_locally_when_disconnected() {
        let prefs = MediaPrefs {
            route_media_keys: true,
            transport: MediaTarget::Machine("not-connected".into()),
            volume: MediaTarget::Local,
            fallback_local: true,
        };
        let screen_of = |machine_id: &str| -> Option<u8> {
            if machine_id == "connected" {
                Some(3)
            } else {
                None
            }
        };
        let context = MediaContext { focus: None, last_active: None, screen_of: &screen_of };
        assert_eq!(resolve(&prefs, MediaCommand::Next, &context), ResolvedTarget::Local);
    }
}
