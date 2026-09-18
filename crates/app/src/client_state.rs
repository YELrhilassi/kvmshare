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
    write_state(state_dir, status, server, "", false);
}

/// Write the `connected` state together with the peer's identity.
///
/// `server_id` is the machine id the server revealed in `Welcome`. The
/// GUI's network page matches the connected session to a discovered peer
/// by this id — address strings churn (DHCP, dual interfaces, a beacon
/// expiring mid-session), the id does not — so a live session stays
/// rendered as one stable row instead of flapping between states.
pub fn write_client_state_connected(state_dir: &Path, server: &str, server_id: &str) {
    write_state(state_dir, "connected", server, server_id, false);
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
    write_state(state_dir, "disconnected", server, "", true);
}

/// Write the state for a connection the server *refused* (a policy
/// answer: revoked, not allowed, outside the local network). The status
/// is `refused` and the server's explanation travels in `reason=`, so
/// the GUI can show the operator the actual problem instead of a
/// forever-"connecting…". The process exits after writing this —
/// retrying cannot change the server's answer — so the GUI reads a
/// stable terminal state, not a flapping one.
pub fn write_client_state_refused(state_dir: &Path, server: &str, reason: &str) {
    let dir = state_dir.to_path_buf();
    let file = dir.join("client.state");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }    // One line; a reason is prose and must never inject newlines into
    // the key=value file.
    let reason = reason.replace(['\n', '\r'], " ");
    let body = format!("status=refused\nserver={server}\nreason={reason}\n");
    let tmp = file.with_extension("state.tmp");
    if fs::write(&tmp, body).is_ok() {
        let _ = fs::rename(&tmp, &file);
    }
}

fn write_state(state_dir: &Path, status: &str, server: &str, server_id: &str, stopped: bool) {
    let dir = state_dir.to_path_buf();
    let file = dir.join("client.state");
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = file.with_extension("state.tmp");
    let marker = if stopped { "stopped=1\n" } else { "" };
    // The id rides only on states that know it (connected); an empty
    // value writes no line, so earlier states never shadow a real one.
    let id_line = if server_id.is_empty() {
        String::new()
    } else {
        format!("server_id={server_id}\n")
    };
    let body = format!("status={status}\nserver={server}\n{id_line}{marker}");
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

    #[test]
    fn connected_state_carries_the_server_id() {
        let dir = scratch_dir("connected-id");
        write_client_state_connected(&dir, "192.168.1.72:24800", "70b97d38631dda4b8f6ef627d753022d");
        let body = fs::read_to_string(dir.join("client.state")).unwrap();
        assert!(body.contains("status=connected"));
        assert!(body.contains("server_id=70b97d38631dda4b8f6ef627d753022d"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn transient_states_write_no_id_line() {
        let dir = scratch_dir("no-id");
        write_client_state(&dir, "connecting", "192.168.1.72:24800");
        let body = fs::read_to_string(dir.join("client.state")).unwrap();
        assert!(!body.contains("server_id="), "a state that does not know the id must not claim one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refused_state_carries_a_single_line_reason() {
        let dir = scratch_dir("refused");
        write_client_state_refused(&dir, "192.168.1.72:24800", "revoked: line one\nline two");
        let body = fs::read_to_string(dir.join("client.state")).unwrap();
        assert!(body.contains("status=refused"));
        assert!(body.contains("reason=revoked: line one line two"));
        assert_eq!(body.lines().count(), 3, "reason must stay on one line");
    }
}