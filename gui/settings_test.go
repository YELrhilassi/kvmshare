package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

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

// Pairing defaults to ON so "connect here" works out of the box, and a
// fresh App (no gui.json yet) has it set.
func TestAcceptPairingDefaultsOn(t *testing.T) {
	a, _ := newTestApp(t)
	if !a.GetSettings().AcceptPairing {
		t.Fatal("acceptPairing should default to on (trust on first use)")
	}
}

func TestLogSettingsControlFiles(t *testing.T) {
	a, _ := newTestApp(t)

	// Defaults: info + enabled, for the active role.
	s := a.GetLogSettings()
	if s.Role != string(ModeServer) || s.Level != "info" || !s.Enabled {
		t.Fatalf("default log settings = %+v", s)
	}

	// SetLogSettings hot-writes BOTH control files (whichever role runs
	// next picks the level up; the running one hot-applies it).
	if err := a.SetLogSettings(LogSettings{Role: "server", Level: "debug", Enabled: false}); err != nil {
		t.Fatal(err)
	}
	for _, role := range []string{roleServer, roleClient} {
		raw, err := os.ReadFile(filepath.Join(a.stateDir, role+".logctl"))
		if err != nil {
			t.Fatalf("control file %s: %v", role, err)
		}
		text := string(raw)
		if !strings.Contains(text, "level=debug") || !strings.Contains(text, "enabled=0") {
			t.Fatalf("control file %s content = %q", role, text)
		}
	}

	// The choice persists across GUI restarts.
	a2 := NewApp()
	if got := a2.GetLogSettings(); got.Level != "debug" || got.Enabled {
		t.Fatalf("persisted log settings = %+v", got)
	}

	// Unknown levels are rejected.
	if err := a.SetLogSettings(LogSettings{Role: "server", Level: "loud", Enabled: true}); err == nil {
		t.Fatal("expected error for invalid level")
	}

	// ClearLog empties a real file and tolerates a missing one.
	if err := os.WriteFile(a.serverLogPath, []byte("hello\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := a.ClearLog(roleServer); err != nil {
		t.Fatal(err)
	}
	st, err := os.Stat(a.serverLogPath)
	if err != nil || st.Size() != 0 {
		t.Fatalf("log not cleared: size=%v err=%v", st, err)
	}
	if err := a.ClearLog(roleClient); err != nil {
		t.Fatalf("clearing a missing log should be a no-op: %v", err)
	}
	if err := a.ClearLog("banana"); err == nil {
		t.Fatal("expected error for unknown role")
	}
}

// Role switching must work while discovery is running: SetSettings
// re-publishes the network advertisement on a mode change, and the
// advertisement reads the settings under the same mutex. Holding a.mu
// across that re-publish used to deadlock the role picker on both
// machines (clicking a role did nothing).
func TestSetSettingsWithDiscoveryDoesNotDeadlock(t *testing.T) {
	a, _ := newTestApp(t)
	a.disc.Start() // discovery goroutines now call displayName()
	defer a.disc.Close()

	done := make(chan error, 1)
	go func() {
		s := a.GetSettings()
		s.Mode = ModeClient
		done <- a.SetSettings(s)
	}()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("SetSettings with discovery running: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("SetSettings deadlocked with discovery running")
	}
	if a.GetSettings().Mode != ModeClient {
		t.Fatal("mode should have switched to client")
	}
}
