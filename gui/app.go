// Backend methods bound into the frontend.
//
// One GUI runs both roles — this machine is either the KVM server (the
// one whose keyboard/mouse is shared) or a client (a controlled machine).
// The role is a GUI-level setting that decides which process the main
// Start/Stop controls; both processes can still be managed individually
// from their pages.
//
// # The service layer
//
// Wails binds every exported method on the registered service, so the
// whole webview API used to be "whatever is exported on *App" — one
// implicit 67-method surface spread over twenty files. The bound methods
// are now grouped into small, named services by the part of the UI they
// serve (settings, roles, layout, media, discovery, clients, trust, logs,
// update, input). Each service is a struct embedding *App, so its method
// bodies read exactly as they did and can still touch App's state and
// helpers directly; `App` embeds the services, which promotes their
// methods into the bound surface.
//
// Two properties make this safe and worth it:
//
//   - The wire names do not change. Wails builds a method's fully
//     qualified name from the *registered* type (`main.App.<Method>`),
//     and a promoted method keeps its own name — so every service method
//     still binds as `main.App.X` and the frontend is untouched.
//   - The surface is now pinned by a test. bindings_test.go holds the
//     exact bound list and checks the frontend's GoApp contract against
//     it, so a method cannot appear on the wire or go missing from it by
//     accident.
//
// Not every bound method is frontend-facing: the discovery engine's Host
// interface (MachineID, ServerPort, LANAddr, AdvertisedRole, ...) is
// implemented by App itself, so those methods must stay exported and are
// therefore bound too. They are grouped at the bottom of the struct and
// recorded separately in the golden list.
//
// The code is split by responsibility into small files:
//
//   app.go      — App state, the embedded service structs, construction
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

// App is the Wails-bound backend. Its exported methods — its own and
// those promoted from the embedded services below — are what the frontend
// can call (window.go.main.App.*).
type App struct {
	// The bound service layer. Each is a view of *App grouped by the part
	// of the UI it serves; App embeds them so their methods are promoted
	// into the bound surface under their original names. They are wired by
	// wireServices and are never nil on an App built by NewApp.
	*settingsService
	*rolesService
	*layoutService
	*mediaService
	*discoveryService
	*clientsService
	*trustService
	*logsService
	*updateService
	*inputService

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

	// Config cache for the state loop: snapshot() needs the trusted /
	// revoked id lists every tick, and re-reading + re-parsing the TOML
	// every second was measurable CPU for data that almost never
	// changes. The cache is keyed on the file's mtime+size, so an edit
	// (through the GUI or by hand) is picked up on the next tick — the
	// cache can never serve stale policy for longer than one second.
	cfgMu      sync.Mutex
	cfgCached  Config
	cfgHave    bool
	cfgMissing bool // cached "no config file" answer
	cfgKey     string
}

// The services. Each embeds *App, which is what lets a method body keep
// using the shared state and helpers unchanged while its receiver — and
// therefore its home in the service layer — moves. They are separate
// types (not one struct) so the bound surface is readable at a glance:
// the exported methods of *settingsService are the settings API.
type (
	settingsService  struct{ *App } // gui.json, paths, identities, launch-at-startup
	rolesService     struct{ *App } // server/client processes and their status
	layoutService    struct{ *App } // the layout config (screens, port, network)
	mediaService     struct{ *App } // media routing + audio sharing
	discoveryService struct{ *App } // peers, probes, interfaces, discovery status
	clientsService   struct{ *App } // the server's connected-client list
	trustService     struct{ *App } // trust/revoke and outbound connect
	logsService      struct{ *App } // log tailing and per-role log settings
	updateService    struct{ *App } // version and self-update
	inputService     struct{ *App } // shortcut-recording key capture
)

// wireServices points every embedded service at this App. Must run before
// any bound method is called: a promoted method on a nil embedded pointer
// panics. NewApp does it right after the struct literal; tests that build
// an App by hand use the same call via testApp in bindings_test.go.
func (a *App) wireServices() *App {
	a.settingsService = &settingsService{a}
	a.rolesService = &rolesService{a}
	a.layoutService = &layoutService{a}
	a.mediaService = &mediaService{a}
	a.discoveryService = &discoveryService{a}
	a.clientsService = &clientsService{a}
	a.trustService = &trustService{a}
	a.logsService = &logsService{a}
	a.updateService = &updateService{a}
	a.inputService = &inputService{a}
	return a
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
	// The service layer first: everything below can call a bound method.
	a.wireServices()
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
