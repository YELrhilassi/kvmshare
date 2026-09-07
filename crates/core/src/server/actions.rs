//! Executes a session [`Action`] against the world: outbound queues,
//! the active-client latch and the local engine.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use kvmshare_log::{log_debug, log_trace};
use kvmshare_protocol::message::Message;

use crate::server::client::{enqueue, Client};
use crate::server::engine::Engine;
use crate::session::Action;
use crate::time::now_ms;

/// Apply a session [`Action`] to the world. A free function so it can
/// run from the main loop and from the client/UDP threads (which hold
/// only the shared state, not the whole `Server`).
pub fn apply_action(
    action: Action,
    active: &Arc<Mutex<Option<u8>>>,
    clients: &Arc<Mutex<HashMap<u8, Arc<Client>>>>,
    last_heard: &Mutex<HashMap<u8, u64>>,
    engine: &mut MutexGuard<'_, Box<dyn Engine>>,
) -> io::Result<()> {
    match action {
        Action::Nothing => {}
        Action::Send(msg) => {
            if let Some(id) = *active.lock().unwrap() {
                // Relative mouse motion is the hot path (up to hundreds
                // per second) — not even trace logs those.
                if !matches!(msg, Message::MouseMoveRel { .. }) {
                    log_trace!("forward {msg:?} -> client {id}");
                }
                enqueue(clients, id, msg);
            }
        }
        Action::SwitchTo { to, x, y } => {
            // Leave whatever screen we are on.
            if let Some(old) = active.lock().unwrap().take() {
                enqueue(clients, old, Message::Leave { screen_id: old });
            }
            *active.lock().unwrap() = Some(to);
            // The client's beacons only flow while it is active, so its
            // "last heard" goes stale the moment the cursor comes home.
            // Without this reset the beacon watchdog (which fires on
            // silence *while active*) would judge the freshly activated
            // client dead before its first beacon can arrive — dropping
            // every crossing that follows an idle stretch. Reset the
            // clock here so the client gets the full watchdog window to
            // start beaconing; a stream that then goes silent is a real
            // wedge and still gets caught.
            last_heard.lock().unwrap().insert(to, now_ms());
            log_debug!("cursor switched to client {to} at ({x},{y})");
            // `Enter` carries the entry point; the client places its
            // cursor there itself (absolute placement is reserved for
            // entry — the motion stream that follows is relative).
            enqueue(clients, to, Message::Enter { screen_id: to, x, y });
            // From here on, local input must only reach the client:
            // grab the pointer+keyboard so the local desktop does not
            // act on the same physical events being forwarded, and
            // isolate the physical devices at the kernel so even
            // raw-reading apps (browsers, terminals) see nothing.
            engine.grab_input(true);
            engine.isolate_input(true);
            // Hide the local cursor **in place** — it is already at
            // the shared edge where it crossed. Warping it (even
            // hidden) would sweep hover/enter effects across local
            // windows; warping it *before* hiding, as an earlier
            // design did, visibly dashed the cursor to the screen
            // center on every crossing.
            engine.show_local_cursor(false);
        }
        Action::SwitchToLocal { x, y } => {
            if let Some(old) = active.lock().unwrap().take() {
                enqueue(clients, old, Message::Leave { screen_id: old });
            }
            log_debug!("cursor back to local screen at ({x},{y})");
            // Control is home: local input belongs to the local
            // desktop again. Release the kernel isolation first so
            // the warp below reaches the desktop.
            engine.isolate_input(false);
            engine.grab_input(false);
            engine.warp_local(x, y);
            engine.show_local_cursor(true);
        }
    }
    Ok(())
}