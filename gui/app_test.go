package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
	"time"
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
	fakeRoleBin = filepath.Join(dir, "fakerole")
	build := exec.Command("go", "build", "-o", fakeRoleBin, "./testdata/fakerole")
	build.Stderr = os.Stderr
	if err := build.Run(); err != nil {
		fmt.Fprintln(os.Stderr, "build fakerole:", err)
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

func TestSettingsRoundTrip(t *testing.T) {
	a, _ := newTestApp(t)

	s, err := a.GetSettings(), error(nil)
	if err != nil {
		t.Fatal(err)
	}
	if s.Mode != ModeServer {
		t.Fatalf("default mode = %q, want server", s.Mode)
	}

	s.Mode = ModeClient
	s.ClientAddr = "10.0.0.5:24800"
	s.ClientName = "laptop"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}

	// A fresh App must read the persisted settings.
	a2 := NewApp()
	got := a2.GetSettings()
	if got.Mode != ModeClient || got.ClientAddr != "10.0.0.5:24800" || got.ClientName != "laptop" {
		t.Fatalf("persisted settings = %+v", got)
	}
}

func TestSetSettingsValidation(t *testing.T) {
	a, _ := newTestApp(t)
	if err := a.SetSettings(Settings{Mode: "banana"}); err == nil {
		t.Fatal("expected error for invalid mode")
	}
	// An empty client address is fine to store — the address is only
	// required when the client is actually started (ClientStart).
	if err := a.SetSettings(Settings{Mode: ModeClient}); err != nil {
		t.Fatalf("empty client address should be storable: %v", err)
	}
}

func TestDefaultSettingsHaveNoMachineSpecificAddress(t *testing.T) {
	a, _ := newTestApp(t)
	s := a.GetSettings()
	if s.ClientAddr != "" {
		t.Fatalf("default client address should be empty, got %q", s.ClientAddr)
	}
	if s.Mode != ModeServer {
		t.Fatalf("default mode = %q, want server", s.Mode)
	}
	if s.ClientName == "" {
		t.Fatal("default client name should fall back to the host name")
	}
}

func TestConfigRoundTrip(t *testing.T) {
	a, configPath := newTestApp(t)

	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.Screens) != 2 {
		t.Fatalf("default config screens = %d, want 2", len(cfg.Screens))
	}

	cfg.Screens[0].Name = "pc"
	cfg.Screens[1].Name = "hp"
	cfg.Screens[1].X = -1920
	if err := a.SaveConfig(cfg); err != nil {
		t.Fatal(err)
	}

	if _, err := os.Stat(configPath); err != nil {
		t.Fatalf("config file not written: %v", err)
	}

	loaded, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if loaded.Screens[0].Name != "pc" || loaded.Screens[1].X != -1920 {
		t.Fatalf("roundtrip mismatch: %+v", loaded)
	}
}

func TestSaveConfigValidation(t *testing.T) {
	a, _ := newTestApp(t)
	if err := a.SaveConfig(Config{Port: 24800, Screens: []Screen{}}); err == nil {
		t.Fatal("expected error for empty screens")
	}
	if err := a.SaveConfig(Config{Port: 24800, Screens: []Screen{{Name: "x", Width: 0, Height: 10}}}); err == nil {
		t.Fatal("expected error for invalid size")
	}
	if err := a.SaveConfig(Config{Port: 24800, Screens: []Screen{{Name: " ", Width: 100, Height: 100}}}); err == nil {
		t.Fatal("expected error for blank name")
	}
	if err := a.SaveConfig(Config{Port: 24800, Screens: []Screen{
		{Name: "pc", Width: 100, Height: 100},
		{Name: "pc", Width: 100, Height: 100},
	}}); err == nil {
		t.Fatal("expected error for duplicate name")
	}
	// A valid config with whitespace-padded names is saved trimmed.
	if err := a.SaveConfig(Config{Port: 24800, Screens: []Screen{{Name: "  hp  ", Width: 100, Height: 100}}}); err != nil {
		t.Fatal(err)
	}
	loaded, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if loaded.Screens[0].Name != "hp" {
		t.Fatalf("name not trimmed: %q", loaded.Screens[0].Name)
	}
	// No temp files may be left behind by the atomic write.
	matches, err := filepath.Glob(filepath.Join(filepath.Dir(a.configPath), ".kvmshare-*.tmp"))
	if err != nil {
		t.Fatal(err)
	}
	if len(matches) != 0 {
		t.Fatalf("stale temp files left: %v", matches)
	}
}

func TestAtomicWriteFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "kvmshare-server.toml")
	if err := atomicWriteFile(path, []byte("port = 24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(path)
	if err != nil || string(raw) != "port = 24800\n" {
		t.Fatalf("content mismatch: %q err=%v", raw, err)
	}
	// Overwriting an existing file works and stays atomic.
	if err := atomicWriteFile(path, []byte("port = 9999\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	raw, _ = os.ReadFile(path)
	if string(raw) != "port = 9999\n" {
		t.Fatalf("overwrite mismatch: %q", raw)
	}
	matches, _ := filepath.Glob(filepath.Join(filepath.Dir(path), ".kvmshare-*.tmp"))
	if len(matches) != 0 {
		t.Fatalf("stale temp files left: %v", matches)
	}
}

func TestProcessLifecycle(t *testing.T) {
	a, _ := newTestApp(t)

	started, err := a.ServerStart()
	if err != nil || !started {
		t.Fatalf("ServerStart: %v (started=%v)", err, started)
	}
	if !a.ServerRunning() {
		t.Fatal("server should be running")
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("server should be stopped")
	}

	// Second start of a stopped process works; starting twice is a no-op.
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
}

func TestClientLifecycle(t *testing.T) {
	a, _ := newTestApp(t)

	// No address configured → refused. (SetSettings itself rejects an
	// empty address in client mode, so poke the field directly.)
	a.mu.Lock()
	a.settings.ClientAddr = ""
	a.mu.Unlock()
	if _, err := a.ClientStart(); err == nil {
		t.Fatal("expected error when client address is empty")
	}

	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}

	// Configured → starts, runs, stops.
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if !a.ClientRunning() {
		t.Fatal("client should be running")
	}
	if err := a.ClientStop(); err != nil {
		t.Fatal(err)
	}
	if a.ClientRunning() {
		t.Fatal("client should be stopped")
	}
}

func TestStartActiveRespectsMode(t *testing.T) {
	a, _ := newTestApp(t)

	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	_ = a.SetSettings(s)

	if _, err := a.StartActive(); err != nil {
		t.Fatal(err)
	}
	if !a.ClientRunning() {
		t.Fatal("StartActive should have started the client in client mode")
	}
	if a.ServerRunning() {
		t.Fatal("server should not be running")
	}
	if err := a.StopActive(); err != nil {
		t.Fatal(err)
	}
	if a.ClientRunning() {
		t.Fatal("client should be stopped")
	}
}

func TestListInterfaces(t *testing.T) {
	a, _ := newTestApp(t)
	ifaces, err := a.ListInterfaces()
	if err != nil {
		t.Fatal(err)
	}
	if len(ifaces) == 0 {
		t.Fatal("expected at least one interface")
	}
	found := false
	for _, ifc := range ifaces {
		if ifc.Name == "" {
			t.Fatal("interface with empty name")
		}
		for _, addr := range ifc.Addrs {
			if addr == "" {
				t.Fatal("interface with empty address")
			}
			found = true
		}
	}
	if !found {
		t.Fatal("expected at least one address")
	}
}

func TestTailLog(t *testing.T) {
	a, _ := newTestApp(t)

	// Missing log → empty string, no error.
	stateDir := filepath.Dir(a.serverLogPath)
	out, err := a.TailLog(filepath.Join(stateDir, "nope.log"), 10)
	if err != nil || out != "" {
		t.Fatalf("missing log: out=%q err=%v", out, err)
	}

	// Real file with more lines than requested → last N only.
	logFile := filepath.Join(stateDir, "x.log")
	var content string
	for i := 0; i < 20; i++ {
		content += "line " + string(rune('A'+i)) + "\n"
	}
	if err := os.WriteFile(logFile, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	out, err = a.TailLog(logFile, 3)
	if err != nil {
		t.Fatal(err)
	}
	if out != "line R\nline S\nline T" {
		t.Fatalf("tail mismatch: %q", out)
	}

	// Path outside the state dir → refused.
	if _, err := a.TailLog("/etc/passwd", 10); err == nil {
		t.Fatal("expected refusal to read outside log dir")
	}
}

func TestStopIsFast(t *testing.T) {
	a, _ := newTestApp(t)
	_, _ = a.ServerStart()
	start := time.Now()
	_ = a.ServerStop()
	if time.Since(start) > 2*time.Second {
		t.Fatalf("ServerStop took %v", time.Since(start))
	}
}

func TestRoleSwitchStopsOldProcess(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should be running")
	}

	// Switching to client mode must stop the server: a machine runs one
	// role at a time.
	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("role switch must stop the server process")
	}
}

func TestStartingOneRoleStopsTheOther(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}

	// Saving client settings must not disturb the running server.
	s := a.GetSettings()
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should still be running after saving client settings")
	}

	// Starting the client stops the server: never both at once.
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("starting the client must stop the server")
	}
	if !a.ClientRunning() {
		t.Fatal("client should be running")
	}
}

// Closing the GUI must NOT stop the role: it keeps running in the
// background, and a fresh GUI instance sees (and can stop) it through
// the role lock.
func TestRoleSurvivesGUICloseAndIsStoppable(t *testing.T) {
	a1, _ := newTestApp(t)
	if _, err := a1.ServerStart(); err != nil {
		t.Fatal(err)
	}

	// A second GUI (e.g. after closing and reopening the window) has no
	// child of its own but must report the background server as running.
	a2 := NewApp()
	if !a2.ServerRunning() {
		t.Fatal("fresh GUI should detect the background server via the role lock")
	}

	// Starting must adopt, never spawn a second instance.
	started, err := a2.ServerStart()
	if err != nil || !started {
		t.Fatalf("ServerStart should adopt the running instance: %v (started=%v)", err, started)
	}
	if a2.serverProc != nil {
		t.Fatal("adopting a background instance must not spawn a child")
	}

	// The fresh GUI can stop the background instance by pid.
	if err := a2.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a2.ServerRunning() {
		t.Fatal("server should be stopped")
	}
	if a1.ServerRunning() {
		t.Fatal("first GUI should also see it stopped")
	}
}

// A role started before the GUI existed (e.g. launched from dmenu) is
// discovered, reported as running, and stoppable.
func TestPreexistingBackgroundInstanceIsAdopted(t *testing.T) {
	a, _ := newTestApp(t)

	// Simulate "kvmshare-server started by hand": a fresh instance that
	// is not a child of any GUI.
	hand := NewApp()
	if _, err := hand.ServerStart(); err != nil {
		t.Fatal(err)
	}
	// Drop every reference so only the flock keeps it alive, like a
	// background process started outside the GUI.
	hand.serverProc = nil

	if !a.ServerRunning() {
		t.Fatal("GUI should report the hand-started server as running")
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("server should be stopped")
	}
}

func TestSaveConfigDoesNotStartServer(t *testing.T) {
	a, configPath := newTestApp(t)
	cfg := Config{Port: defaultPort, Screens: []Screen{{Name: "pc", Width: 1920, Height: 1080, X: 0, Y: 0}}}
	if err := a.SaveConfig(cfg); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(configPath); err != nil {
		t.Fatalf("config not written: %v", err)
	}
	if a.ServerRunning() {
		t.Fatal("saving config must not start the server (the running server picks the file up itself)")
	}
}

func TestSingleInstanceLock(t *testing.T) {
	a, _ := newTestApp(t)
	if err := a.SingleInstance(); err != nil {
		t.Fatalf("first instance should get the lock: %v", err)
	}
	b := NewApp() // second instance, same HOME -> same lock file
	if err := b.SingleInstance(); err == nil {
		t.Fatal("second instance must be refused")
	}
}
