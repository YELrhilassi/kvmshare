// Backend methods bound into the frontend.
//
// One GUI runs both roles — this machine is either the KVM server (the
// one whose keyboard/mouse is shared) or a client (a controlled machine).
// The role is a GUI-level setting that decides which process the main
// Start/Stop controls; both processes can still be managed individually
// from their pages.
//
// The code is split by responsibility into small files:
//
//   app.go     — App state, file resolution, persisted settings
//   config.go  — the layout config (kvmshare-server.toml)
//   process.go — spawning/stopping the server and client processes
//   netlog.go  — network interfaces and log tailing
//
// All long-lived processes log to ~/.local/state/kvmshare/ so the GUI can
// tail them live.

package main

import (
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sync"
)

// Mode is what this machine does in the KVM.
type Mode string

const (
	ModeServer Mode = "server"
	ModeClient Mode = "client"
)

// Settings is the GUI's own persisted state (role + client connection).
type Settings struct {
	Mode       Mode   `json:"mode"`
	ClientAddr string `json:"clientAddr"` // host:port of the server to connect to
	ClientName string `json:"clientName"` // screen name used on the server
}

// Paths reports where everything lives (config, logs, binaries).
type Paths struct {
	ConfigPath string `json:"configPath"`
	ServerLog  string `json:"serverLog"`
	ClientLog  string `json:"clientLog"`
	ServerBin  string `json:"serverBin"`
	ClientBin  string `json:"clientBin"`
}

// App is the Wails-bound backend. Methods on this type are what the
// frontend can call (window.go.main.App.*).
type App struct {
	stateDir      string
	configPath    string
	serverPath    string
	clientPath    string
	settingsPath  string
	serverLogPath string
	clientLogPath string

	// Instance lock: only one GUI may manage processes per machine.
	instanceLockPath string
	instanceLock     *os.File

	mu         sync.Mutex
	settings   Settings
	serverProc *proc
	clientProc *proc

	// Lifecycle notifications (client connect/disconnect from the server
	// log). nil until StartNotifyWatcher is called.
	notify *notify
}

// NewApp locates every file the GUI needs.
//
// Config search order: KVMSHARE_CONFIG, then ~/.config/kvmshare/, then
// next to the executable (development builds). Binaries via env var, then
// PATH, then next to the executable. Logs and the GUI's own settings live
// in ~/.local/state/kvmshare/ (XDG_STATE_HOME default).
func NewApp() *App {
	dir := executableDir()
	home, _ := os.UserHomeDir()

	configPath := firstNonEmpty(os.Getenv("KVMSHARE_CONFIG"))
	if configPath == "" && home != "" {
		candidate := filepath.Join(home, ".config", "kvmshare", "kvmshare-server.toml")
		if fileExists(candidate) {
			configPath = candidate
		}
	}
	if configPath == "" {
		configPath = filepath.Join(dir, "kvmshare-server.toml")
	}

	serverPath := firstNonEmpty(os.Getenv("KVMSHARE_SERVER"))
	if serverPath == "" {
		serverPath = lookPathElse("kvmshare-server", filepath.Join(dir, "kvmshare-server"))
	}
	clientPath := firstNonEmpty(os.Getenv("KVMSHARE_CLIENT"))
	if clientPath == "" {
		clientPath = lookPathElse("kvmshare-client", filepath.Join(dir, "kvmshare-client"))
	}

	stateDir := filepath.Join(home, ".local", "state", "kvmshare")
	if home == "" {
		stateDir = dir
	}
	_ = os.MkdirAll(stateDir, 0o755)

	a := &App{
		stateDir:         stateDir,
		configPath:       configPath,
		serverPath:       serverPath,
		clientPath:       clientPath,
		settingsPath:     filepath.Join(stateDir, "gui.json"),
		serverLogPath:    filepath.Join(stateDir, "server.log"),
		clientLogPath:    filepath.Join(stateDir, "client.log"),
		instanceLockPath: filepath.Join(stateDir, "gui.lock"),
		settings: Settings{
			Mode:       ModeServer,
			ClientName: hostnameOr("client"),
		},
	}
	a.loadSettings()
	a.notify = newNotify(a.serverLogPath)
	return a
}

// SingleInstance takes the GUI's instance lock. Only one GUI per machine
// may manage processes; a second instance exits with an error. The lock
// (flock) is released automatically when this process dies — no stale
// state after a crash.
func (a *App) SingleInstance() error {
	f, err := os.OpenFile(a.instanceLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return fmt.Errorf("open instance lock: %w", err)
	}
	if err := tryLockFile(f); err != nil {
		f.Close()
		return fmt.Errorf("kvmshare is already running on this machine")
	}
	a.instanceLock = f
	return nil
}

func lookPathElse(name, fallback string) string {
	if p, err := exec.LookPath(name); err == nil {
		return p
	}
	return fallback
}

func hostnameOr(fallback string) string {
	if h, err := os.Hostname(); err == nil && h != "" {
		return h
	}
	return fallback
}

func fileExists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

func executableDir() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	return filepath.Dir(exe)
}

func firstNonEmpty(vals ...string) string {
	for _, v := range vals {
		if v != "" {
			return v
		}
	}
	return ""
}

// ---------------------------------------------------------------------------
// Settings (role + client connection), persisted to gui.json
// ---------------------------------------------------------------------------

func (a *App) loadSettings() {
	raw, err := os.ReadFile(a.settingsPath)
	if err != nil {
		return
	}
	var s Settings
	if json.Unmarshal(raw, &s) != nil {
		return
	}
	if s.Mode != ModeServer && s.Mode != ModeClient {
		s.Mode = ModeServer
	}
	if s.ClientName == "" {
		s.ClientName = hostnameOr("client")
	}
	a.settings = s
}

func (a *App) saveSettingsLocked() {
	raw, err := json.MarshalIndent(a.settings, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(a.settingsPath, raw, 0o644)
}

// GetSettings returns the persisted GUI state.
func (a *App) GetSettings() Settings {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings
}

// SetSettings stores the GUI state. Changing the role also stops the
// process of the other role, because a machine runs as a server or as a
// client — never both. The client address may stay empty until the
// client is actually started (start validates it).
func (a *App) SetSettings(s Settings) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if s.Mode != ModeServer && s.Mode != ModeClient {
		return fmt.Errorf("mode must be 'server' or 'client'")
	}
	changed := a.settings.Mode != s.Mode
	a.settings = s
	a.saveSettingsLocked()
	if changed {
		// Switching roles: the old role's instance is no longer wanted.
		if s.Mode == ModeClient {
			a.stopRoleLocked(roleServer)
		} else {
			a.stopRoleLocked(roleClient)
		}
	}
	return nil
}

// StartNotifyWatcher begins watching the server log for client
// connect/disconnect events and raising desktop notifications. Runs in
// the background for the whole GUI lifetime; idempotent.
func (a *App) StartNotifyWatcher() {
	a.notify.run()
}

// ConnectedClients reports how many clients the server currently has
// connected (tracked from the log; 0 when unknown).
func (a *App) ConnectedClients() int {
	return a.notify.connectedCount()
}

// GetPaths reports the resolved file locations.
func (a *App) GetPaths() Paths {
	a.mu.Lock()
	defer a.mu.Unlock()
	return Paths{
		ConfigPath: a.configPath,
		ServerLog:  a.serverLogPath,
		ClientLog:  a.clientLogPath,
		ServerBin:  a.serverPath,
		ClientBin:  a.clientPath,
	}
}
