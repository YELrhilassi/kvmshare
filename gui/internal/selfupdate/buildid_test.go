package selfupdate

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// fakeBinary writes a shell script that prints `banner` for --version,
// standing in for a real role binary (crates/app/src/args.rs prints the
// same shape). Windows CI would need .bat stubs; the suite skips there.
func fakeBinary(t *testing.T, dir, name, banner string) {
	t.Helper()
	if runtime.GOOS == "windows" {
		t.Skip("shell-stub binaries need a POSIX shell")
	}
	p := filepath.Join(dir, name)
	script := "#!/bin/sh\ncat <<'EOF'\n" + banner + "EOF\n"
	if err := os.WriteFile(p, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
}

const bannerA = "kvmshare-server 0.7.3 (build 9f3a000000000001)\n"
const bannerB = "kvmshare-server 0.7.3 (build 9f3a000000000002)\n"

// The GUI answers --version with the same banner shape the Rust role
// binaries print — the install check asks every binary in the set,
// GUI included, and parses all of them with one regex.
func TestVersionBanner(t *testing.T) {
	orig := BuildID
	t.Cleanup(func() { BuildID = orig })

	BuildID = "ab12cd34ef567890"
	got := VersionBanner("kvmshare-gui")
	want := "kvmshare-gui v0.0.0-dev (build ab12cd34ef567890)"
	if got != want {
		t.Fatalf("banner = %q, want %q", got, want)
	}
	if id, ok := parseBuildID(got); !ok || id != "ab12cd34ef567890" {
		t.Fatalf("own banner must parse: %q", got)
	}

	BuildID = "" // an unstamped (bare go build) GUI says so honestly
	if got := VersionBanner("kvmshare-gui"); !strings.Contains(got, "(build unknown)") {
		t.Fatalf("unstamped banner = %q, want build unknown", got)
	}
}

func TestParseBuildID(t *testing.T) {
	if id, ok := parseBuildID(bannerA); !ok || id != "9f3a000000000001" {
		t.Fatalf("parseBuildID(%q) = %q, %v", bannerA, id, ok)
	}
	// An older binary's unknown-argument error must not parse as an id.
	if _, ok := parseBuildID("error: unknown argument \"--version\""); ok {
		t.Fatal("an unparsable banner must not yield an id")
	}
}

func TestBuildsConsistent(t *testing.T) {
	dir := t.TempDir()

	mk := func(name, b string) {
		fakeBinary(t, dir, name, strings.Replace(b, "kvmshare-server", strings.TrimSuffix(name, binSuffix()), 1))
	}
	mk(binWithSuffix("kvmshare-server"), bannerA)
	mk(binWithSuffix("kvmshare-client"), bannerA)
	mk(binWithSuffix("kvmshare-gui"), bannerA)

	if !BuildsConsistent(dir) {
		t.Fatal("three binaries with the same build id must be consistent")
	}

	// One binary from another build: inconsistent.
	mk(binWithSuffix("kvmshare-client"), bannerB)
	if BuildsConsistent(dir) {
		t.Fatal("a mixed set must not be consistent")
	}

	// An unanswerable binary (old build, no banner): inconsistent.
	fakeBinary(t, dir, binWithSuffix("kvmshare-gui"), "error: unknown argument \"--version\"")
	if BuildsConsistent(dir) {
		t.Fatal("a binary that cannot vouch for itself must block consistency")
	}
}

func TestRepairManifest(t *testing.T) {
	dir := t.TempDir()
	fakeBinary(t, dir, binWithSuffix("kvmshare-server"), bannerA)
	fakeBinary(t, dir, binWithSuffix("kvmshare-client"), bannerA)
	fakeBinary(t, dir, binWithSuffix("kvmshare-gui"), bannerA)

	if err := RepairManifest(dir); err != nil {
		t.Fatal(err)
	}
	if err := VerifyBinaries(dir); err != nil {
		t.Fatalf("manifest written from the binaries must verify: %v", err)
	}
}

func binWithSuffix(base string) string {
	if runtime.GOOS == "windows" {
		return base + ".exe"
	}
	return base
}

func binSuffix() string {
	if runtime.GOOS == "windows" {
		return ".exe"
	}
	return ""
}
