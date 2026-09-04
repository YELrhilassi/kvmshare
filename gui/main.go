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
package main

import (
	"embed"
	"io/fs"
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

func main() {
	core := NewApp()

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
	stopBus := ensureSessionBus(core.stateDir)
	defer stopBus()

	// Input isolation (Linux server) needs a one-time system grant. The
	// sibling installer handles it silently — at most one privilege
	// prompt, never again after. Runs in the background.
	core.ensureInputAccess()

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

	window := app.Window.NewWithOptions(application.WebviewWindowOptions{
		Name:             "main",
		Title:            "kvmshare",
		Width:            1200,
		Height:           800,
		MinWidth:         880,
		MinHeight:        560,
		URL:              "/",
		Linux:            application.LinuxWindow{Icon: windowIcon},
		BackgroundColour: application.NewRGBA(10, 10, 12, 255),
	})

	// A second launch asks us to come forward (see SingleInstance).
	watchRaiseSignal(func() {
		window.Show()
		window.Focus()
	})

	// Closing the window hides it to the tray when a tray is actually
	// present (roles keep running and the app stays reachable). With no
	// tray host — no session bus, or no StatusNotifierWatcher — hiding
	// would strand the app invisibly (the classic ghost-instance trap),
	// so closing quits the GUI instead. Roles are independent background
	// processes either way: quitting the GUI never stops them, and a
	// later launch adopts them again.
	window.RegisterHook(events.Common.WindowClosing, func(e *application.WindowEvent) {
		if trayHostAvailable() {
			window.Hide()
			e.Cancel()
			return
		}
		// No tray: cancel the default close (which would destroy the
		// window and leave a windowless zombie process) and quit cleanly.
		e.Cancel()
		app.Quit()
	})

	setupTray(app, core, window)
	core.StartNotifyWatcher()

	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
}
