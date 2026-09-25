// The kvmshare GUI: manage the KVM from one window — or from the tray.
//
// The frontend (React + TypeScript, built by Vite into frontend/dist) is
// embedded in this binary and served by Wails v3. The bound service
// (NewApp) loads and saves the server config, manages the
// kvmshare-server / kvmshare-client processes (which keep running in the
// background when this app is gone), tails their logs and lists network
// interfaces.
//
// Lifecycle: closing the window only hides it. The app keeps running as
// a system-tray item with live status and Start/Stop controls, and a
// machine's role processes are independent of the GUI entirely — quit
// the GUI and they keep running; reopen and the GUI adopts them (the
// role locks in the Rust binaries enforce one instance per role).
//
// Process-shape note (what Task Manager shows): the GUI is one process
// plus the WebView2 runtime's own helper processes (browser-process
// model — network, GPU and renderer helpers on Windows; a WebKit
// network+web process pair on Linux). Those children are the webview
// engine, not extra kvmshare instances, and they live and die with the
// GUI. Killing kvmshare-gui therefore also removes its tray icon — the
// tray item is drawn *by* the GUI process. This is deliberate coupling,
// not a leak: the KVM session (server/client role processes) survives
// the GUI, so a killed GUI never strands a live cross-machine session;
// relaunching the GUI re-adopts the roles and re-creates the icon.
package main

import (
	"embed"
	"fmt"
	"io/fs"
	"kvmshare/gui/internal/selfupdate"
	"kvmshare/gui/internal/sessionbus"
	"log"
	"log/slog"
	"os"

	"github.com/wailsapp/wails/v3/pkg/application"
	"github.com/wailsapp/wails/v3/pkg/events"
)

//go:embed all:frontend/dist
var dist embed.FS

//go:embed assets/icon.png
var windowIcon []byte

// parseLaunchArgs resolves the command line. Only --autostart and
// --version exist: the startup entries pass --autostart so the GUI can
// tell a login-session launch (may start hidden to the tray) from a
// manual one (always shows the window), and --version prints the same
// `<name> <ver> (build <id>)` banner the role binaries print — the
// install check asks every binary in the set, GUI included. Anything
// else is an error — swallowing unknown flags once made
// "kvmshare-gui --version" start a server silently.
func parseLaunchArgs(argv []string) (autostart, version bool, err error) {
	for _, arg := range argv[1:] {
		switch arg {
		case "--autostart":
			autostart = true
		case "--version":
			version = true
		default:
			return false, false, fmt.Errorf("unknown argument %q (only --autostart and --version are supported)", arg)
		}
	}
	return autostart, version, nil
}

func main() {
	autostartLaunch, versionFlag, err := parseLaunchArgs(os.Args)
	if err != nil {
		log.Fatalf("kvmshare: %v", err)
	}
	if versionFlag {
		fmt.Println(selfupdate.VersionBanner("kvmshare-gui"))
		return
	}

	core := NewApp()
	// Startup entries written by older builds lack --autostart; with the
	// distinction live, an entry without it would pop the window on
	// every login. Rewrite once when the operator's setting says the
	// entry should exist.
	core.healAutostartEntry()

	// Only one GUI per machine. A second launch raises the running
	// instance's window and exits quietly — from dmenu or a launcher
	// there is no terminal, so "already running" must never look like
	// "nothing happened".
	raised, err := core.SingleInstance()
	if err != nil {
		log.Fatalf("kvmshare: %v", err)
	}
	if raised {
		return // the running instance is now in front
	}

	// A D-Bus session bus must exist before anything touches D-Bus: the
	// tray, the notify watcher, and the WebKitGTK webview (its child
	// processes inherit this env). Without one, godbus/WebKit autolaunch
	// a fresh private bus per launch, and each private bus grows an
	// immortal dbus-activated stack (portals, at-spi, gvfs, notification
	// daemon) that survives the GUI — the process explosion. This adopts
	// an existing bus or creates exactly one managed one, and stops it
	// again on exit.
	stopBus := sessionbus.Ensure(core.stateDir)
	defer stopBus()

	// Role-elevation tasks left behind by a hard-killed role are swept
	// here (the debug tasks earlier builds registered are removed too).
	core.cleanupElevationTasks()

	// Input isolation (Linux server) needs a one-time system grant. The
	// sibling installer handles it silently — at most one privilege
	// prompt, never again after. Runs in the background.
	core.ensureInputAccess()

	// UAC prompts must be answerable with the shared mouse/keyboard
	// (Windows): the elevated GUI moves them to the normal desktop once,
	// and the uninstall restores the original policy.
	core.ensureUacAnswerable()

	// Discovery beacons + pairing ride a UDP port that Windows Firewall
	// silently drops without an explicit rule (the session port usually
	// earns one when the server first runs). Open both inbound — the
	// elevated GUI can — so "on this network" and "connect here" work.
	core.ensureFirewall()

	assets, err := fs.Sub(dist, "frontend/dist")
	if err != nil {
		log.Fatalf("kvmshare: embedded frontend: %v", err)
	}

	// KVMSHARE_GUI_DEBUG=1 turns on framework-level debug logging (asset
	// requests, binding calls) — useful when diagnosing load failures.
	level := slog.LevelInfo
	if os.Getenv("KVMSHARE_GUI_DEBUG") != "" {
		level = slog.LevelDebug
	}
	logger := slog.New(slog.NewTextHandler(os.Stderr, &slog.HandlerOptions{Level: level}))

	app := application.New(application.Options{
		Name:        "kvmshare",
		Description: "Share one keyboard and mouse across your machines.",
		Services: []application.Service{
			application.NewService(core),
		},
		Assets: application.AssetOptions{
			Handler: application.BundledAssetFileServer(assets),
		},
		// App-level icon: Windows uses this for the window/taskbar icon
		// (the exe also carries the icon as resource ID 3 via the .syso,
		// which Wails tries first).
		Icon:     windowIcon,
		Logger:   logger,
		LogLevel: level,
	})

	// The frontend gets live state as events, not by polling the bridge:
	// hand the service the event manager and start the single re-check
	// loop that emits a snapshot only when something changed.
	core.attachEvents(app.Event)
	core.stateLoop()
	// Self-diagnosis: if this process ever starts burning a whole core
	// (a spin loop), dump every goroutine's stack to the state dir —
	// the artifact that names the culprit when a user only reports
	// "it uses a lot of CPU".
	armDebugWatchdog(core)

	windowOpts := application.WebviewWindowOptions{
		Name:             "main",
		Title:            "kvmshare",
		Width:            1200,
		Height:           800,
		MinWidth:         880,
		MinHeight:        560,
		URL:              "/",
		Linux:            application.LinuxWindow{Icon: windowIcon, WebviewGpuPolicy: application.WebviewGpuPolicyAlways},
		BackgroundColour: application.NewRGBA(10, 10, 12, 255),
	}
	// Start quietly only when this launch came from the login session
	// (the startup entry's --autostart flag) and the operator asked for
	// it — but only when a tray host exists to keep the app reachable;
	// the same guard as close-to-tray, so a hidden start can never
	// strand the app. A manual launch — icon, launcher, terminal —
	// always shows the window.
	if core.StartHiddenToTray(autostartLaunch) {
		windowOpts.Hidden = true
	}
	window := app.Window.NewWithOptions(windowOpts)

	// A second launch asks us to come forward (see SingleInstance). On
	// Windows the ask arrives on a named event scoped to this install.
	watchRaiseSignal(core.raiseScope(), func() {
		window.Show()
		window.Focus()
	})

	// Closing the window hides it to the tray when a tray is actually
	// present (SNI watcher on the bus, or an XEmbed tray manager like
	// i3bar's — see tray.go): roles keep running and the app stays
	// reachable. With no tray host at all, hiding would strand the app
	// invisibly (the classic ghost-instance trap), so closing quits the
	// GUI instead, and quitting stops the roles too (a running role with
	// no tray to control it strands the other machine's cursor and, on
	// Windows, keeps the input gate on).
	window.RegisterHook(events.Common.WindowClosing, func(e *application.WindowEvent) {
		if trayHostAvailable() {
			window.Hide()
			e.Cancel()
			return
		}
		// No tray: cancel the default close (which would destroy the
		// window and leave a windowless zombie process) and quit cleanly.
		e.Cancel()
		if err := core.StopAll(); err != nil {
			app.Logger.Warn("close: could not stop every role process, quitting anyway", "err", err)
		}
		app.Quit()
	})

	// The tray is built once the application is running: the Wails SNI
	// tray requires the app's run loop, and the XEmbed fallback needs
	// the window to exist (its click handler shows it).
	app.Event.OnApplicationEvent(events.Common.ApplicationStarted, func(*application.ApplicationEvent) {
		setupTray(app, core, window)
	})
	core.StartNotifyWatcher()
	// Advertise this machine on the LAN and watch for nearby kvmshare
	// machines (discovery + pairing + auto-connect).
	core.StartDiscovery()
	core.AutoConnectLoop()
	// The saved start role runs after discovery: a client's auto-connect
	// needs the engine up to see its server, and a server's advertisement
	// should replace the idle one immediately.
	core.applyStartRole()

	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
	// The low-level keyboard hook (Windows) must not outlive the GUI:
	// a leaked hook would keep suppressing keystrokes after the process
	// is gone to a user's eye.
	exitHookThread()
}
