// kvmshare-installer — the GUI installer, a Wails v3 window on both
// Windows and Linux with the same face everywhere. The heavy lifting
// (download, checksum, install, desktop integration) lives in
// internal/installer, shared with the CLI `kvmshare-install`; this app
// is only the window: a service bound to a single embedded page.
//
// No console anywhere: on Windows the binary is built with -H windowsgui,
// on Linux it is launched from a .desktop entry or by hand.
package main

import (
	"embed"
	"log"
	"log/slog"

	"github.com/wailsapp/wails/v3/pkg/application"
)

//go:embed all:index.html
var assets embed.FS

//go:embed assets/icon.png
var windowIcon []byte

func main() {
	core := NewInstaller()

	logger := slog.New(slog.NewTextHandler(log.Default().Writer(), &slog.HandlerOptions{Level: slog.LevelWarn}))

	app := application.New(application.Options{
		Name:        "kvmshare installer",
		Description: "Install or update kvmshare.",
		Services: []application.Service{
			application.NewService(core),
		},
		Assets: application.AssetOptions{
			Handler: application.BundledAssetFileServer(assets),
		},
		Icon:     windowIcon,
		Logger:   logger,
		LogLevel: slog.LevelWarn,
	})

	app.Window.NewWithOptions(application.WebviewWindowOptions{
		Name:             "installer",
		Title:            "kvmshare installer",
		Width:            480,
		Height:           640,
		MinWidth:         420,
		MinHeight:        560,
		MaxWidth:         560,
		MaxHeight:        760,
		BackgroundColour: application.NewRGBA(10, 10, 12, 255),
	})

	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
}
