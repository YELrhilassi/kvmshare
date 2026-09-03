// The kvmshare GUI: a desktop layout editor.
//
// The frontend (plain HTML/CSS/JS, no build step) shows the virtual
// desktop as draggable screens. The backend (this process) loads and
// saves the server config file and manages the kvmshare-server process,
// so the whole KVM is operated from one window.
package main

import (
	"embed"
	"log"

	"github.com/wailsapp/wails/v2"
	"github.com/wailsapp/wails/v2/pkg/options"
	"github.com/wailsapp/wails/v2/pkg/options/assetserver"
)

//go:embed all:frontend/dist
var assets embed.FS

func main() {
	app := NewApp()

	err := wails.Run(&options.App{
		Title:  "kvmshare",
		Width:  1080,
		Height: 720,
		AssetServer: &assetserver.Options{
			Assets: assets,
		},
		Bind: []interface{}{
			app,
		},
	})
	if err != nil {
		log.Fatal(err)
	}
}
