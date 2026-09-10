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
    write_state(state_dir, status, server, false);
}

/// Write the state for a disconnect the *server* requested, adding the
/// `stopped=1` marker.
///
/// Why the extra marker: the client writes "disconnected" on every
/// transient drop too (it is about to retry), so the GUI cannot use the
/// status alone to tell "the operator told us to stop" from "the link
/// blinked". Only this exit leaves `stopped=1`, which the GUI reads to
/// hold auto-connect off instead of immediately reconnecting to the
/// server the operator just dismissed.
pub fn write_client_state_stopped(state_dir: &Path, server: &str) {
    write_state(state_dir, "disconnected", server, true);
}

fn write_state(state_dir: &Path, status: &str, server: &str, stopped: bool) {
    let dir = state_dir.to_path_buf();
    let file = dir.join("client.state");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = file.with_extension("state.tmp");
    let marker = if stopped { "stopped=1\n" } else { "" };
    let body = format!("status={status}\nserver={server}\n{marker}");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kvmshare-state-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn transient_disconnect_has_no_stopped_marker() {
        let dir = scratch_dir("transient");
        write_client_state(&dir, "disconnected", "192.168.1.72:24800");
        let body = fs::read_to_string(dir.join("client.state")).unwrap();
        assert!(body.contains("status=disconnected"));
        assert!(!body.contains("stopped=1"), "transient drops must not read as a requested stop");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn requested_disconnect_sets_stopped_marker() {
        let dir = scratch_dir("requested");
        write_client_state_stopped(&dir, "192.168.1.72:24800");
        let body = fs::read_to_string(dir.join("client.state")).unwrap();
        assert!(body.contains("status=disconnected"));
        assert!(body.contains("stopped=1"), "a server-requested stop must be distinguishable");
        let _ = fs::remove_dir_all(&dir);
    }
}