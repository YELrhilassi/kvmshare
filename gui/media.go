package main

// media.go — the GUI side of media routing and audio sharing.
//
// Two different files, because the settings describe two different
// machines:
//
//   - the server's kvmshare-server.toml owns [media] (where media keys go
//     when this machine is the one being typed on) and [audio] (which
//     machine it streams its output to, when it is the server). The Rust
//     server hot-reloads both; the GUI edits them precisely, the way it
//     already edits [shortcuts] and [input] (see config.go — typed Go
//     structs, not generic maps, so numbers stay numbers).
//
//   - the client's kvmshare-client.toml owns this machine's own [audio]:
//     whether a *client* captures and plays. The layout on a client comes
//     from the server over the wire, so the client's file is only ever
//     the audio consent — which is why it is a separate file with a
//     separate schema (see crates/app/src/config/client.rs).
//
// The frontend sees one flat JSON shape per role; the mapping lives here,
// next to the other file-shape mappings (config.go).

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/pelletier/go-toml/v2"
	"kvmshare/gui/internal/fileutil"
)

// MediaTarget is one routing decision: where a category of media keys
// goes. The wire strings are owned by kvmshare_core::media::MediaTarget —
// the GUI passes them through, and an unknown value is rejected against
// the same list the Rust parser enforces, so a bad save can never leave
// the server refusing to load its own config.
type MediaTarget string

const (
	mediaTargetLocal             MediaTarget = "local"
	mediaTargetFollowFocus       MediaTarget = "follow_focus"
	mediaTargetLastActiveSource  MediaTarget = "last_active_source"
	mediaTargetFocusOrLastActive MediaTarget = "focus_or_last_active"
	// "machine:<id>" is constructed, not a constant.
)

// validMediaTarget reports whether `t` is a value the Rust server's
// config parser accepts (kvmshare_core::media::MediaTarget::parse). A
// pinned machine is `machine:` + a non-empty id.
func validMediaTarget(t string) bool {
	if id, ok := strings.CutPrefix(t, "machine:"); ok {
		return strings.TrimSpace(id) != ""
	}
	switch MediaTarget(t) {
	case mediaTargetLocal, mediaTargetFollowFocus, mediaTargetLastActiveSource, mediaTargetFocusOrLastActive:
		return true
	}
	return false
}

// MediaSection is the `[media]` section as the frontend sees it.
type MediaSection struct {
	RouteMediaKeys bool   `json:"routeMediaKeys"`
	Transport      string `json:"transport"`
	Volume         string `json:"volume"`
	FallbackLocal  bool   `json:"fallbackLocal"`
}

// AudioSection is the `[audio]` section as the frontend sees it. One
// shape for both roles: the server's copy additionally carries `peer`
// (which client to pair with), which is meaningless on a client — the
// field is simply absent there (omitempty), and the client page never
// renders a peer picker.
type AudioSection struct {
	Send            bool    `json:"send"`
	Receive         bool    `json:"receive"`
	CaptureDevice   string  `json:"captureDevice"`
	PlaybackDevice  string  `json:"playbackDevice"`
	ActivityFloorDb float64 `json:"activityFloorDb"`
	Peer            string  `json:"peer,omitempty"`
}

// mediaFile / audioFile are the on-disk `[media]` / `[audio]` shapes:
// snake_case, as the Rust server's serde schema writes and reads them.
type mediaFile struct {
	RouteMediaKeys bool   `toml:"route_media_keys"`
	Transport      string `toml:"transport"`
	Volume         string `toml:"volume"`
	FallbackLocal  bool   `toml:"fallback_local"`
}

type audioFile struct {
	Send            bool    `toml:"send"`
	Receive         bool    `toml:"receive"`
	CaptureDevice   string  `toml:"capture_device"`
	PlaybackDevice  string  `toml:"playback_device"`
	ActivityFloorDb float64 `toml:"activity_floor_db"`
	Peer            string  `toml:"peer,omitempty"`
}

func (s MediaSection) file() mediaFile {
	return mediaFile{
		RouteMediaKeys: s.RouteMediaKeys,
		Transport:      s.Transport,
		Volume:         s.Volume,
		FallbackLocal:  s.FallbackLocal,
	}
}

func (s AudioSection) file() audioFile {
	return audioFile{
		Send:            s.Send,
		Receive:         s.Receive,
		CaptureDevice:   s.CaptureDevice,
		PlaybackDevice:  s.PlaybackDevice,
		ActivityFloorDb: s.ActivityFloorDb,
		Peer:            s.Peer,
	}
}

func (f mediaFile) json() MediaSection {
	return MediaSection{
		RouteMediaKeys: f.RouteMediaKeys,
		Transport:      f.Transport,
		Volume:         f.Volume,
		FallbackLocal:  f.FallbackLocal,
	}
}

func (f audioFile) json() AudioSection {
	return AudioSection{
		Send:            f.Send,
		Receive:         f.Receive,
		CaptureDevice:   f.CaptureDevice,
		PlaybackDevice:  f.PlaybackDevice,
		ActivityFloorDb: f.ActivityFloorDb,
		Peer:            f.Peer,
	}
}

// defaults matching the Rust server's `Default` impls, so a page shown
// before the first save agrees with what the server would write.
func defaultMedia() MediaSection {
	return MediaSection{
		RouteMediaKeys: true,
		Transport:      string(mediaTargetFollowFocus),
		Volume:         string(mediaTargetFollowFocus),
		FallbackLocal:  true,
	}
}

func defaultAudio() AudioSection {
	return AudioSection{ActivityFloorDb: -50.0}
}

// validateMedia rejects a routing section the Rust parser would refuse,
// naming the field, so the error points at the control that is wrong.
func validateMedia(m MediaSection) error {
	if !validMediaTarget(m.Transport) {
		return fmt.Errorf("[media] transport: unknown target %q", m.Transport)
	}
	if !validMediaTarget(m.Volume) {
		return fmt.Errorf("[media] volume: unknown target %q", m.Volume)
	}
	return nil
}

// validateAudio rejects an audio section that cannot be honoured — the
// same bounds `AudioConfig::validate` enforces, so a save the GUI accepts
// is a config the role never rejects. The comparison is written as a
// single inclusive-range test rather than two >/< checks so a NaN floor
// is rejected too (every comparison with NaN is false): the Rust side
// refuses NaN via `is_finite`, and the two validators must agree.
func validateAudio(a AudioSection) error {
	if !(a.ActivityFloorDb >= -120 && a.ActivityFloorDb <= 0) {
		return fmt.Errorf("[audio] silence floor must be between -120 and 0 dBFS")
	}
	return nil
}

// LoadMediaAudio returns the server-role media routing and audio sharing
// settings from the server's config file.
func (a *App) LoadMediaAudio() (MediaSection, AudioSection, error) {
	raw, err := os.ReadFile(a.configPath)
	if os.IsNotExist(err) {
		// No config yet: the defaults the server will write on its first
		// start. Nothing to read is not an error to show.
		return defaultMedia(), defaultAudio(), nil
	}
	if err != nil {
		return MediaSection{}, AudioSection{}, fmt.Errorf("read config: %w", err)
	}
	var cf struct {
		Media  *mediaFile  `toml:"media"`
		Audio  *audioFile  `toml:"audio"`
	}
	if err := toml.Unmarshal(raw, &cf); err != nil {
		return MediaSection{}, AudioSection{}, fmt.Errorf("parse config: %w", err)
	}
	m, au := defaultMedia(), defaultAudio()
	if cf.Media != nil {
		m = cf.Media.json()
	}
	if cf.Audio != nil {
		au = cf.Audio.json()
	}
	return m, au, nil
}

// SaveMediaAudio writes the server-role `[media]` and `[audio]` sections
// into the server's config file, preserving every other section. The
// running server hot-reloads the file; nothing here touches the process.
func (a *App) SaveMediaAudio(m MediaSection, au AudioSection) error {
	if err := validateMedia(m); err != nil {
		return err
	}
	if err := validateAudio(au); err != nil {
		return err
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	// Read-modify-write under the shared config lock, exactly like
	// SaveConfig: the server also writes this file (auto-trust, screen
	// corrections), and the lock is what keeps the two writers from
	// erasing each other.
	raw, err := os.ReadFile(a.configPath)
	if err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("read config: %w", err)
	}
	var cf map[string]any
	if len(raw) > 0 {
		if err := toml.Unmarshal(raw, &cf); err != nil {
			return fmt.Errorf("parse config: %w", err)
		}
	}
	if cf == nil {
		cf = map[string]any{}
	}
	cf["media"] = m.file()
	cf["audio"] = au.file()
	out, err := toml.Marshal(cf)
	if err != nil {
		return fmt.Errorf("encode config: %w", err)
	}
	if err := fileutil.WriteLocked(a.configPath, out, 0o644, true); err != nil {
		return fmt.Errorf("write config: %w", err)
	}
	return nil
}

// clientConfigPath resolves the client role's own config file — the
// machine-local audio consent. The client binary resolves the same path
// (KVMSHARE_CLIENT_CONFIG, then ~/.config/kvmshare/kvmshare-client.toml);
// the GUI writes where the client reads.
func (a *App) clientConfigPath() string {
	if p := os.Getenv("KVMSHARE_CLIENT_CONFIG"); p != "" {
		return p
	}
	if home, err := os.UserHomeDir(); err == nil && home != "" {
		return filepath.Join(home, ".config", "kvmshare", "kvmshare-client.toml")
	}
	// No home at all (rare): beside the state dir, which always exists.
	return filepath.Join(a.stateDir, "kvmshare-client.toml")
}

// LoadClientAudio returns this machine's own audio settings — what the
// client role reads, whatever server it connects to. A missing file is
// the inert default (no audio), which is what every machine that never
// opened this page wants.
func (a *App) LoadClientAudio() (AudioSection, error) {
	raw, err := os.ReadFile(a.clientConfigPath())
	if os.IsNotExist(err) {
		return defaultAudio(), nil
	}
	if err != nil {
		return AudioSection{}, fmt.Errorf("read client config: %w", err)
	}
	var cf struct {
		Audio *audioFile `toml:"audio"`
	}
	if err := toml.Unmarshal(raw, &cf); err != nil {
		return AudioSection{}, fmt.Errorf("parse client config: %w", err)
	}
	if cf.Audio == nil {
		return defaultAudio(), nil
	}
	return cf.Audio.json(), nil
}

// SaveClientAudio writes the client role's audio settings. The client
// re-reads the file on every (re)connect, so a save applies at the next
// connection without touching the process — the same contract the
// server's hot reload honours.
func (a *App) SaveClientAudio(au AudioSection) error {
	if err := validateAudio(au); err != nil {
		return err
	}
	// The client's file is only the audio consent; a `peer` value would
	// be dead weight here (the client has exactly one peer — the server
	// that admitted it), so it is never written.
	au.Peer = ""
	path := a.clientConfigPath()
	raw, err := os.ReadFile(path)
	if err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("read client config: %w", err)
	}
	var cf map[string]any
	if len(raw) > 0 {
		if err := toml.Unmarshal(raw, &cf); err != nil {
			return fmt.Errorf("parse client config: %w", err)
		}
	}
	if cf == nil {
		cf = map[string]any{}
	}
	cf["audio"] = au.file()
	out, err := toml.Marshal(cf)
	if err != nil {
		return fmt.Errorf("encode client config: %w", err)
	}
	if err := fileutil.Write(path, out, 0o644); err != nil {
		return fmt.Errorf("write client config: %w", err)
	}
	return nil
}
