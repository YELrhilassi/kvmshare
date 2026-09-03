package main

import (
	"fmt"
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

// Config is the layout the frontend edits.
type Config struct {
	Port    int      `json:"port"`
	Screens []Screen `json:"screens"`
}

// configFile is the on-disk TOML shape. Kept separate from the JSON shape
// so the two formats can evolve independently.
type configFile struct {
	Port    int          `toml:"port"`
	Screens []screenFile `toml:"screens"`
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
		return Config{}, fmt.Errorf("parse config %s: %w", a.configPath, err)
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
	return cfg, nil
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
	cf := configFile{Port: cfg.Port}
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
	// Atomic replace: the running server watches this file, so it must
	// never see a torn write.
	if err := atomicWriteFile(a.configPath, raw, 0o644); err != nil {
		return fmt.Errorf("write config: %w", err)
	}

	return nil
}

// defaultConfig is shown when no config file exists yet (never written
// until the user saves).
func defaultConfig() Config {
	return Config{
		Port: defaultPort,
		Screens: []Screen{
			{Name: "server", Width: defaultScreenW, Height: defaultScreenH, X: 0, Y: 0},
			{Name: "client", Width: defaultScreenW, Height: defaultScreenH, X: -defaultScreenW, Y: 0},
		},
	}
}
