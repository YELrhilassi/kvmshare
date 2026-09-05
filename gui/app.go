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
	"runtime"
	"sync"
	"time"
)

// Mode is what this machine does in the KVM.
type Mode string

const (
	ModeServer Mode = "server"
	ModeClient Mode = "client"
)

// Settings is the GUI's own persisted state (role + client connection +
// the operator's logging preferences).
type Settings struct {
	Mode       Mode   `json:"mode"`
	ClientAddr string `json:"clientAddr"` // host:port of the server to connect to
	ClientName string `json:"clientName"` // screen name used on the server
	LogLevel   string `json:"logLevel"`   // error|warn|info|debug|trace
	LogEnabled bool   `json:"logEnabled"` // false silences the role's log entirely
}

// LogSettings is what the Logs page shows and edits: the logging
// configuration for this machine's instance (one role runs at a time, so
// one level applies to whichever role is active).
type LogSettings struct {
	Role    string `json:"role"` // the active role: "server" or "client"
	Level   string `json:"level"`
	Enabled bool   `json:"enabled"`
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
		serverPath = lookPathElse("kvmshare-server", filepath.Join(dir, binName("kvmshare-server", runtime.GOOS)))
	}
	clientPath := firstNonEmpty(os.Getenv("KVMSHARE_CLIENT"))
	if clientPath == "" {
		clientPath = lookPathElse("kvmshare-client", filepath.Join(dir, binName("kvmshare-client", runtime.GOOS)))
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
			LogLevel:   "info",
			LogEnabled: true,
		},
	}
	a.loadSettings()
	a.notify = newNotify(a.serverLogPath)
	return a
}

// SingleInstance takes the GUI's instance lock — only one GUI per machine
// may manage processes. Returns whether another instance was running and
// has been asked to come forward (raised, err == nil), so the caller can
// exit quietly: from dmenu or a launcher there is no terminal to show an
// error, so "already running" must *show the window*, not die silently.
// The flock releases automatically when this process dies — no stale
// state after a crash.
func (a *App) SingleInstance() (raised bool, err error) {
	f, err := os.OpenFile(a.instanceLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return false, fmt.Errorf("open instance lock: %w", err)
	}
	if err := tryLockFile(f); err != nil {
		f.Close()
		// Someone else holds the lock. Ask that instance (its pid is
		// recorded in the pid file) to show itself; give it a moment to
		// write the pid in case it is still starting up.
		for i := 0; i < 10; i++ {
			if pid := a.pidFromLock("gui"); pid > 0 {
				if raiseInstance(pid) == nil {
					return true, nil
				}
				break
			}
			time.Sleep(50 * time.Millisecond)
		}
		return false, fmt.Errorf("kvmshare is already running on this machine")
	}
	// We hold the lock: record our pid in the dedicated pid file so a
	// later launch can raise us (the lock file itself is lock-only — its
	// byte-range lock on Windows blocks reads by other handles).
	_ = os.WriteFile(a.rolePidPath("gui"), []byte(fmt.Sprintf("%d\n", os.Getpid())), 0o644)
	a.instanceLock = f
	return false, nil
}

func lookPathElse(name, fallback string) string {
	if p, err := exec.LookPath(name); err == nil {
		return p
	}
	return fallback
}

// binName returns the executable file name for `base` on `goos`:
// Windows binaries carry .exe, elsewhere they are bare. Used for the
// "next to the GUI" fallback, so an installed Windows GUI finds the
// role binaries installed beside it in %LOCALAPPDATA%\kvmshare.
func binName(base, goos string) string {
	if goos == "windows" {
		return base + ".exe"
	}
	return base
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
	// Presence check: a pre-update gui.json has no log fields, and the
	// zero value for a bool is *disabled* — so distinguish "absent"
	// (default logging ON) from an explicit `logEnabled: false`.
	var present map[string]json.RawMessage
	_ = json.Unmarshal(raw, &present)
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
	if !validLogLevel(s.LogLevel) {
		s.LogLevel = "info"
	}
	if _, ok := present["logEnabled"]; !ok {
		s.LogEnabled = true // logging defaults to ON
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
	if s.LogLevel == "" {
		s.LogLevel = "info" // omitted (e.g. older callers) → default
	}
	if !validLogLevel(s.LogLevel) {
		return fmt.Errorf("unknown log level %q (use error, warn, info, debug or trace)", s.LogLevel)
	}
	a.settings = s
	a.saveSettingsLocked()
	// The level/enabled the user picked must apply to the running
	// instance (hot reload) and to whichever role starts next.
	a.writeLogCtlLocked(roleServer)
	a.writeLogCtlLocked(roleClient)
	// Changing mode is a *selection*, not a command to stop anything:
	// the role currently running on this machine keeps running until the
	// user starts the other one. Starting a role stops the opposite role
	// first (ServerStart / ClientStart) — cleanup belongs at the moment
	// it matters, so toggling never silently kills a working session.
	if s.Mode == ModeServer {
		// Becoming the server means input isolation applies when it
		// starts: make sure the system grant exists (silent once
		// granted, and it never prompts when access already works).
		a.ensureInputAccess()
	}
	return nil
}

// GetLogSettings returns the operator's logging configuration plus the
// role it applies to (the active role — one instance per machine).
func (a *App) GetLogSettings() LogSettings {
	a.mu.Lock()
	defer a.mu.Unlock()
	return LogSettings{
		Role:    string(a.settings.Mode),
		Level:   a.settings.LogLevel,
		Enabled: a.settings.LogEnabled,
	}
}

// SetLogSettings stores the level/enabled choice and applies it live:
// the control files are re-written, and the running role process picks
// the change up within a poll interval — no restart. The inactive role's
// control file is written too, so the setting holds whichever role
// starts next (a machine is one role at a time).
func (a *App) SetLogSettings(s LogSettings) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !validLogLevel(s.Level) {
		return fmt.Errorf("unknown log level %q (use error, warn, info, debug or trace)", s.Level)
	}
	a.settings.LogLevel = s.Level
	a.settings.LogEnabled = s.Enabled
	a.saveSettingsLocked()
	a.writeLogCtlLocked(roleServer)
	a.writeLogCtlLocked(roleClient)
	return nil
}

func validLogLevel(l string) bool {
	switch l {
	case "error", "warn", "info", "debug", "trace":
		return true
	}
	return false
}

// writeLogCtlLocked writes the role's log-control file, which the Rust
// process polls and hot-applies (level + enabled). Written atomically
// (tmp + rename) so a crash never leaves a torn file. Callers hold a.mu.
func (a *App) writeLogCtlLocked(role string) {
	content := fmt.Sprintf("level=%s\nenabled=%d\n", a.settings.LogLevel, boolInt(a.settings.LogEnabled))
	path := filepath.Join(a.stateDir, role+".logctl")
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(content), 0o644); err != nil {
		return
	}
	_ = os.Rename(tmp, path)
}

func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

// ClearLog empties the given role's log file ("server" or "client"). The
// role process keeps appending from the new offset — appends are atomic,
// so a running process cannot resurrect cleared lines.
func (a *App) ClearLog(role string) error {
	a.mu.Lock()
	var path string
	switch role {
	case roleServer:
		path = a.serverLogPath
	case roleClient:
		path = a.clientLogPath
	default:
		a.mu.Unlock()
		return fmt.Errorf("unknown role %q", role)
	}
	a.mu.Unlock()
	if err := os.Truncate(path, 0); err != nil && !os.IsNotExist(err) {
		return err
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
