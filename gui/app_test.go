package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// newTestApp points HOME and the config at a temp dir and uses a harmless
// binary (/bin/sleep) as the "server" and "client" executables.
func newTestApp(t *testing.T) (*App, string) {
	t.Helper()
	home := t.TempDir()
	configPath := filepath.Join(home, "kvmshare-server.toml")

	t.Setenv("HOME", home)
	t.Setenv("KVMSHARE_CONFIG", configPath)
	t.Setenv("KVMSHARE_SERVER", "/bin/sleep")
	t.Setenv("KVMSHARE_CLIENT", "/bin/sleep")

	return NewApp(), configPath
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
