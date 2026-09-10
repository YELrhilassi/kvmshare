package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestConfigRoundTrip(t *testing.T) {
	a, configPath := newTestApp(t)

	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	// The default describes this machine only — no invented client
	// screens (clients are admitted when they connect).
	if len(cfg.Screens) != 1 {
		t.Fatalf("default config screens = %d, want 1", len(cfg.Screens))
	}

	cfg.Screens[0].Name = "pc"
	cfg.Screens = append(cfg.Screens, Screen{Name: "hp", Width: 1920, Height: 1080, X: -1920, Y: 0})
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

// The frontend iterates network.trustedIds — it must never receive
// `null`. A config without a [network] section (older files) loads with
// an empty, non-nil slice.
func TestConfigNetworkNeverNil(t *testing.T) {
	a, configPath := newTestApp(t)

	// A legacy config: screens only, no [network] section at all.
	legacy := "port = 24800\n\n[[screens]]\nname = 'pc'\nwidth = 1920\nheight = 1080\nx = 0\ny = 0\n"
	if err := os.WriteFile(configPath, []byte(legacy), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Network.TrustedIDs == nil {
		t.Fatal("trustedIds must never be nil (the frontend iterates it)")
	}
	if len(cfg.Network.TrustedIDs) != 0 {
		t.Fatalf("trustedIds should be empty, got %v", cfg.Network.TrustedIDs)
	}
	// The legacy default is secure: allowlist + local-only on.
	if !cfg.Network.Allowlist || !cfg.Network.LocalOnly {
		t.Fatalf("legacy config should default to allowlist+local-only, got %+v", cfg.Network)
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
