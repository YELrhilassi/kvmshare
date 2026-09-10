package main

// The client's real connection state. The Rust client writes
// `<state_dir>/client.state` on every transition (connecting →
// connected → disconnected), so this GUI can distinguish "the client
// process is running" from "the client is actually connected to the
// server". The frontend shows the real state; a running-but-not-connected
// process no longer reads as "Connected".

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"
)

// How long the client may stay in "connecting" before the UI calls it
// what it is: the server is unreachable, and the client keeps retrying.
const connectingGrace = 8 * time.Second

// ClientState is what the Home page's connection panel shows.
type ClientState struct {
	Status string `json:"status"` // "connected" | "connecting" | "disconnected"
	Server string `json:"server"` // address the client talks to
	// ConnectingSinceMs is when the current "connecting" run began
	// (unix ms, 0 when not connecting). The page uses it to say
	// "can't reach the server" instead of a forever-"Connecting…".
	ConnectingSinceMs int64 `json:"connectingSinceMs"`
}

// connectingClock remembers when the current "connecting" run began.
// It is written from two goroutines — the state loop and the Wails
// bridge (ClientStatus is a bound method) — so it carries its own
// mutex; a plain field here was a data race the race detector never
// saw only because the two writers happened to alternate.
type connectingClock struct {
	mu    sync.Mutex
	since time.Time
}

// mark records the transition into "connecting", returning when this
// run began (now, when the transition is fresh).
func (c *connectingClock) mark(connecting bool) time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	if connecting {
		if c.since.IsZero() {
			c.since = time.Now()
		}
		return c.since
	}
	c.since = time.Time{}
	return time.Time{}
}

// ClientStatus reads the client's live state file. Missing or unreadable
// means "not connected" (no client has written anything yet).
func (a *App) ClientStatus() ClientState {
	path := filepath.Join(a.stateDir, "client.state")
	raw, err := os.ReadFile(path)
	if err != nil {
		a.connectingSince.mark(false)
		return ClientState{Status: "disconnected", Server: ""}
	}
	st := ClientState{Status: "disconnected"}
	for _, line := range strings.Split(string(raw), "\n") {
		kv := strings.SplitN(strings.TrimSpace(line), "=", 2)
		if len(kv) != 2 {
			continue
		}
		switch kv[0] {
		case "status":
			if kv[1] == "connected" || kv[1] == "connecting" || kv[1] == "disconnected" {
				st.Status = kv[1]
			}
		case "server":
			st.Server = kv[1]
		}
	}
	// Remember when the client started waiting, so "connecting" can
	// age into "unreachable". Only the process state says whether it
	// is really connecting; the file alone cannot tell a stale
	// "connecting" from a live one (reconciledClientState fixes that
	// before this reaches the page).
	st.ConnectingSinceMs = a.connectingSince.mark(st.Status == "connecting").UnixMilli()
	return st
}
