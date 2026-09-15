//! Machine-id list manipulation for the config's `[network]` section.
//!
//! Id lists (`trusted_ids`, `revoked_ids`) match by prefix — an entry may
//! be a full 32-char id or its 8-char short form — so *removal* must drop
//! every form of an id, not just its exact string. One module owns that
//! rule so the auto-trust writer and any future list editor cannot drift.

/// Add `id` to `list` (when `on`) or remove every form of it from `list`
/// (when off). Matching follows the server's rule: either string may be a
/// prefix of the other (short form ↔ full id). Always returns a fresh
/// slice, so a caller holding a borrow is never aliased.
pub fn set_id(list: &[String], id: &str, on: bool) -> Vec<String> {
    let kept: Vec<String> = list
        .iter()
        .filter(|t| !(matches(t, id) || matches(id, t)))
        .cloned()
        .collect();
    if on {
        let mut kept = kept;
        kept.push(id.to_owned());
        kept
    } else {
        kept
    }
}

/// Does `id` match a list `entry`? Delegates to the server's matcher so
/// the policy and every list editor share one definition.
fn matches(a: &str, b: &str) -> bool {
    kvmshare_core::server::id_matches(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn add_is_idempotent_across_forms() {
        let list = v(&["70b97d38631dda4b8f6ef627d753022d"]);
        let again = set_id(&list, "70b97d38", true);
        assert_eq!(again.len(), 1, "adding the short form of an existing entry must not duplicate it");
    }

    #[test]
    fn remove_drops_every_form() {
        let list = v(&["70b97d38", "aaaaaaaa11111111"]);
        let gone = set_id(&list, "70b97d38631dda4b8f6ef627d753022d", false);
        assert_eq!(gone, v(&["aaaaaaaa11111111"]), "removing by full id must drop the short entry");
    }

    #[test]
    fn unrelated_entries_are_untouched() {
        let list = v(&["ab", "aaaaaaaa11111111"]);
        // A 2-char entry can never match anything (the matcher's guard)
        // but set_id is surgical: it only touches entries matching the
        // id it was given, never unrelated list content.
        let out = set_id(&list, "bbbbbbbb22222222", true);
        assert_eq!(out, v(&["ab", "aaaaaaaa11111111", "bbbbbbbb22222222"]));
    }
}
