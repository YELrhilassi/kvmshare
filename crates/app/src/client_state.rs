//! The client's live state file.
//!
//! The client process writes its connection state to
//! `<state_dir>/client.state` on every transition (connecting →
//! connected → disconnected), so the GUI can show what the client is
//! doing *right now* — a running process is not the same as a live
//! connection. The GUI reads this file; the format is deliberately
//! trivial (key=value lines) and written atomically (tmp + rename) so a
//! crash never leaves a torn file.

use std::fs;
use std::path::Path;

/// Write the client's current connection state. `status` is one of
/// "connected", "connecting", "disconnected"; `server` is the address it
/// is (or was) talking to. Never fails the caller: a state file is
/// best-effort observability, not a critical path.
pub fn write_client_state(state_dir: &Path, status: &str, server: &str) {
    let dir = state_dir.to_path_buf();
    let file = dir.join("client.state");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = file.with_extension("state.tmp");
    let body = format!("status={status}\nserver={server}\n");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &file);
    }
}