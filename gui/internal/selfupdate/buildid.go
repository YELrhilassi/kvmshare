package selfupdate

// Build-id verification, the second half of the install check.
//
// The sha256 manifest pins the *file set* to each other, but it is
// sidecar state: a writer that replaces a binary without rewriting the
// manifest (a manual deploy, a partial copy, an interrupted update)
// leaves a stale manifest that then bricks a perfectly consistent
// install with "does not match the install manifest". That failure
// mode punished exactly the people who install correctly.
//
// The binaries themselves now carry the fact: every Rust role binary
// stamps a 64-bit **build id** (see crates/app/build.rs) and prints it
// in `--version` (`kvmshare-server 0.7.3 (build 9f3a…)`). Two binaries
// from one build report the same id, so the *truth* about "were these
// installed together" is readable from the binaries — the manifest is
// only a cache of that truth.
//
// The GUI's check therefore becomes: verify against the manifest; on
// mismatch, ask the binaries. A consistent set repairs the stale
// manifest and launches; a genuinely mixed set still fails closed with
// the message that names the offending binary.

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"regexp"
	"runtime"
	"time"
)

// versionTimeout bounds one `--version` invocation: the role binaries
// answer immediately; anything slower means a wedged binary, which is
// itself an answer (inconsistent — do not launch).
const versionTimeout = 5 * time.Second

// versionRe matches the `--version` banner: `<bin> <ver> (build <id>)`.
// The build id is 16 lowercase hex digits (FNV-1a 64, zero-padded).
var versionRe = regexp.MustCompile(`\(build ([0-9a-f]{16})\)`)

// VersionBanner is the one-line identity every kvmshare binary prints
// for --version: `<name> <version> (build <id>)`. The GUI carries its
// id as a linker-stamped variable; the Rust binaries stamp theirs in
// build.rs. Both stamp from the same workspace fingerprint, so one
// release reports one id across the whole binary set — the fact the
// install check verifies.
func VersionBanner(name string) string {
	id := BuildID
	if id == "" {
		id = "unknown"
	}
	return fmt.Sprintf("%s %s (build %s)", name, Version, id)
}

// manifestBinaries lists the files a complete install contains, in the
// manifest's platform naming. The wheel daemon is optional in the
// check (older installs predate it) but recorded when present.
func manifestBinaries() []string {
	suffix := ""
	if runtime.GOOS == "windows" {
		suffix = ".exe"
	}
	return []string{
		"kvmshare-server" + suffix,
		"kvmshare-client" + suffix,
		"kvmshare-gui" + suffix,
		"kvmshare-wheel-daemon" + suffix,
	}
}

// parseBuildID extracts the build id from a `--version` banner.
// Exported for tests; returns ok=false for anything unparsable,
// including the "unknown argument" error an older binary prints — an
// old binary cannot vouch for itself, so it must never self-heal a
// manifest.
func parseBuildID(out string) (string, bool) {
	m := versionRe.FindStringSubmatch(out)
	if m == nil {
		return "", false
	}
	return m[1], true
}

// buildID runs `<dir>/<name> --version` and returns its build id. An
// error means the binary could not answer (missing, old, not
// executable, timed out).
func buildID(dir, name string) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), versionTimeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, dir+"/"+name, "--version")
	var stdout bytes.Buffer
	cmd.Stdout = &stdout
	err := cmd.Run()
	if err != nil {
		// The --version banner travels on stdout; a process that wrote
		// its banner and then failed (a stale dir without exec bits, a
		// dying host) still answered. Prefer the banner over the exit
		// status — but only a real banner, never an error line.
		if stdout.Len() > 0 {
			if id, ok := parseBuildID(stdout.String()); ok {
				return id, nil
			}
		}
		return "", err
	}
	id, ok := parseBuildID(stdout.String())
	if !ok {
		return "", errUnparsableVersion
	}
	return id, nil
}

// errUnparsableVersion marks output the version parser did not accept.
var errUnparsableVersion = errorString("binary did not print a build id")

// errorString is a tiny constant error type (errors.New equivalent) so
// the sentinel above stays a package-level var.
type errorString string

func (e errorString) Error() string { return string(e) }

// BuildsConsistent reports whether the binaries in `dir` were built
// together. The Rust roles stamp one workspace fingerprint in build.rs,
// the Go GUI stamps the same fingerprint at link time — so a real
// release reports **one id for the whole set**. An id of "unknown"
// (a binary built without the stamp) cannot vouch for kinship, and a
// binary that cannot answer at all — ancient, corrupt, not executable
// — fails the check the same way. Used on manifest mismatch to tell a
// stale manifest (repairable) from a genuinely mixed install (an error
// the user must resolve).
func BuildsConsistent(dir string) bool {
	ids := make(map[string]string)
	for _, name := range manifestBinaries() {
		if _, err := os.Stat(dir + "/" + name); err != nil {
			continue // optional or missing; missing *required* files are the manifest's own error
		}
		id, err := buildID(dir, name)
		if err != nil {
			return false // a present binary that cannot answer → not consistent
		}
		if id == "unknown" {
			return false // present but unstamped: no kinship claim possible
		}
		ids[name] = id
	}
	if len(ids) < 2 {
		return false // fewer than two answers cannot demonstrate kinship
	}
	first := ""
	for _, id := range ids {
		if first == "" {
			first = id
		} else if id != first {
			return false
		}
	}
	return true
}

// RepairManifest rewrites the manifest in `dir` from the binaries that
// are actually there. Only meaningful after BuildsConsistent(dir).
func RepairManifest(dir string) error {
	var bins []string
	for _, name := range manifestBinaries() {
		if _, err := os.Stat(dir + "/" + name); err == nil {
			bins = append(bins, dir+"/"+name)
		}
	}
	if len(bins) == 0 {
		return errorString("no binaries to manifest")
	}
	return WriteManifest(dir, bins)
}
