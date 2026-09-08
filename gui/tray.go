// System tray: live role status plus Start/Stop/Open/Quit while the
// window is hidden. The GUI keeps running in the tray so a machine's
// KVM role can be watched and controlled without keeping a window open.
//
// Two backends, picked once at startup:
//   - SNI (org.kde.StatusNotifierItem) — Wails' built-in tray, used when
//     a StatusNotifierWatcher is on the session bus (GNOME/KDE/waybar).
//   - XEmbed — a native X11 tray icon implemented directly on Xlib (no
//     GTK: Wails links GTK4 and a GTK3 GtkStatusIcon would collide two
//     GType systems in one process), with its own X11 popup menu (Open /
//     Start-Stop / Restart / Quit). For bars that only speak the legacy
//     protocol (i3bar, xfce4-panel, trayer, stalonetray). See
//     tray_xembed_linux.go.
//
// If neither host exists there is nowhere to put an icon, so the GUI
// quits instead of hiding to a tray that does not exist (see main.go's
// window-close hook).
package main

import (
	_ "embed"
	"context"
	"fmt"
	"log/slog"
	"runtime"
	"strings"
	"sync"
	"time"

	"github.com/godbus/dbus/v5"
	"github.com/wailsapp/wails/v3/pkg/application"
	"github.com/wailsapp/wails/v3/pkg/icons"
)

//go:embed assets/tray.png
var trayIcon []byte

// trayBackend names how the tray is actually being served.
type trayBackend int

const (
	trayNone  trayBackend = iota // no tray host on this desktop
	traySNI                      // org.kde.StatusNotifierItem (Wails)
	trayXEmbed                   // legacy XEmbed via raw Xlib (X11 only)
)

var (
	trayOnce      sync.Once
	trayAvailable bool
	trayKind      trayBackend
)

// detectTrayHost decides, once, which tray backend this desktop supports:
// SNI when a StatusNotifierWatcher owns a name on the session bus, else
// XEmbed when an XEmbed tray manager owns the _NET_SYSTEM_TRAY selection.
// Safe to call from any goroutine: the SNI probe uses a bounded dbus
// call and the XEmbed probe opens its own short-lived X connection.
func detectTrayHost(logger *slog.Logger) {
	trayOnce.Do(func() {
		if logger != nil {
			logger.Debug("tray: probing SNI watcher")
		}
		if runtime.GOOS != "linux" {
			trayAvailable, trayKind = true, traySNI
			return
		}
		if sniWatcherPresent() {
			if logger != nil {
				logger.Debug("tray: SNI watcher found")
			}
			trayAvailable, trayKind = true, traySNI
			return
		}
		if logger != nil {
			logger.Debug("tray: no SNI watcher — probing XEmbed manager")
		}
		if xembedManagerPresent() {
			if logger != nil {
				logger.Debug("tray: XEmbed manager found")
			}
			trayAvailable, trayKind = true, trayXEmbed
			return
		}
		if logger != nil {
			logger.Debug("tray: no XEmbed manager either")
		}
		trayAvailable, trayKind = false, trayNone
	})
}

// trayHostAvailable reports whether any tray host exists on this desktop.
func trayHostAvailable() bool {
	detectTrayHost(nil)
	return trayAvailable
}

// sniWatcherPresent reports whether org.kde.StatusNotifierWatcher is on
// the session bus. The probe must never hang the tray setup: godbus has
// no default call timeout, so the context bounds it.
func sniWatcherPresent() bool {
	conn, err := dbus.SessionBus()
	if err != nil {
		return false // no session bus — no tray
	}
	defer conn.Close()
	ctx, cancel := context.WithTimeout(context.Background(), trayProbeTimeout)
	defer cancel()
	var owner string
	err = conn.Object("org.freedesktop.DBus", "/org/freedesktop/DBus").
		CallWithContext(ctx, "org.freedesktop.DBus.GetNameOwner", 0, "org.kde.StatusNotifierWatcher").
		Store(&owner)
	return err == nil && owner != ""
}

// trayProbeTimeout bounds the SNI watcher probe.
const trayProbeTimeout = 500 * time.Millisecond

// trayState is the live role status both tray backends render.
type trayState struct {
	role    Mode   // ModeServer | ModeClient
	running bool
	detail  string // "" | " · N clients" for a running server
}

// computeTrayState reads the current role state (shared by both backends).
func computeTrayState(core *App) trayState {
	s := core.GetSettings()
	switch s.Mode {
	case ModeClient:
		return trayState{role: ModeClient, running: core.ClientRunning()}
	default:
		st := trayState{role: ModeServer, running: core.ServerRunning()}
		if st.running {
			if n := core.ConnectedClients(); n > 0 {
				noun := "client"
				if n > 1 {
					noun = "clients"
				}
				st.detail = fmt.Sprintf(" · %d %s", n, noun)
			}
		}
		return st
	}
}

// tray owns the SNI tray menu items so their labels and enabled state can
// track the current role and whether it is running. The menu is updated
// only when that state actually changes — no needless DBus churn.
type tray struct {
	sys   *application.SystemTray
	core  *App
	win   *application.WebviewWindow
	state *application.MenuItem
	start *application.MenuItem
	stop  *application.MenuItem

	mu      sync.Mutex
	lastKey string // "server/running" etc. of the last rendered state
}

// setupTray creates the tray icon and menu, and starts the status
// refresher. `core` and `win` must be valid for the app's lifetime.
func setupTray(app *application.App, core *App, win *application.WebviewWindow) {
	detectTrayHost(app.Logger)

	switch trayKind {
	case trayXEmbed:
		app.Logger.Info("tray: using XEmbed fallback (legacy tray manager)")
		setupXEmbedTray(app, core, win)
		return
	case trayNone:
		// Nowhere to put an icon: skip tray creation entirely so Wails
		// never attempts an SNI registration that can only fail (the
		// "failed to register" log noise). The app still runs — closing
		// the window then quits-and-stops (see main.go).
		app.Logger.Info("tray: no tray host on this desktop; tray disabled")
		return
	}

	systemTray := app.SystemTray.New()
	if runtime.GOOS == "darwin" {
		systemTray.SetTemplateIcon(icons.SystrayMacTemplate)
	} else {
		systemTray.SetIcon(trayIcon)
	}

	t := &tray{sys: systemTray, core: core, win: win}

	menu := app.NewMenu()
	menu.Add("kvmshare").SetEnabled(false)
	t.state = menu.Add("…").SetEnabled(false)
	menu.AddSeparator()
	menu.Add("Open kvmshare").OnClick(func(*application.Context) {
		win.Show()
		win.Focus()
	})
	t.start = menu.Add("Start").OnClick(func(*application.Context) {
		_, _ = core.StartActive()
		t.refresh()
	})
	t.stop = menu.Add("Stop").OnClick(func(*application.Context) {
		_ = core.StopActive()
		t.refresh()
	})
	menu.AddSeparator()
	menu.Add("Quit").OnClick(func(*application.Context) {
		// Quitting must not strand the role: a running server or client
		// would keep sharing input with the other machine (and, on
		// Windows, keep the elevated client's input gate on) with no
		// tray left to control it. Stop everything first; on a stubborn
		// process, quit anyway — a background role is better than a GUI
		// that refuses to leave. A failed stop is logged, not silently
		// dropped: the next launch then shows what was left behind.
		if err := core.StopAll(); err != nil {
			app.Logger.Warn("tray: quit — could not stop every role process, quitting anyway", "err", err)
		}
		app.Quit()
	})
	systemTray.SetMenu(menu)

	t.refresh()

	// Keep the tray in sync with state changes that happen anywhere —
	// this window, a previous GUI instance, or a binary started by hand.
	go func() {
		ticker := time.NewTicker(time.Second)
		defer ticker.Stop()
		for range ticker.C {
			t.refresh()
		}
	}()
}

// refresh reads the current role state and updates the menu if it
// changed. Cheap when nothing changed (state is compared, not pushed).
func (t *tray) refresh() {
	st := computeTrayState(t.core)

	roleTitle := "Server"
	if st.role == ModeClient {
		roleTitle = "Client"
	}
	stateText := "stopped"
	if st.running {
		stateText = "running" + st.detail
	}

	t.mu.Lock()
	defer t.mu.Unlock()
	key := roleTitle + "/" + stateText
	if key == t.lastKey {
		return
	}
	t.lastKey = key

	verb := strings.ToLower(roleTitle)

	t.state.SetLabel(fmt.Sprintf("%s · %s", roleTitle, stateText))
	t.start.SetLabel("Start " + verb)
	t.stop.SetLabel("Stop " + verb)
	t.start.SetEnabled(!st.running)
	t.stop.SetEnabled(st.running)
	t.sys.SetTooltip(fmt.Sprintf("kvmshare — %s %s", roleTitle, stateText))
}
