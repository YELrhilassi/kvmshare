package main

import (
	"fmt"
	"kvmshare/gui/internal/fileutil"
	"os"
	"strings"

	"github.com/pelletier/go-toml/v2"
)

// Defaults used when a config file omits values. Must match the Rust
// server's defaults (crates/app/src/lib.rs: DEFAULT_PORT / W / H).
const (
	defaultPort    = 24800
	defaultScreenW = 1920
	defaultScreenH = 1080
)

// Screen mirrors one [[screens]] entry in the config (JSON shape the
// frontend works with).
type Screen struct {
	Name   string `json:"name"`
	Width  int    `json:"width"`
	Height int    `json:"height"`
	X      int    `json:"x"`
	Y      int    `json:"y"`
}

// Network is the `[network]` section the frontend edits: who may
// connect to this server. TrustedIDs and RevokedIDs are independent —
// an id may appear in both, and a revoked id is always refused (a hard
// deny that outranks the layout and the trusted list).
type Network struct {
	Allowlist  bool     `json:"allowlist"`
	LocalOnly  bool     `json:"localOnly"`
	TrustedIDs []string `json:"trustedIds"`
	RevokedIDs []string `json:"revokedIds"`
}

// Config is the layout the frontend edits.
type Config struct {
	Port    int      `json:"port"`
	Screens []Screen `json:"screens"`
	Network Network  `json:"network"`
	// Sections the GUI does not invent but edits precisely: their on-disk
	// shape is owned by the Rust server, so these are typed structs with
	// tags matching its schema exactly. They used to travel as generic
	// key/value maps — through JavaScript every number became a float64,
	// so the file gained `key = 71.0` and camelCase input names, and the
	// Rust server rejected the whole sections. Bindings then silently
	// never registered.
	Shortcuts *ShortcutSection `json:"shortcuts,omitempty"`
	Input     *InputSection    `json:"input,omitempty"`
}

// ShortcutSection is the `[shortcuts]` TOML section (schema owned by
// kvmshare_core::actions::BindSection).
type ShortcutSection struct {
	Enabled  bool              `json:"enabled" toml:"enabled"`
	Bindings []ShortcutBinding `json:"bindings" toml:"bindings"`
}

// ShortcutBinding is one chord → action entry.
type ShortcutBinding struct {
	Mods   ShortcutMods `json:"mods" toml:"mods"`
	Key    uint32       `json:"key" toml:"key"`
	Action string       `json:"action" toml:"action"`
	Screen string       `json:"screen,omitempty" toml:"screen,omitempty"`
}

// ShortcutMods is the modifier set of a chord.
type ShortcutMods struct {
	Ctrl  bool `json:"ctrl" toml:"ctrl"`
	Alt   bool `json:"alt" toml:"alt"`
	Shift bool `json:"shift" toml:"shift"`
	Meta  bool `json:"meta" toml:"meta"`
}

// InputSection is the `[input]` feel section. The file uses the Rust
// server's snake_case names; the frontend JSON uses camelCase.
type InputSection struct {
	PointerSpeed float64 `json:"pointerSpeed" toml:"-"`
	WheelSpeed   float64 `json:"wheelSpeed" toml:"-"`
	SwapScroll   bool    `json:"swapScroll" toml:"-"`
}

// configFile is the on-disk TOML shape. Kept separate from the JSON shape
// so the two formats can evolve independently.
type configFile struct {
	Port      int              `toml:"port"`
	Screens   []screenFile     `toml:"screens"`
	Network   networkFile      `toml:"network"`
	Shortcuts *ShortcutSection `toml:"shortcuts,omitempty"`
	Input     *inputFile       `toml:"input,omitempty"`
}

// inputFile is the on-disk `[input]` shape: snake_case, as the Rust
// server's serde schema writes and reads it.
type inputFile struct {
	PointerSpeed float64 `toml:"pointer_speed"`
	WheelSpeed   float64 `toml:"wheel_speed"`
	SwapScroll   bool    `toml:"swap_scroll"`
}

// jsonInput converts the file shape to the frontend shape.
func (f *inputFile) jsonInput() *InputSection {
	return &InputSection{PointerSpeed: f.PointerSpeed, WheelSpeed: f.WheelSpeed, SwapScroll: f.SwapScroll}
}

// fileInput converts the frontend shape to the file shape.
func (s *InputSection) fileInput() *inputFile {
	return &inputFile{PointerSpeed: s.PointerSpeed, WheelSpeed: s.WheelSpeed, SwapScroll: s.SwapScroll}
}

type networkFile struct {
	Allowlist  bool     `toml:"allowlist"`
	LocalOnly  bool     `toml:"local_only"`
	TrustedIDs []string `toml:"trusted_ids"`
	RevokedIDs []string `toml:"revoked_ids"`
}

type screenFile struct {
	Name   string  `toml:"name"`
	Width  int     `toml:"width"`
	Height int     `toml:"height"`
	X      int     `toml:"x"`
	Y      int     `toml:"y"`
	Scale  float32 `toml:"scale,omitempty"`
}

// LoadConfig returns the current layout. When no config exists yet it
// returns a sensible default (a two-machine desktop) without writing.
// loadConfigCached is LoadConfig with a mtime+size-keyed cache, for
// the per-second state loop: the file is re-read only when it changed
// (or the cache is cold), so a tick costs one stat call, not a full
// TOML parse. Any writer (SaveConfig, a manual edit) bumps the mtime
// and is picked up on the next tick. Errors are cached too — a missing
// file does not turn every tick into a syscall round trip.
func (a *App) loadConfigCached() (Config, error) {
	var key string
	if fi, err := os.Stat(a.configPath); err == nil {
		key = fmt.Sprintf("%d|%d", fi.ModTime().UnixNano(), fi.Size())
	} else {
		key = "missing"
	}

	a.cfgMu.Lock()
	defer a.cfgMu.Unlock()
	if a.cfgHave && a.cfgKey == key {
		if a.cfgMissing {
			return defaultConfig(), nil
		}
		return a.cfgCached, nil
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		// Do not cache unexpected errors (disk trouble, a torn write):
		// the next tick may see a complete file.
		return Config{}, err
	}
	a.cfgHave = true
	a.cfgKey = key
	a.cfgMissing = key == "missing"
	a.cfgCached = cfg
	return cfg, nil
}

func (a *App) LoadConfig() (Config, error) {
	raw, err := os.ReadFile(a.configPath)
	if err != nil {
		if os.IsNotExist(err) {
			return defaultConfig(), nil
		}
		return Config{}, fmt.Errorf("read config: %w", err)
	}

	var cf configFile
	if err := toml.Unmarshal(raw, &cf); err != nil {
		// Older GUI releases wrote integral numbers as floats (`key =
		// 71.0`) — JavaScript's only number type leaking into the file —
		// which strict decoding rejects. Heal the file once instead of
		// leaving the user with a page that can never load.
		healed, herr := healConfigText(raw)
		if herr != nil {
			return Config{}, fmt.Errorf("parse config %s: %w", a.configPath, err)
		}
		if err := toml.Unmarshal(healed, &cf); err != nil {
			return Config{}, fmt.Errorf("parse config %s: %w", a.configPath, err)
		}
		// Persist the healed shape so the next start parses strictly.
		_ = fileutil.Write(a.configPath, healed, 0o644)
	}
	if cf.Port == 0 {
		cf.Port = defaultPort // the Rust server applies the same default
	}

	cfg := Config{Port: cf.Port, Screens: make([]Screen, 0, len(cf.Screens))}
	for _, s := range cf.Screens {
		cfg.Screens = append(cfg.Screens, Screen{
			Name:   s.Name,
			Width:  s.Width,
			Height: s.Height,
			X:      s.X,
			Y:      s.Y,
		})
	}
	cfg.Network = Network{
		Allowlist:  cf.Network.Allowlist,
		LocalOnly:  cf.Network.LocalOnly,
		TrustedIDs: nonNilStrings(cf.Network.TrustedIDs),
		RevokedIDs: nonNilStrings(cf.Network.RevokedIDs),
	}
	if cf.Shortcuts != nil {
		cfg.Shortcuts = cf.Shortcuts
	}
	if cf.Input != nil {
		cfg.Input = cf.Input.jsonInput()
	}
	// Old configs have no [network] section; default to secure.
	if !cf.Network.Allowlist && !cf.Network.LocalOnly && len(cf.Network.TrustedIDs) == 0 {
		cfg.Network.Allowlist = true
		cfg.Network.LocalOnly = true
	}
	return cfg, nil
}

// nonNilStrings returns `s`, or an empty (non-nil) slice when s is nil
// — JSON would otherwise encode nil as `null`, which frontends must
// never see for a list field.
func nonNilStrings(s []string) []string {
	if s == nil {
		return []string{}
	}
	return s
}

// SaveConfig writes the layout and validates it. The first screen is
// always the server's own screen. If the server is running it notices the
// change on disk and adopts it live — no restart needed.
func (a *App) SaveConfig(cfg Config) error {
	a.mu.Lock()
	defer a.mu.Unlock()

	if len(cfg.Screens) == 0 {
		return fmt.Errorf("at least one screen is required (the server's own)")
	}
	// Screen names are how clients are matched to screens on the wire, so
	// they must be non-empty and unique — a bad name silently breaks
	// connections.
	seen := map[string]bool{}
	for i, s := range cfg.Screens {
		s.Name = strings.TrimSpace(s.Name)
		if s.Name == "" {
			return fmt.Errorf("screen %d has no name", i+1)
		}
		if seen[s.Name] {
			return fmt.Errorf("screen name %q is used more than once", s.Name)
		}
		seen[s.Name] = true
		if s.Width <= 0 || s.Height <= 0 {
			return fmt.Errorf("screen %q has an invalid size", s.Name)
		}
	}
	// Lists the caller omitted (nil) keep their on-disk values: editing the
	// layout must never silently clear the trust/revoke policy. An explicit
	// empty list (non-nil — JSON `[]` decodes that way) does clear it, which
	// is how the Server page removes the last entry.
	if cfg.Network.TrustedIDs == nil || cfg.Network.RevokedIDs == nil {
		if current, err := a.LoadConfig(); err == nil {
			if cfg.Network.TrustedIDs == nil {
				cfg.Network.TrustedIDs = current.Network.TrustedIDs
			}
			if cfg.Network.RevokedIDs == nil {
				cfg.Network.RevokedIDs = current.Network.RevokedIDs
			}
		}
	}
	// Sections the frontend did not touch keep their on-disk values.
	if cfg.Shortcuts == nil || cfg.Input == nil {
		if current, err := a.LoadConfig(); err == nil {
			if cfg.Shortcuts == nil {
				cfg.Shortcuts = current.Shortcuts
			}
			if cfg.Input == nil {
				cfg.Input = current.Input
			}
		}
	}
	cf := configFile{
		Port: cfg.Port,
		Network: networkFile{
			Allowlist:  cfg.Network.Allowlist,
			LocalOnly:  cfg.Network.LocalOnly,
			TrustedIDs: cfg.Network.TrustedIDs,
			RevokedIDs: cfg.Network.RevokedIDs,
		},
		Shortcuts: cfg.Shortcuts,
		Input:     nil,
	}
	if cfg.Input != nil {
		cf.Input = cfg.Input.fileInput()
	}
	for _, s := range cfg.Screens {
		cf.Screens = append(cf.Screens, screenFile{
			Name:   strings.TrimSpace(s.Name),
			Width:  s.Width,
			Height: s.Height,
			X:      s.X,
			Y:      s.Y,
			Scale:  1.0,
		})
	}
	raw, err := toml.Marshal(cf)
	if err != nil {
		return fmt.Errorf("encode config: %w", err)
	}
	// Atomic replace under the cross-process lock: the running server
	// watches this file AND writes it too (auto-trust, screen-size
	// correction). Both writers read-modify-write the whole file, so
	// without the shared lock a save could land between the server's
	// read and write and erase its change — a trust edit vanishing, the
	// flapping users saw. The lock (same file the Rust side takes)
	// makes the whole read-modify-write series exclusive.
	if err := fileutil.WriteLocked(a.configPath, raw, 0o644, true); err != nil {
		return fmt.Errorf("write config: %w", err)
	}

	return nil
}

// defaultConfig is shown when no config file exists yet (never written
// until the user saves). It describes this machine only — the real
// server binary creates the same shape (its own name + display) on its
// first start, so the two defaults agree and no invented client screens
// ever appear.
func defaultConfig() Config {
	name := "server"
	if h, err := os.Hostname(); err == nil && strings.TrimSpace(h) != "" {
		name = strings.TrimSpace(h)
	}
	return Config{
		Port: defaultPort,
		Screens: []Screen{
			{Name: name, Width: defaultScreenW, Height: defaultScreenH, X: 0, Y: 0},
		},
		Network: Network{Allowlist: true, LocalOnly: true},
	}
}
