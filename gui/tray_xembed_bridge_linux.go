//go:build linux

package main

// Pure-Go half of the XEmbed tray: the wiring between the cgo icon engine
// (tray_xembed_linux.go) and the app — menu actions, live role state the
// C menu reads on open, the //export callbacks the C thread calls, and
// the icon asset decoder. No cgo here.

import (
	"bytes"
	"image"
	"image/png"
	"sync"
	"time"

	"github.com/wailsapp/wails/v3/pkg/application"
)

func setupXEmbedTray(app *application.App, core *App, win *application.WebviewWindow) {
	icon, err := decodeTrayIcon()
	if err != nil {
		app.Logger.Info("tray: XEmbed fallback unavailable — bad icon asset")
		return
	}

	xembedAct = xembedActions{
		open: func() {
			app.Logger.Info("tray: menu — open window")
			win.Show()
			win.Focus()
		},
		start: func() {
			app.Logger.Info("tray: menu — start role")
			if _, err := core.StartActive(); err != nil {
				app.Logger.Error("tray: menu — start failed", "err", err)
			}
		},
		stop: func() {
			app.Logger.Info("tray: menu — stop role")
			if err := core.StopActive(); err != nil {
				app.Logger.Error("tray: menu — stop failed", "err", err)
			}
		},
		restart: func() {
			// StopActive waits for the role lock to be released, so the
			// start that follows cannot collide with the old instance.
			app.Logger.Info("tray: menu — restart role")
			if err := core.StopActive(); err != nil {
				app.Logger.Error("tray: menu — restart stop failed", "err", err)
			}
			if _, err := core.StartActive(); err != nil {
				app.Logger.Error("tray: menu — restart start failed", "err", err)
			}
		},
		quit: func() {
			// Quitting must not strand the role: a running server or
			// client would keep sharing input with the other machine (and,
			// on Windows, keep the elevated client's input gate on) with
			// no tray left to control it.
			app.Logger.Info("tray: menu — quit")
			if err := core.StopAll(); err != nil {
				app.Logger.Warn("tray: quit — could not stop every role process, quitting anyway", "err", err)
			}
			app.Quit()
		},
	}

	// Seed the menu labels, then keep them current for the tray's life.
	// The C thread reads them via kvmTrayState each time the menu opens.
	refreshXEmbedState(core)
	go func() {
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for range ticker.C {
			refreshXEmbedState(core)
		}
	}() // The C thread parks in its event loop for the process lifetime; the
	// launcher itself lives in the cgo file (it crosses the C boundary).
	launchTrayIconThread(app, icon)
}

// launchTrayIconThread is implemented in tray_xembed_linux.go: it hands
// the decoded RGBA to the cgo engine and parks a goroutine on the icon
// thread for the process lifetime.
// that the C menu reads (via kvmTrayState).
func refreshXEmbedState(core *App) {
	st := computeTrayState(core)
	role := "server"
	if st.role == ModeClient {
		role = "client"
	}
	setXEmbedState(role, st.running)
}

// xembedActions are the popup menu's actions, wired once in
// setupXEmbedTray before the icon thread starts and read from the C
// thread through the //export callbacks below. Assigned-before-start, so
// no locking is needed.
type xembedActions struct {
	open    func()
	start   func()
	stop    func()
	restart func()
	quit    func()
}

var xembedAct xembedActions

// xembedState is the role status the C menu renders. It is written by
// the refresh ticker (Go) and read by the C thread via kvmTrayState when
// the menu opens — the mutex keeps the two safe.
type xembedState struct {
	mu      sync.Mutex
	role    string // "server" | "client"
	running bool
}

var xembedSt xembedState

func setXEmbedState(role string, running bool) {
	xembedSt.mu.Lock()
	xembedSt.role = role
	xembedSt.running = running
	xembedSt.mu.Unlock()
}

func getXEmbedState() (string, bool) {
	xembedSt.mu.Lock()
	defer xembedSt.mu.Unlock()
	return xembedSt.role, xembedSt.running
}

// trayPixels is a decoded icon as raw RGBA.
type trayPixels struct {
	rgba []byte
	w, h int
}

// decodeTrayIcon decodes the embedded tray.png to RGBA.
func decodeTrayIcon() (trayPixels, error) {
	img, err := png.Decode(bytes.NewReader(trayIcon))
	if err != nil {
		return trayPixels{}, err
	}
	b := img.Bounds()
	w, h := b.Dx(), b.Dy()
	rgba := make([]byte, w*h*4)
	// image/png decodes to *image.NRGBA or *image.RGBA; both store 8-bit
	// channels in this layout, but NRGBA is non-premultiplied which is
	// what the C side expects.
	switch src := img.(type) {
	case *image.NRGBA:
		copy(rgba, src.Pix)
	default:
		// Convert any other model through NRGBA.
		for y := 0; y < h; y++ {
			for x := 0; x < w; x++ {
				r, g, bb, a := img.At(b.Min.X+x, b.Min.Y+y).RGBA()
				i := (y*w + x) * 4
				rgba[i] = byte(r >> 8)
				rgba[i+1] = byte(g >> 8)
				rgba[i+2] = byte(bb >> 8)
				rgba[i+3] = byte(a >> 8)
			}
		}
	}
	return trayPixels{rgba: rgba, w: w, h: h}, nil
}
