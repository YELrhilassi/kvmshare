package main

import (
	"os"
	"path/filepath"
	"testing"

	"kvmshare/gui/internal/selfupdate"
)

// The exact field failure: binaries replaced by a deploy without
// rewriting the manifest. The GUI must detect a consistent set (same
// build id), repair the manifest, and allow the launch.
func TestStaleManifestSelfHeals(t *testing.T) {
	dir := t.TempDir()
	// Three fake binaries reporting one build id.
	banner := "#!/bin/sh\necho 'kvmshare-x 0.7.3 (build aaaaaaaaaaaaaaa1)'\n"
	for _, n := range []string{"kvmshare-server", "kvmshare-client", "kvmshare-gui"} {
		if err := os.WriteFile(filepath.Join(dir, n), []byte(banner), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	// A manifest from an *older* install (stale hashes).
	stale := "0000000000000000000000000000000000000000000000000000000000000000  kvmshare-server\n0000000000000000000000000000000000000000000000000000000000000000  kvmshare-client\n0000000000000000000000000000000000000000000000000000000000000000  kvmshare-gui\n"
	if err := os.WriteFile(filepath.Join(dir, selfupdate.ManifestName), []byte(stale), 0o644); err != nil {
		t.Fatal(err)
	}
	// The GUI's own check, exercised through the App method.
	a := &App{}
	if err := a.verifyBinary(filepath.Join(dir, "kvmshare-server")); err != nil {
		t.Fatalf("consistent binaries behind a stale manifest must self-heal, got: %v", err)
	}
	// The manifest was rewritten to match reality.
	if err := selfupdate.VerifyBinaries(dir); err != nil {
		t.Fatalf("manifest was not repaired: %v", err)
	}
}

func TestMixedBuildStillRefused(t *testing.T) {
	dir := t.TempDir()
	b1 := "#!/bin/sh\necho 'kvmshare-x 0.7.3 (build aaaaaaaaaaaaaaa1)'\n"
	b2 := "#!/bin/sh\necho 'kvmshare-x 0.7.3 (build aaaaaaaaaaaaaaa2)'\n"
	if err := os.WriteFile(filepath.Join(dir, "kvmshare-server"), []byte(b1), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "kvmshare-client"), []byte(b2), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "kvmshare-gui"), []byte(b1), 0o755); err != nil {
		t.Fatal(err)
	}
	a := &App{}
	if err := a.verifyBinary(filepath.Join(dir, "kvmshare-server")); err == nil {
		t.Fatal("a mixed build set must still be refused")
	}
}
