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
	"strings"
	"sync"
	"time"

	"github.com/wailsapp/wails/v3/pkg/application"
)

// Mode is what this machine does in the KVM.
type Mode string

const (
	ModeServer Mode = "server"
	ModeClient Mode = "client"
)

// Settings is the GUI's own persisted state (role + client connection +
// the operator's logging preferences + discovery/trust settings).
type Settings struct {
	Mode       Mode   `json:"mode"`
	ClientAddr string `json:"clientAddr"` // host:port of the server to connect to
	ClientName string `json:"clientName"` // screen name used on the server
	LogLevel   string `json:"logLevel"`   // error|warn|info|debug|trace
	LogEnabled bool   `json:"logEnabled"` // false silences the role's log entirely

	// TrustedServers is the list of server machine ids this machine
	// accepts connection requests from (discovery pairing). Empty means
	// "no server may command this machine to connect".
	TrustedServers []string `json:"trustedServers"`
	// AcceptPairing lets any local kvmshare server request a connection
	// (a convenience with a trust trade-off; off by default).
	AcceptPairing bool `json:"acceptPairing"`
	// AutoConnect makes the client automatically connect to the last
	// used server when it appears on the network (discovery).
	AutoConnect bool `json:"autoConnect"`
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
	installPath   string
	settingsPath  string
	serverLogPath string
	clientLogPath string

	// Instance lock: only one GUI may manage processes per machine.
	instanceLockPath string
	instanceLock     *os.File

	mu              sync.Mutex
	settings        Settings
	serverProc      *proc
	clientProc      *proc
	// When the current "connecting" run began (see ClientStatus).
	// Only meaningful while the client process is actually running.
	connectingSince time.Time

	// Lifecycle notifications (client connect/disconnect from the server
	// log). nil until StartNotifyWatcher is called.
	notify *notify
	// Network discovery (mDNS advertise + browse + pairing listener).
	disc *discovery

	// Live-state events: the Wails event manager (attached once the
	// application exists), the last snapshot JSON emitted (dedupe), and
	// the loop's once-guard.
	events    *application.EventManager
	stateMu   sync.Mutex
	lastState string
	stateOnce sync.Once
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

	// An explicit KVMSHARE_CONFIG wins as-is (the operator owns it).
	// Otherwise the canonical per-user location is used. The GUI does
	// **not** pre-create this file: the server owns its layout config
	// and creates a machine-accurate default itself on first start (this
	// machine's real name + display, no invented clients), so the GUI and
	// the server can never disagree about who wrote it first. The Layout
	// page simply shows a placeholder view until that file exists.
	configPath := firstNonEmpty(os.Getenv("KVMSHARE_CONFIG"))
	if configPath == "" {
		if home != "" {
			configPath = filepath.Join(home, ".config", "kvmshare", "kvmshare-server.toml")
		} else {
			// No home at all (rare): fall back next to the executable.
			configPath = filepath.Join(dir, "kvmshare-server.toml")
		}
	}

	serverPath := firstNonEmpty(os.Getenv("KVMSHARE_SERVER"))
	if serverPath == "" {
		serverPath = lookPathElse("kvmshare-server", filepath.Join(dir, binName("kvmshare-server", runtime.GOOS)))
	}
	clientPath := firstNonEmpty(os.Getenv("KVMSHARE_CLIENT"))
	if clientPath == "" {
		clientPath = lookPathElse("kvmshare-client", filepath.Join(dir, binName("kvmshare-client", runtime.GOOS)))
	}
	// The standalone installer/bootstrap: kept current by the updater so
	// the portable update path never lags behind the GUI's.
	installPath := firstNonEmpty(os.Getenv("KVMSHARE_INSTALL"))
	if installPath == "" {
		installPath = lookPathElse("kvmshare-install", filepath.Join(dir, binName("kvmshare-install", runtime.GOOS)))
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
		installPath:      installPath,
		settingsPath:     filepath.Join(stateDir, "gui.json"),
		serverLogPath:    filepath.Join(stateDir, "server.log"),
		clientLogPath:    filepath.Join(stateDir, "client.log"),
		instanceLockPath: filepath.Join(stateDir, "gui.lock"),
		settings: Settings{
			Mode:          ModeServer,
			LogLevel:      "info",
			LogEnabled:    true,
			AcceptPairing: true,
		},
	}
	// The generated machine name needs the machine id, which is created
	// lazily on first use — so it is filled here, after the App exists
	// (loadSettings keeps the name when the user has chosen one).
	a.settings.ClientName = a.defaultClientName()
	a.loadSettings()
	a.notify = newNotify(a.serverLogPath)
	a.disc = newDiscovery(a)
	return a
}

// SingleInstance takes the GUI's instance lock — only one GUI per machine
// may manage processes. Returns whether another instance was running and
// has been asked to come forward (raised, err == nil), so the caller can
// exit quietly: from dmenu or a launcher there is no terminal to show an
// error, so "already running" must *show the window*, not die silently.
// The flock releases automatically when this process dies — no stale
// state after a crash.
// restartHandoff is the second half of an in-place update (see
// update.go): the freshly-updated process was spawned by the old one,
// which is still alive and still holds the instance lock. Wait for that
// lock to be released instead of raising the old instance — it is on its
// way out and will never come forward. Bounded: if the lock is still
// held after the grace period, something else is genuinely running, and
// the caller falls through to the normal "already running" handling.
func (a *App) restartHandoff() {
	if os.Getenv("KVMSHARE_RESTART") == "" {
		return
	}
	deadline := time.Now().Add(6 * time.Second)
	for time.Now().Before(deadline) {
		f, err := os.OpenFile(a.instanceLockPath, os.O_CREATE|os.O_RDWR, 0o644)
		if err == nil {
			if tryLockFile(f) == nil {
				unlockFile(f) // probe only — the caller takes the lock
				f.Close()
				break
			}
			f.Close()
		}
		time.Sleep(100 * time.Millisecond)
	}
	// The handoff env must not leak into processes this instance spawns
	// later (a role, or a future restart of its own).
	_ = os.Unsetenv("KVMSHARE_RESTART")
}

func (a *App) SingleInstance() (raised bool, err error) {
	a.restartHandoff()
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
				if raiseInstance(pid, a.raiseScope()) == nil {
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
	// Sibling first: the GUI and its role binaries ship (and are
	// upgraded) together in one install directory. Searching PATH first
	// let a stale copy from an older install silently win over the
	// freshly deployed one — the launcher must never mix versions.
	if sib, err := os.Executable(); err == nil {
		sibling := filepath.Join(filepath.Dir(sib), binName(name, runtime.GOOS))
		if st, err := os.Stat(sibling); err == nil && !st.IsDir() {
			return sibling
		}
	}
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

// machineName is this machine's default friendly name: the real host
// name plus a short random suffix derived from the stable machine id
// ("bliss-8f3a"). The suffix keeps two machines with the same host
// name distinct on the network, and being derived from the persisted id
// it never changes between launches. Users can override it on the
// Client page (the "name on the server"); nothing else in the product
// invents "pc"/"hp"-style defaults.
func machineName(host, machineID string) string {
	h := strings.TrimSpace(host)
	if h == "" {
		h = "machine"
	}
	suffix := shortID(machineID)
	if len(suffix) > 4 {
		suffix = suffix[:4]
	}
	return h + "-" + suffix
}

// defaultClientName returns the name this machine presents to servers
// when the user has not chosen one yet.
func (a *App) defaultClientName() string {
	return machineName(hostnameOr("machine"), a.GetMachineId())
}

// displayName is the friendly name advertised on the network (beacons,
// pairing requests, mDNS): the user-chosen name when there is one,
// otherwise the generated machine name.
func (a *App) displayName() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	if n := strings.TrimSpace(a.settings.ClientName); n != "" {
		return n
	}
	return a.defaultClientName()
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
		s.ClientName = a.defaultClientName()
	}
	if !validLogLevel(s.LogLevel) {
		s.LogLevel = "info"
	}
	if _, ok := present["logEnabled"]; !ok {
		s.LogEnabled = true // logging defaults to ON
	}
	// Pairing defaults to ON: trust-on-first-use means a discovered
	// server's request is accepted once and remembered, so "connect
	// here" works out of the box. The toggle exists for stricter setups.
	if _, ok := present["acceptPairing"]; !ok {
		s.AcceptPairing = true
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

// ConnectToServer points the client at `addr` (host:port) and starts it.
// Used by discovery pairing: a trusted server asked this machine to
// connect. Idempotent when a client is already running.
func (a *App) ConnectToServer(addr string) error {
	a.mu.Lock()
	next := a.settings
	next.ClientAddr = addr
	a.settings = next
	a.saveSettingsLocked()
	a.mu.Unlock()
	_, err := a.ClientStart()
	return err
}

// SetSettings stores the GUI state. Changing the role also stops the
// process of the other role, because a machine runs as a server or as a
// client — never both. The client address may stay empty until the
// client is actually started (start validates it).
func (a *App) SetSettings(s Settings) error {
	a.mu.Lock()
	if s.Mode != ModeServer && s.Mode != ModeClient {
		a.mu.Unlock()
		return fmt.Errorf("mode must be 'server' or 'client'")
	}
	if s.LogLevel == "" {
		s.LogLevel = "info" // omitted (e.g. older callers) → default
	}
	if !validLogLevel(s.LogLevel) {
		a.mu.Unlock()
		return fmt.Errorf("unknown log level %q (use error, warn, info, debug or trace)", s.LogLevel)
	}
	a.settings = s
	a.saveSettingsLocked()
	// The level/enabled the user picked must apply to the running
	// instance (hot reload) and to whichever role starts next.
	a.writeLogCtlLocked(roleServer)
	a.writeLogCtlLocked(roleClient)
	a.mu.Unlock()

	// Everything below is done WITHOUT a.mu held: re-publishing the
	// network advertisement reads the settings again (it takes the lock
	// itself), and the input-access grant runs in the background.
	// Holding the lock across these used to deadlock — SetSettings is
	// the one path that reaches the advertisement from inside a.mu.
	// A role switch changes what this machine advertises on the network
	// (server vs client) — re-publish so nearby machines see the truth.
	a.ReAdvertise()
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

// StartDiscovery advertises this machine and browses for peers. Runs for
// the whole GUI lifetime; a role switch re-advertises under the new role.
func (a *App) StartDiscovery() {
	a.disc.start()
}

// ReAdvertise re-publishes the mDNS record under the current role.
func (a *App) ReAdvertise() {
	if a.disc != nil {
		a.disc.republish()
	}
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
