//! The shared clipboard contract, used by **both** roles.
//!
//! Reading or writing the system clipboard can block indefinitely while
//! another process holds it open (some apps open it and never close it;
//! an X11 selection owner can be slow to answer a conversion request).
//! A clipboard call must therefore **never share a lock with the cursor
//! or the input path** — on the client the clipboard is split from the
//! [`Injector`](crate::client::Injector), and on the server it is split
//! from the [`Engine`](crate::server::Engine). Both roles give the
//! clipboard its own lock, serviced by its own thread, so a stalled
//! clipboard read delays only clipboard sync — never a cursor
//! placement, never a motion forward.

/// Platform clipboard access behind its own lock.
///
/// The same trait serves the client (injecting clipboard the server
/// sent, reading local changes to push up) and the server (applying
/// clipboard a client copied, polling local changes to broadcast).
pub trait Clipboard: Send {
    /// Put `data` into the local clipboard (received from a peer).
    fn set(&mut self, mime: &str, data: &[u8]);
    /// Read the current local clipboard, if any.
    fn get(&mut self) -> Option<(String, Vec<u8>)>;
    /// The last clipboard content applied from a *remote* source (set via
    /// [`Clipboard::set`]). Pollers compare against this so content that
    /// arrived from a peer is never echoed back to it.
    fn last_injected(&mut self) -> Option<(String, Vec<u8>)>;
}
