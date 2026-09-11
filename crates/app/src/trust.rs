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
}
