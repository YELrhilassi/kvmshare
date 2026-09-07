//! The server's clipboard broadcast poller.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use kvmshare_core::server::{Server, ServerClipboard};
use kvmshare_protocol::message::Message;

/// How often the server checks its local clipboard for changes.
const CLIPBOARD_POLL: Duration = Duration::from_millis(500);

/// Watch the local clipboard and broadcast changes to every client.
///
/// Content that arrived *from* a client (already applied to the local
/// clipboard by the server's per-client handler) is skipped via
/// [`Clipboard::last_injected`], so a copy on a client does not echo
/// back to all clients.
///
/// The poll runs on its own thread and touches **only** the clipboard's
/// own lock: reading the system clipboard can block for a long time (an
/// X11 selection owner that is busy or gone, another process holding
/// the clipboard open), and that must never hold the engine lock that
/// serializes every cursor motion.
pub fn spawn_server_clipboard(clipboard: ServerClipboard, server: Arc<Server>) {
    thread::spawn(move || {
        let mut last_seen: Option<(String, Vec<u8>)> = None;
        loop {
            thread::sleep(CLIPBOARD_POLL);
            // The guard is dropped at the end of each statement; a
            // blocking clipboard read delays only this thread.
            let cur = { clipboard.lock().unwrap().get() };
            let last_injected = { clipboard.lock().unwrap().last_injected() };
            if let Some(cur) = cur {
                if last_seen.as_ref() != Some(&cur) && last_injected.as_ref() != Some(&cur) {
                    let (mime, data) = cur.clone();
                    if let Err(e) = server.broadcast(&Message::Clipboard { mime, data }) {
                        kvmshare_log::log_warn!("clipboard broadcast: {e}");
                    }
                    last_seen = Some(cur);
                }
            }
        }
    });
}