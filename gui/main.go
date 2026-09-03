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

	// Only one GUI per machine: a second instance exits with an error
	// instead of fighting over processes and the role locks.
	if err := core.SingleInstance(); err != nil {
		log.Fatalf("kvmshare: %v", err)
	}

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

	// Closing the window hides it to the tray — roles keep running and
	// the app stays reachable for status and control.
	window.RegisterHook(events.Common.WindowClosing, func(e *application.WindowEvent) {
		window.Hide()
		e.Cancel()
	})

	setupTray(app, core, window)
	core.StartNotifyWatcher()

	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
}
