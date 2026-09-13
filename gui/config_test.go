package main

import (
	"os"
	"path/filepath"
	"reflect"
	"strings"
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
// The old GUI wrote [shortcuts]/[input] as generic maps through
// JavaScript: integral numbers became floats (`key = 71.0`) and the
// input names went camelCase. The strict schema rejected both, so
// bindings never registered. Healing must convert both, and a strict
// file must pass through byte-identical.
func TestConfigHealsLegacyFloatsAndInputNames(t *testing.T) {
	a, configPath := newTestApp(t)

	legacy := "port = 24800\n" +
		"\n[[screens]]\nname = 'pc'\nwidth = 1920\nheight = 1080\nx = 0\ny = 0\n" +
		"\n[[screens]]\nname = 'hp'\nwidth = 1920\nheight = 1080\nx = -1920\ny = 0\n" +
		"\n[shortcuts]\nenabled = true\n" +
		"[[shortcuts.bindings]]\nmods = { ctrl = true, alt = false, shift = false, meta = false }\n" +
		"key = 71.0\naction = 'switch'\nscreen = 'hp'\n" +
		"\n[input]\npointerSpeed = 1.5\nwheelSpeed = 2.0\nswapScroll = true\n"
	if err := os.WriteFile(configPath, []byte(legacy), 0o644); err != nil {
		t.Fatal(err)
	}

	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatalf("legacy file must load after healing: %v", err)
	}
	if cfg.Shortcuts == nil || len(cfg.Shortcuts.Bindings) != 1 {
		t.Fatalf("shortcuts section lost: %+v", cfg.Shortcuts)
	}
	b := cfg.Shortcuts.Bindings[0]
	if b.Key != 71 || !b.Mods.Ctrl || b.Screen != "hp" {
		t.Fatalf("binding mismatch: %+v", b)
	}
	if cfg.Input == nil || cfg.Input.PointerSpeed != 1.5 || cfg.Input.WheelSpeed != 2.0 || !cfg.Input.SwapScroll {
		t.Fatalf("input section lost: %+v", cfg.Input)
	}

	// Healing persists: the healed file must now parse strictly (this
	// time without needing the healer).
	raw, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(raw), "71.0") || strings.Contains(string(raw), "pointerSpeed") {
		t.Fatalf("healed file still contains legacy shapes:\n%s", raw)
	}
	cfg2, err := a.LoadConfig()
	if err != nil {
		t.Fatalf("healed file must parse strictly: %v", err)
	}
	if !reflect.DeepEqual(cfg.Shortcuts, cfg2.Shortcuts) {
		t.Fatalf("heal changed meaning: %+v vs %+v", cfg.Shortcuts, cfg2.Shortcuts)
	}
}

// A strict file (what the Rust server itself writes) must load with no
// healing and survive a GUI save byte-shape-compatible: integer keys,
// snake_case input names, no camelCase leakage.
func TestConfigRoundTripsShortcutsAndInput(t *testing.T) {
	a, configPath := newTestApp(t)

	strict := "port = 24800\n" +
		"\n[[screens]]\nname = 'pc'\nwidth = 1920\nheight = 1080\nx = 0\ny = 0\n" +
		"\n[shortcuts]\nenabled = true\n" +
		"[[shortcuts.bindings]]\nmods = { ctrl = false, alt = false, shift = false, meta = false }\n" +
		"key = 71\naction = 'switch'\nscreen = 'hp'\n" +
		"\n[input]\npointer_speed = 1.25\nwheel_speed = 1.0\nswap_scroll = false\n"
	if err := os.WriteFile(configPath, []byte(strict), 0o644); err != nil {
		t.Fatal(err)
	}

	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if err := a.SaveConfig(cfg); err != nil {
		t.Fatal(err)
	}

	raw, err := os.ReadFile(configPath)
	if err != nil {
		t.Fatal(err)
	}
	text := string(raw)
	if strings.Contains(text, "71.0") {
		t.Fatal("save re-floated the binding key")
	}
	if !strings.Contains(text, "key = 71") {
		t.Fatalf("binding key missing after save:\n%s", text)
	}
	if strings.Contains(text, "pointerSpeed") || !strings.Contains(text, "pointer_speed") {
		t.Fatalf("input section not snake_case after save:\n%s", text)
	}

	loaded, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if loaded.Shortcuts == nil || len(loaded.Shortcuts.Bindings) != 1 || loaded.Shortcuts.Bindings[0].Key != 71 {
		t.Fatalf("binding lost across save: %+v", loaded.Shortcuts)
	}
	if loaded.Input == nil || loaded.Input.PointerSpeed != 1.25 {
		t.Fatalf("input lost across save: %+v", loaded.Input)
	}
}

// The frontend's Settings form (and the layout test above) rely on a
// Genuinely corrupt value — a non-integral key — surfacing as an error
// rather than silently healing into something else.
func TestConfigRejectsFractionalKey(t *testing.T) {
	a, configPath := newTestApp(t)

	broken := "port = 24800\n" +
		"\n[[screens]]\nname = 'pc'\nwidth = 1920\nheight = 1080\nx = 0\ny = 0\n" +
		"\n[shortcuts]\nenabled = true\n" +
		"[[shortcuts.bindings]]\nmods = {}\nkey = 71.5\naction = 'cycle'\n"
	if err := os.WriteFile(configPath, []byte(broken), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, err := a.LoadConfig(); err == nil {
		t.Fatal("a fractional HID id must not load (it is real corruption, not a writer quirk)")
	}
}

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

// The trust/revoke lists survive a save that does not mention them (a
// layout edit), while an explicit empty list still clears them. Without
// the nil-means-keep rule, editing a screen would silently wipe the
// server's revocation policy.
func TestSaveConfigPreservesOmittedPolicyLists(t *testing.T) {
	a, _ := newTestApp(t)

	base := Config{
		Port:    defaultPort,
		Screens: []Screen{{Name: "pc", Width: 1920, Height: 1080, X: 0, Y: 0}},
		Network: Network{
			Allowlist:  true,
			LocalOnly:  true,
			TrustedIDs: []string{"70b97d38"},
			RevokedIDs: []string{"aabbccdd11223344"},
		},
	}
	if err := a.SaveConfig(base); err != nil {
		t.Fatal(err)
	}

	// A layout edit that omits the network lists (nil) keeps them.
	layoutOnly := base
	layoutOnly.Screens = append(layoutOnly.Screens, Screen{Name: "hp", Width: 1920, Height: 1080, X: -1920, Y: 0})
	layoutOnly.Network.TrustedIDs = nil
	layoutOnly.Network.RevokedIDs = nil
	if err := a.SaveConfig(layoutOnly); err != nil {
		t.Fatal(err)
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if !idTrusted(cfg.Network.TrustedIDs, "70b97d38") {
		t.Fatalf("trusted ids were wiped by a layout save: %v", cfg.Network.TrustedIDs)
	}
	if !idRevoked(cfg.Network.RevokedIDs, "aabbccdd11223344") {
		t.Fatalf("revoked ids were wiped by a layout save: %v", cfg.Network.RevokedIDs)
	}

	// An explicit empty list clears them.
	cleared := cfg
	cleared.Network.TrustedIDs = []string{}
	cleared.Network.RevokedIDs = []string{}
	if err := a.SaveConfig(cleared); err != nil {
		t.Fatal(err)
	}
	cfg, err = a.LoadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if len(cfg.Network.TrustedIDs) != 0 || len(cfg.Network.RevokedIDs) != 0 {
		t.Fatalf("an explicit empty list must clear the policy: %+v", cfg.Network)
	}
}
