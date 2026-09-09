package selfupdate

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// seedInstall writes two fake binaries into a temp dir, returning paths.
func seedInstall(t *testing.T, dir string) []string {
	t.Helper()
	a := filepath.Join(dir, "kvmshare-server"+ext())
	b := filepath.Join(dir, "kvmshare-client"+ext())
	for _, p := range []string{a, b} {
		if err := os.WriteFile(p, []byte("binary:"+filepath.Base(p)), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	return []string{a, b}
}

func ext() string {
	if os.PathSeparator == '\\' {
		return ".exe"
	}
	return ""
}

func TestManifestRoundtripVerifies(t *testing.T) {
	dir := t.TempDir()
	bins := seedInstall(t, dir)
	if err := WriteManifest(dir, bins); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(filepath.Join(dir, ManifestName)); err != nil {
		t.Fatalf("manifest not written: %v", err)
	}
	if err := VerifyBinaries(dir); err != nil {
		t.Fatalf("fresh install must verify: %v", err)
	}
}

func TestManifestDetectsStaleBinary(t *testing.T) {
	dir := t.TempDir()
	bins := seedInstall(t, dir)
	if err := WriteManifest(dir, bins); err != nil {
		t.Fatal(err)
	}
	// Overwrite one binary after the fact: the mixed-install scenario.
	if err := os.WriteFile(bins[0], []byte("stale old build"), 0o755); err != nil {
		t.Fatal(err)
	}
	err := VerifyBinaries(dir)
	if err == nil {
		t.Fatal("stale binary must fail verification")
	}
	if !strings.Contains(err.Error(), filepath.Base(bins[0])) {
		t.Fatalf("error must name the offending file, got: %v", err)
	}
}

func TestManifestDetectsMissingFile(t *testing.T) {
	dir := t.TempDir()
	bins := seedInstall(t, dir)
	if err := WriteManifest(dir, bins); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(bins[1]); err != nil {
		t.Fatal(err)
	}
	err := VerifyBinaries(dir)
	if err == nil || !strings.Contains(err.Error(), "missing") {
		t.Fatalf("missing binary must be reported, got: %v", err)
	}
}

func TestManifestMissingIsDistinct(t *testing.T) {
	dir := t.TempDir()
	seedInstall(t, dir)
	err := VerifyBinaries(dir)
	if err == nil {
		t.Fatal("manifest-less dir must fail")
	}
	if err != ErrNoManifest {
		t.Fatalf("missing manifest must surface as ErrNoManifest, got: %v", err)
	}
}
