package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"testing"

	"kvmshare/gui/internal/selfupdate"
)

// fakeRoleBin is a tiny test binary that mirrors the real kvmshare
// binaries' role-lock contract (see testdata/fakerole), built once by
// TestMain. It lets the lifecycle tests exercise background discovery,
// adopt-on-start and stop-by-pid without a Rust build.
var fakeRoleBin string

func TestMain(m *testing.M) {
	dir, err := os.MkdirTemp("", "kvmshare-fakerole-*")
	if err != nil {
		fmt.Fprintln(os.Stderr, "fakerole temp dir:", err)
		os.Exit(1)
	}
	defer os.RemoveAll(dir)
	// On Windows `go build -o name` appends .exe to an extensionless
	// output, so the path the harness later spawns must match the name
	// the toolchain actually produces.
	fakeRoleBin = filepath.Join(dir, "fakerole")
	if runtime.GOOS == "windows" {
		fakeRoleBin += ".exe"
	}
	build := exec.Command("go", "build", "-o", fakeRoleBin, "./testdata/fakerole")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		fmt.Fprintln(os.Stderr, "build fakerole:", err)
		os.Exit(1)
	}
	// Manifest next to the fake binaries: the GUI verifies a role
	// binary against its install-dir manifest before spawning (see
	// process.go verifyBinary), so the harness must present a
	// consistent "install" — which also exercises verification in
	// every lifecycle test.
	if err := selfupdate.WriteManifest(dir, []string{fakeRoleBin}); err != nil {
		fmt.Fprintln(os.Stderr, "manifest fakerole:", err)
		os.Exit(1)
	}
	os.Exit(m.Run())
}

// newTestApp points HOME and the config at a temp dir and uses the fake
// role binary as the "server" and "client" executables. Each test gets
// its own state dir, so role locks never leak between tests.
func newTestApp(t *testing.T) (*App, string) {
	t.Helper()
	home := t.TempDir()
	configPath := filepath.Join(home, "kvmshare-server.toml")

	t.Setenv("HOME", home)
	t.Setenv("KVMSHARE_CONFIG", configPath)
	t.Setenv("KVMSHARE_SERVER", fakeRoleBin)
	t.Setenv("KVMSHARE_CLIENT", fakeRoleBin)

	a := NewApp()
	// Never leave a role process running past the test.
	t.Cleanup(func() {
		a.mu.Lock()
		defer a.mu.Unlock()
		a.stopRoleLocked(roleServer)
		a.stopRoleLocked(roleClient)
	})
	return a, configPath
}
