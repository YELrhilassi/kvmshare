// Binary-manifest verification: every installer path writes a sha256
// manifest next to the binaries it deployed, and the GUI refuses to
// spawn a role binary that does not match it.
//
// Why: the GUI resolves binaries via sibling-or-PATH lookup, and a
// machine can hold several copies of kvmshare from different installs
// (an installer's dir, ~/.local/bin, a dev prefix). A stale copy that
// wins the lookup runs an old backend against a new GUI — the exact
// mixed-version failure that once left a client's cursor stuck on
// entry (old server + new client, entry-point math disagreed). The
// manifest turns "which binary did I get?" from guesswork into a
// verifiable fact: whatever installs the binaries also records their
// hashes, and whatever launches them checks them first.
//
// The manifest file is `binaries.sha256` in the install dir, in the
// same `sha256sum`-compatible format dist/ ships (hex hash + two spaces
// + name), so it can be checked by hand with `sha256sum -c` too.
//
// This is a **self-consistency** manifest: it pins the binaries to
// each other (the set that was installed together), not to a release
// tag. It closes the mixed-install hole; release integrity is already
// verified against the release SHA256SUMS at download time.

package selfupdate

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// ManifestName is the manifest file written next to the binaries.
const ManifestName = "binaries.sha256"

// fileHash returns the lowercase sha256 hex digest of a file.
func fileHash(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// WriteManifest hashes `bins` (paths, any order) and writes the
// manifest into `dir` (one entry per file, name → hash). Missing files
// are an error: a manifest that skips a binary would vouch for a set
// that is not actually there.
func WriteManifest(dir string, bins []string) error {
	type entry struct{ name, hash string }
	entries := make([]entry, 0, len(bins))
	for _, p := range bins {
		name := filepath.Base(p)
		sum, err := fileHash(p)
		if err != nil {
			return fmt.Errorf("hash %s: %w", name, err)
		}
		entries = append(entries, entry{name, sum})
	}
	// Deterministic file content: the same install always produces the
	// same manifest bytes.
	sort.Slice(entries, func(i, j int) bool { return entries[i].name < entries[j].name })
	var buf bytes.Buffer
	for _, e := range entries {
		fmt.Fprintf(&buf, "%s  %s\n", e.hash, e.name)
	}
	return os.WriteFile(filepath.Join(dir, ManifestName), buf.Bytes(), 0o644)
}

// ErrNoManifest: the install dir predates manifests (an old install).
// The GUI treats it as *unverified* — see VerifyBinaries.
var ErrNoManifest = errors.New("no " + ManifestName + " in the install dir (install predates manifests — reinstall)")

// VerifyBinaries checks every binary in `dir` against the manifest.
// Returns the offending binary names on mismatch so an error message
// can name the actual file to replace.
func VerifyBinaries(dir string) error {
	raw, err := os.ReadFile(filepath.Join(dir, ManifestName))
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return ErrNoManifest
		}
		return err
	}
	// Parse `hash  name` lines (sha256sum format: two spaces, or one
	// `*` for binary mode — accept both, we only need the pair).
	type entry struct{ hash, name string }
	var entries []entry
	sc := bufio.NewScanner(bytes.NewReader(raw))
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		// strings.Cut returns (before, after, found): the hash is
		// first on the line, the name after the separator.
		hash, name, ok := strings.Cut(line, "  ")
		if !ok {
			hash, name, ok = strings.Cut(line, " ")
			name = strings.TrimPrefix(name, "*")
		}
		if !ok {
			continue
		}
		entries = append(entries, entry{strings.TrimSpace(hash), strings.TrimSpace(name)})
	}
	if err := sc.Err(); err != nil {
		return err
	}
	if len(entries) == 0 {
		return fmt.Errorf("%s has no entries", ManifestName)
	}
	for _, e := range entries {
		p := filepath.Join(dir, e.name)
		sum, err := fileHash(p)
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				return fmt.Errorf("%s is missing (manifest lists it)", p)
			}
			return err
		}
		if sum != e.hash {
			return fmt.Errorf("%s does not match the install manifest (expected %s…, got %s…) — reinstall kvmshare", p, e.hash[:12], sum[:12])
		}
	}
	return nil
}
