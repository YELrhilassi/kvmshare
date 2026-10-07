//! The server's connection policy: who may connect at all.
//!
//! Loaded from the `[network]` config section and shared with every
//! accept path behind one lock, so a hot policy change applies to the
//! very next handshake instead of the next restart. The rules are
//! ordered where it matters: revocation is checked before everything
//! else and cannot be bypassed by a pinned layout screen.

/// The server's connection policy, loaded from the `[network]` config
/// section. Decides which peers may connect at all.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Only accept clients whose **exact** screen name appears in the
    /// layout, plus trusted machine ids. When false, any name is
    /// admitted dynamically (the legacy plug-and-play behavior).
    pub allowlist: bool,
    /// Only accept connections from the local network (RFC1918 private
    /// ranges, loopback and link-local). Blocks WAN/bridged peers.
    pub local_only: bool,
    /// Machine ids that may connect even when their name is not in the
    /// layout (they are admitted dynamically, like a fresh client).
    pub trusted_ids: Vec<String>,
    /// Machine ids that may **never** connect. Checked before everything
    /// else, including the layout and `trusted_ids`: revoking a machine is
    /// a hard deny, so it is the one policy that cannot be bypassed by a
    /// pinned layout screen. Both lists may hold the same id; revoke wins.
    pub revoked_ids: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self { allowlist: true, local_only: true, trusted_ids: Vec::new(), revoked_ids: Vec::new() }
    }
}

impl Policy {
    /// Is `machine_id` explicitly revoked? An entry may be the full id or
    /// its 8-char short form, so revoking the short id shown in the GUI
    /// also refuses the full one. Revocation is the strongest rule:
    /// callers check this before anything else.
    pub fn is_revoked(&self, machine_id: &str) -> bool {
        self.revoked_ids.iter().any(|r| id_matches(machine_id, r))
    }

    /// Is `machine_id` trusted (admitted even without a layout screen)?
    pub fn is_trusted(&self, machine_id: &str) -> bool {
        self.trusted_ids.iter().any(|t| id_matches(machine_id, t))
    }
}

/// Does a machine id match an id-list entry? The **entry** may be the
/// full id or a prefix of it (the 8-char short form the GUI shows).
/// Guards: empty entries never match; a short form must be at least 4
/// chars so a typo'd one-char "trust" cannot silently admit everything
/// starting with it.
pub fn id_matches(id: &str, entry: &str) -> bool {
    if entry.is_empty() || entry.len() < 4 {
        return false;
    }
    id == entry || id.starts_with(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_may_be_the_full_id_or_its_short_form() {
        let full = "98980a4d9afac273a9aac53ec1c57c35";
        assert!(id_matches(full, "98980a4d"), "short entry matches the full id");
        assert!(id_matches(full, full));
        assert!(!id_matches(full, "98"), "too-short entries never match");
        assert!(!id_matches(full, ""));
        assert!(!id_matches("other-machine", "98980a4d"));
    }

    #[test]
    fn revocation_outranks_trust() {
        let policy = Policy {
            trusted_ids: vec!["70b97d38".into()],
            revoked_ids: vec!["70b97d38".into()],
            ..Policy::default()
        };
        assert!(policy.is_revoked("70b97d38631dda4b8f6ef627d753022d"));
        // The caller's ordering (revoke checked first) plus this pin:
        // both answers coexist, and the handshake must check revocation
        // first.
        assert!(policy.is_trusted("70b97d38631dda4b8f6ef627d753022d"));
    }
}
