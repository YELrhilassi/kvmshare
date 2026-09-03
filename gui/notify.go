package main

// Lifecycle notifications: the server writes stable markers to its log —
// "kvmshare-server: client X connected" / "... disconnected" — and this
// watcher turns them into desktop notifications. It tails the log file,
// keeps the set of connected clients (so the tray can show a live count)
// and notifies only on real transitions. Notifications go over DBus
// (org.freedesktop.Notifications), the standard channel on Linux; a
// missing notification daemon is silently ignored.

import (
	"fmt"
	"os"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/godbus/dbus/v5"
)

// clientEventRe matches the server's connect/disconnect markers. The
// marker text lives in the Rust server; keep both sides in sync. Not
// anchored: lines may carry a timestamp/level prefix from the server's
// leveled logger.
var clientEventRe = regexp.MustCompile(`client (\S+) (connected|disconnected)`)

// How often the log is re-read. Cheap (offset-based), so once per second
// is fine and keeps notifications snappy.
const notifyPoll = time.Second

type notify struct {
	mu      sync.Mutex
	logPath string

	clients map[string]bool // screen name -> connected?
	offset  int64           // bytes already consumed in the log
	primed  bool            // first pass builds state without notifying
	started sync.Once
	stop    chan struct{}

	// fire delivers one notification; replaced in tests.
	fire func(name string, connected bool)
}

func newNotify(logPath string) *notify {
	return &notify{
		logPath: logPath,
		clients: map[string]bool{},
		stop:    make(chan struct{}),
		fire:    notifyDesktop,
	}
}

// run starts the watcher loop (one goroutine, safe to call once).
func (n *notify) run() {
	n.started.Do(func() {
		go n.loop()
	})
}

// connectedCount returns how many clients the server currently has.
func (n *notify) connectedCount() int {
	n.mu.Lock()
	defer n.mu.Unlock()
	count := 0
	for _, up := range n.clients {
		if up {
			count++
		}
	}
	return count
}

func (n *notify) loop() {
	ticker := time.NewTicker(notifyPoll)
	defer ticker.Stop()
	for {
		select {
		case <-n.stop:
			return
		case <-ticker.C:
			n.poll()
		}
	}
}

// poll reads the new tail of the log and processes client transitions.
func (n *notify) poll() {
	f, err := os.Open(n.logPath)
	if err != nil {
		return // no log yet (role never started)
	}
	defer f.Close()

	st, err := f.Stat()
	if err != nil {
		return
	}
	n.mu.Lock()
	defer n.mu.Unlock()
	if st.Size() < n.offset {
		n.offset = 0 // log was truncated/replaced — re-prime
		n.clients = map[string]bool{}
		n.primed = false
	}
	if _, err := f.Seek(n.offset, 0); err != nil {
		return
	}
	buf := make([]byte, st.Size()-n.offset)
	read, err := f.Read(buf)
	if err != nil && read == 0 {
		return
	}
	n.offset += int64(read)

	for _, line := range strings.Split(string(buf[:read]), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		m := clientEventRe.FindStringSubmatch(line)
		if m == nil {
			continue
		}
		name, up := m[1], m[2] == "connected"
		if n.clients[name] == up {
			continue // no change (duplicate marker, re-read tail)
		}
		n.clients[name] = up
		if !n.primed {
			continue // building initial state — no notifications yet
		}
		go n.fire(name, up)
	}
	n.primed = true
}

// notifyDesktop raises one notification via org.freedesktop.Notifications.
// Runs in its own goroutine: a slow/hung bus must never block the watcher.
func notifyDesktop(name string, connected bool) {
	conn, err := dbus.SessionBus()
	if err != nil {
		return // no session bus / no daemon — notifications are best-effort
	}
	defer conn.Close()
	obj := conn.Object("org.freedesktop.Notifications", "/org/freedesktop/Notifications")
	summary := fmt.Sprintf("Client connected: %s", name)
	if !connected {
		summary = fmt.Sprintf("Client disconnected: %s", name)
	}
	body := "The shared desktop layout now has its clients updated."
	if connected {
		body = "The machine is reachable and sharing input."
	}
	_ = obj.Call("org.freedesktop.Notifications.Notify", 0,
		"kvmshare",                // app name
		uint32(0),                 // replaces id
		"kvmshare",                // icon name
		summary,                   // summary
		body,                      // body
		[]string{},                // actions
		map[string]dbus.Variant{}, // hints
		int32(-1),                 // timeout: daemon default
	)
}
