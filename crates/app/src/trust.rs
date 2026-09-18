//! Trust policy helpers shared by the role binaries.
//!
//! The GUI owns the operator's trust/revoke decisions (settings for the
//! client, `[network]` for the server). The client process needs one of
//! them — the revoked-servers list — to refuse a session the GUI never
//! screened (a hand-typed address, a reconnect), because the server's
//! machine id only becomes known from its `Welcome`, after the connection
//! is already up.
//!
//! Parsing lives here rather than in the binary so it is unit-testable,
//! and it builds a [`Policy`] so the matching rules (prefix, 4-char
//! minimum, full or short ids) are *exactly* the server side's — one
//! implementation, no drift between what a server refuses and what a
//! client refuses.

use kvmshare_core::server::Policy;

/// Environment variable carrying the machine ids this process must
/// refuse, as a comma-separated list. The GUI sets it (from its
/// revoked-servers list) when it spawns the client.
pub const REVOKED_ENV: &str = "KVMSHARE_REVOKED_IDS";

/// Parse a comma-separated revoked-ids list (as carried in
/// [`REVOKED_ENV`]) into a [`Policy`] whose `revoked_ids` is the only
/// field set. Blank entries and surrounding whitespace are ignored, so an
/// unset or empty variable yields an empty list — no revocation, which is
/// the correct default for a standalone client run.
pub fn policy_from_revoked_list(raw: &str) -> Policy {
    let ids = raw
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Policy { revoked_ids: ids, ..Policy::default() }
}

/// [`policy_from_revoked_list`] applied to the process environment.
pub fn revoked_policy_from_env() -> Policy {
    policy_from_revoked_list(&std::env::var(REVOKED_ENV).unwrap_or_default())
}

/// File in the state dir carrying the same list, used when the spawn
/// could not carry an environment variable: the GUI's scheduled-task
/// spawn on Windows (an elevated role process has no custom env) writes
/// `revoked-ids.txt` beside the locks instead. One list, two channels —
/// the env wins when both exist (the direct-spawn channel is written
/// fresh at every spawn and can never go stale).
pub const REVOKED_FILE: &str = "revoked-ids.txt";

/// The effective revoked-servers policy for this process: the env list
/// when set, otherwise the state-dir file, otherwise empty.
pub fn revoked_policy(state_dir: &std::path::Path) -> Policy {
    if let Ok(raw) = std::env::var(REVOKED_ENV) {
        if !raw.trim().is_empty() {
            return policy_from_revoked_list(&raw);
        }
    }
    match std::fs::read_to_string(state_dir.join(REVOKED_FILE)) {
        Ok(raw) => policy_from_revoked_list(&raw),
        Err(_) => Policy::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_unset_list_revokes_nothing() {
        for raw in ["", "   ", ",", " , "] {
            let p = policy_from_revoked_list(raw);
            assert!(p.revoked_ids.is_empty(), "{raw:?} should revoke nothing");
            assert!(!p.is_revoked("70b97d38631dda4b8f6ef627d753022d"));
        }
    }

    #[test]
    fn list_is_trimmed_and_prefix_matched() {
        let p = policy_from_revoked_list(" 70b97d38 , aabbccdd11223344 ,, ");
        assert_eq!(p.revoked_ids, vec!["70b97d38".to_string(), "aabbccdd11223344".to_string()]);
        // Short form refuses the full id (the same machine).
        assert!(p.is_revoked("70b97d38631dda4b8f6ef627d753022d"));
        // A different machine is untouched.
        assert!(!p.is_revoked("98980a4d000000000000000000000000"));
    }

    #[test]
    fn state_dir_file_is_used_when_env_is_empty() {
        let dir = std::env::temp_dir().join(format!("kvmtrust-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join(REVOKED_FILE), "70b97d38\n").unwrap();
        // Env unset (or empty) → the file decides.
        std::env::remove_var(REVOKED_ENV);
        let p = revoked_policy(&dir);
        assert!(p.is_revoked("70b97d38631dda4b8f6ef627d753022d"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
