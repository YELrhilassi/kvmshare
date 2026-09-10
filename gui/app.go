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
//   app.go      — App state and construction
//   paths.go    — binary/config/state file resolution, machine naming
//   settings.go — persisted GUI settings (gui.json) + log settings
//   instance.go — single-instance lock and raise
//   config.go   — the layout config (kvmshare-server.toml)
//   process.go  — child process plumbing (spawn, reaper, stop)
//   roles.go    — role lifecycle (start/stop/adopt, auto-restart)
//   rolelock.go — role lock/pid files, background-instance detection
//   netlog.go   — network interfaces and log tailing
//   live.go     — the pushed live-state event stream
//
// All long-lived processes log to ~/.local/state/kvmshare/ so the GUI can
// tail them live.

package main

import (
	"kvmshare/gui/internal/notify"
	"os"
	"path/filepath"
	"sync"

	"github.com/wailsapp/wails/v3/pkg/application"

	"kvmshare/gui/internal/discovery"
)

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

	mu         sync.Mutex
	settings   Settings
	serverProc *proc
	clientProc *proc
	// When the current "connecting" run began (see ClientStatus).
	// Guarded internally: written from both the state loop and the
	// Wails bridge goroutines.
	connectingSince connectingClock

	// Lifecycle notifications (client connect/disconnect from the server
	// log). nil until StartNotifyWatcher is called.
	notify *notify.Watcher
	// Network discovery engine (beacons + probes + mDNS + pairing).
	disc *discovery.Service

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
// in ~/.local/state/kvmshare/ (XDG_STATE_HOME default). File resolution
// itself lives in paths.go (NewAppPaths).
func NewApp() *App {
	stateDir, configPath, serverPath, clientPath, installPath := NewAppPaths()

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
	a.notify = notify.New(a.serverLogPath)
	a.disc = discovery.New(a)
	return a
}

// StartNotifyWatcher begins watching the server log for client
// connect/disconnect events and raising desktop notifications. Runs in
// the background for the whole GUI lifetime; idempotent.
func (a *App) StartNotifyWatcher() {
	a.notify.Run()
}

// ConnectedClients reports how many clients the server currently has
// connected (tracked from the log; 0 when unknown).
func (a *App) ConnectedClients() int {
	return a.notify.ConnectedCount()
}
