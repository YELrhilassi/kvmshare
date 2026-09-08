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
)

// ClientState is what the Home page's connection panel shows.
type ClientState struct {
	Status string `json:"status"` // "connected" | "connecting" | "disconnected"
	Server string `json:"server"` // address the client talks to
}

// ClientStatus reads the client's live state file. Missing or unreadable
// means "not connected" (no client has written anything yet).
func (a *App) ClientStatus() ClientState {
	path := filepath.Join(a.stateDir, "client.state")
	raw, err := os.ReadFile(path)
	if err != nil {
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
	return st
}