// Package ids holds the machine-identity helpers shared by the GUI and
// the discovery engine: the short human-facing form of a machine id,
// and the trust matching that both sides of pairing use.
//
// The Rust binaries implement the same contracts (crates/app/src/
// machine_id.rs and the server's trusted-ids policy), so the rules here
// — prefix matching, the 4-char minimum — must stay in sync with them.
package ids

import "strings"

// maxShortLen is how many characters of a machine id are shown to
// humans and accepted in trust entries.
const maxShortLen = 8

// minEntryLen is the shortest trusted entry that is honored. Anything
// shorter would match too broadly (a typo could trust everything).
const minEntryLen = 4

// ShortID returns the human-facing form of a machine id: its first
// maxShortLen characters. Trusted-id entries may use this short form
// (prefix match), so users never type 32 hex chars.
func ShortID(id string) string {
	if len(id) > maxShortLen {
		return id[:maxShortLen]
	}
	return id
}

// Trusted reports whether id matches an entry in trusted: either equal
// or a prefix of it (full ids and short ids both work, in both
// directions). Entries shorter than minEntryLen are ignored.
func Trusted(trusted []string, id string) bool {
	for _, t := range trusted {
		t = strings.TrimSpace(t)
		if len(t) < minEntryLen {
			continue
		}
		if id == t || strings.HasPrefix(id, t) || strings.HasPrefix(t, id) {
			return true
		}
	}
	return false
}
