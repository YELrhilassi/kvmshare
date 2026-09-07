package main

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"

	"kvmshare/gui/internal/selfupdate"
)

// TestLiveInstallPipeline runs the real update pipeline — fetch the
// latest GitHub release, download the platform archive, verify its
// checksum, extract it, and ReplaceSet over the real install paths —
// exactly as ApplyUpdate does, minus the process restart (covered by
// TestRestartHandoff). Guarded by KVMSHARE_LIVE_UPDATE so it never runs
// in normal test runs. Run with:
//
//	KVMSHARE_LIVE_UPDATE=1 go test -run TestLiveInstallPipeline -v .
func TestLiveInstallPipeline(t *testing.T) {
	if os.Getenv("KVMSHARE_LIVE_UPDATE") == "" {
		t.Skip("live update test — set KVMSHARE_LIVE_UPDATE=1 to run")
	}
	a := NewApp()
	rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
	if err != nil {
		t.Fatalf("fetch release: %v", err)
	}
	t.Logf("latest release: %s (current build: %s)", rel.Tag, selfupdate.Version)

	dir := t.TempDir()
	asset, err := rel.AssetFor()
	if err != nil {
		t.Fatalf("asset for platform: %v", err)
	}
	archive := filepath.Join(dir, asset.Name)
	if err := selfupdate.Download(asset.URL, archive); err != nil {
		t.Fatalf("download: %v", err)
	}
	sums, err := selfupdate.FetchChecksums(rel)
	if err != nil {
		t.Fatalf("checksums: %v", err)
	}
	expected, ok := sums[asset.Name]
	if !ok {
		t.Fatalf("no checksum for %s", asset.Name)
	}
	if err := selfupdate.VerifyFile(archive, expected); err != nil {
		t.Fatalf("verify: %v", err)
	}
	extracted, err := selfupdate.Extract(archive, dir)
	if err != nil {
		t.Fatalf("extract: %v", err)
	}
	t.Logf("extracted binaries: %v", extracted)

	// The real GUI path (the App does not track its own binary; resolve
	// it the same way NewApp resolves the role binaries).
	guiPath := lookPathElse("kvmshare-gui", filepath.Join(filepath.Dir(a.serverPath), binName("kvmshare-gui", runtime.GOOS)))
	if _, err := os.Stat(guiPath); err != nil {
		t.Fatalf("cannot resolve the installed GUI at %s: %v", guiPath, err)
	}
	replacements := map[string]string{
		guiPath:         extracted["kvmshare-gui"],
		a.serverPath:    extracted["kvmshare-server"],
		a.clientPath:    extracted["kvmshare-client"],
		a.installPath:   extracted["kvmshare-install"],
	}
	if err := selfupdate.ReplaceSet(replacements); err != nil {
		t.Fatalf("replace: %v", err)
	}
	t.Logf("replaced gui/server/client/install with %s binaries", rel.Tag)
}