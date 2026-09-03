// System tray: live role status plus Start/Stop/Open/Quit while the
// window is hidden. The GUI keeps running in the tray so a machine's
// KVM role can be watched and controlled without keeping a window open.
package main

import (
	_ "embed"
	"fmt"
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

// trayHostAvailable reports whether a system tray actually exists on this
// desktop: a session bus AND an org.kde.StatusNotifierWatcher (provided by
// KDE/GNOME panels, or a standalone SNI host). Checked once at startup —
// the result does not change for the life of the process. Close-to-tray
// only makes sense when there is a tray to hide into; without one, closing
// the window quits the GUI (roles are independent background processes).
func trayHostAvailable() bool {
	// Windows and macOS always have a system tray; only Linux needs the
	// StatusNotifierWatcher probe (KDE/GNOME panels, or a standalone SNI
	// host). Checked once at startup — the result does not change for the
	// life of the process.
	if runtime.GOOS != "linux" {
		return true
	}
	trayOnce.Do(func() {
		conn, err := dbus.SessionBus()
		if err != nil {
			return // no session bus — no tray
		}
		defer conn.Close()
		var owner string
		if err := conn.Object("org.freedesktop.DBus", "/org/freedesktop/DBus").
			Call("org.freedesktop.DBus.GetNameOwner", 0, "org.kde.StatusNotifierWatcher").
			Store(&owner); err == nil && owner != "" {
			trayAvailable = true
		}
	})
	return trayAvailable
}

var (
	trayOnce      sync.Once
	trayAvailable bool
)

// tray owns the tray menu items so their labels and enabled state can
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
	s := t.core.GetSettings()
	role, running := ModeServer, false
	switch s.Mode {
	case ModeClient:
		role, running = ModeClient, t.core.ClientRunning()
	default:
		role, running = ModeServer, t.core.ServerRunning()
	}

	t.mu.Lock()
	defer t.mu.Unlock()
	key := fmt.Sprintf("%s/%t", role, running)
	if key == t.lastKey {
		return
	}
	t.lastKey = key

	roleTitle := "Server"
	if role == ModeClient {
		roleTitle = "Client"
	}
	stateText := "stopped"
	if running {
		stateText = "running"
		if role == ModeServer {
			if n := t.core.ConnectedClients(); n > 0 {
				s := "client"
				if n > 1 {
					s = "clients"
				}
				stateText = fmt.Sprintf("running · %d %s", n, s)
			}
		}
	}
	verb := strings.ToLower(roleTitle)

	t.state.SetLabel(fmt.Sprintf("%s · %s", roleTitle, stateText))
	t.start.SetLabel("Start " + verb)
	t.stop.SetLabel("Stop " + verb)
	t.start.SetEnabled(!running)
	t.stop.SetEnabled(running)
	t.sys.SetTooltip(fmt.Sprintf("kvmshare — %s %s", roleTitle, stateText))
}
