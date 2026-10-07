package main

// media_test.go — the media/audio settings surface. The pieces that can
// silently corrupt a config file (validation, round-tripping, preserve-
// other-sections writes) are the ones that get pinned here; the GUI has
// no other defence against writing a file the Rust side refuses to load.

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/pelletier/go-toml/v2"
)

func TestValidMediaTarget(t *testing.T) {
	valid := []string{
		"local", "follow_focus", "last_active_source", "focus_or_last_active",
		"machine:98980a4d", "machine:some-name",
	}
	for _, v := range valid {
		if !validMediaTarget(v) {
			t.Errorf("validMediaTarget(%q) = false, want true", v)
		}
	}
	invalid := []string{
		"", "machine:", "machine:  ", "local_only", "audio", "FOLLOW_FOCUS",
	}
	for _, v := range invalid {
		if validMediaTarget(v) {
			t.Errorf("validMediaTarget(%q) = true, want false", v)
		}
	}
}

// A media section the GUI refuses must match what the Rust parser
// refuses — checked against the same examples its own tests use.
func TestValidateMediaMatchesTheRustRules(t *testing.T) {
	good := defaultMedia()
	if err := validateMedia(good); err != nil {
		t.Fatalf("default media section rejected: %v", err)
	}
	bad := defaultMedia()
	bad.Transport = "typo"
	if err := validateMedia(bad); err == nil {
		t.Fatal("a typo'd target must be rejected, not saved")
	}
}

func TestValidateAudioBounds(t *testing.T) {
	if err := validateAudio(defaultAudio()); err != nil {
		t.Fatalf("default audio section rejected: %v", err)
	}
	tooLoud := defaultAudio()
	tooLoud.ActivityFloorDb = 1
	if err := validateAudio(tooLoud); err == nil {
		t.Fatal("a floor above 0 dBFS must be rejected")
	}
	tooQuiet := defaultAudio()
	tooQuiet.ActivityFloorDb = -121
	if err := validateAudio(tooQuiet); err == nil {
		t.Fatal("a floor below -120 dBFS must be rejected")
	}
}

// Saving the media/audio sections must not disturb the rest of the
// config: the layout, the trust policy and the shortcut bindings all
// live in the same file.
func TestSaveMediaAudioPreservesOtherSections(t *testing.T) {
	dir := t.TempDir()
	cfg := filepath.Join(dir, "kvmshare-server.toml")
	existing := `
[[screens]]
name = "pc"
width = 1920
height = 1080

[network]
allowlist = true
trusted_ids = ["abc123"]

[shortcuts]
enabled = true
bindings = []

[input]
pointer_speed = 1.0
wheel_speed = 1.0
swap_scroll = false
`
	if err := os.WriteFile(cfg, []byte(existing), 0o644); err != nil {
		t.Fatal(err)
	}

	a := &App{configPath: cfg, stateDir: dir}
	if err := a.SaveMediaAudio(defaultMedia(), defaultAudio()); err != nil {
		t.Fatalf("save: %v", err)
	}

	raw, err := os.ReadFile(cfg)
	if err != nil {
		t.Fatal(err)
	}
	text := string(raw)
	for _, want := range []string{"pc", "abc123", "pointer_speed"} {
		if !strings.Contains(text, want) {
			t.Errorf("saved config lost %q\n---\n%s", want, text)
		}
	}
	var cf struct {
		Media *mediaFile `toml:"media"`
	}
	if err := toml.Unmarshal(raw, &cf); err != nil {
		t.Fatal(err)
	}
	if cf.Media == nil || !cf.Media.RouteMediaKeys {
		t.Error("[media] section missing or wrong after save")
	}
}

// A config the server has not created yet gets one created by the save —
// media settings are editable before the server ever ran.
func TestSaveMediaAudioCreatesMissingConfig(t *testing.T) {
	dir := t.TempDir()
	a := &App{configPath: filepath.Join(dir, "missing.toml"), stateDir: dir}
	if err := a.SaveMediaAudio(defaultMedia(), defaultAudio()); err != nil {
		t.Fatalf("save to a missing config: %v", err)
	}
	if _, err := os.Stat(a.configPath); err != nil {
		t.Errorf("config not created: %v", err)
	}
}

// An invalid save must not touch the file at all — a half-applied
// settings page is worse than a rejected one.
func TestSaveMediaAudioRejectsInvalidWithoutWriting(t *testing.T) {
	dir := t.TempDir()
	cfg := filepath.Join(dir, "kvmshare-server.toml")
	before := []byte("[media]\nroute_media_keys = true\n")
	if err := os.WriteFile(cfg, before, 0o644); err != nil {
		t.Fatal(err)
	}
	a := &App{configPath: cfg, stateDir: dir}
	bad := defaultMedia()
	bad.Transport = "nope"
	if err := a.SaveMediaAudio(bad, defaultAudio()); err == nil {
		t.Fatal("invalid media section accepted")
	}
	raw, _ := os.ReadFile(cfg)
	if string(raw) != string(before) {
		t.Error("a rejected save must not modify the file")
	}
}

// The client's audio file round-trips and never grows a `peer` field —
// the client has exactly one peer (the server that admitted it), and a
// meaningless field invites someone to set it.
func TestClientAudioRoundTripDropsPeer(t *testing.T) {
	dir := t.TempDir()
	t.Setenv("KVMSHARE_CLIENT_CONFIG", filepath.Join(dir, "kvmshare-client.toml"))

	a := &App{stateDir: dir}
	got, err := a.LoadClientAudio()
	if err != nil {
		t.Fatalf("load with no file: %v", err)
	}
	if got.Send || got.Receive {
		t.Error("a missing client file must be the inert default")
	}

	got.Send = true
	got.Receive = true
	got.Peer = "should-not-survive"
	if err := a.SaveClientAudio(got); err != nil {
		t.Fatalf("save: %v", err)
	}
	back, err := a.LoadClientAudio()
	if err != nil {
		t.Fatalf("reload: %v", err)
	}
	if !back.Send || !back.Receive {
		t.Error("audio settings did not round-trip")
	}
	if back.Peer != "" {
		t.Errorf("peer = %q, want empty (the client file must not carry a peer)", back.Peer)
	}
}

// The client file must stay a client file: whatever else is in it (a
// pasted server layout, for instance) survives untouched.
func TestSaveClientAudioPreservesOtherContent(t *testing.T) {
	dir := t.TempDir()
	path := filepath.Join(dir, "kvmshare-client.toml")
	t.Setenv("KVMSHARE_CLIENT_CONFIG", path)
	if err := os.WriteFile(path, []byte("port = 24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	a := &App{stateDir: dir}
	if err := a.SaveClientAudio(defaultAudio()); err != nil {
		t.Fatalf("save: %v", err)
	}
	raw, _ := os.ReadFile(path)
	if !strings.Contains(string(raw), "port = 24800") {
		t.Errorf("client file lost unrelated content:\n%s", raw)
	}
}
