package main

// Connected clients, from the server's perspective. The Rust server
// writes `clients.json` in the state dir on every connect/disconnect
// (see the server binary's event sink), so this GUI reads a small file
// instead of parsing logs or speaking the wire protocol.
//
// Per-client commands (disconnect / reconnect / restart) travel the
// opposite way: the GUI writes a line to `server.cmd`, the server polls
// the file and turns each line into a `Control::ClientCommand` on its
// main loop, which sends the wire `Control` message to that client.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// ConnectedClient is one entry of the server's `clients.json`.
type ConnectedClient struct {
	Name    string `json:"name"`
	ID      string `json:"id"`
	Addr    string `json:"addr"`
	SinceMs int64  `json:"sinceMs"`
}

// ListClients returns the server's currently connected clients (read
// from `clients.json`; empty when the server is not running or has none).
//
// The file is written by the server and never removed when the server
// dies, so it outlives the process that produced it — reading it blind
// made the GUI claim "connected to you" with no server running (the
// same lie the client's stale state file used to tell, reconciled the
// same way: a live server role lock is the gate for trusting the file).
// Always returns a non-nil slice so the frontend can safely iterate it.
func (a *App) ListClients() []ConnectedClient {
	if !a.roleActive(roleServer) {
		return []ConnectedClient{} // no server running: the file is a leftover
	}
	path := filepath.Join(a.stateDir, "clients.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		return []ConnectedClient{} // no server yet, or none connected
	}
	var out []ConnectedClient
	if json.Unmarshal(raw, &out) != nil {
		return []ConnectedClient{}
	}
	return out
}

// ClientCommand sends one operational command to a connected client by
// screen name. The command is written to `server.cmd`; the server picks
// it up within its poll interval. The client's behavior:
//
//   - disconnect — ends its session and stays stopped (must be started
//     again)
//   - reconnect   — ends its session and reconnects immediately
//   - restart     — same as reconnect (a fresh session)
func (a *App) ClientCommand(name, action string) error {
	switch action {
	case "disconnect", "reconnect", "restart":
	default:
		return fmt.Errorf("unknown client command %q (use disconnect, reconnect or restart)", action)
	}
	if strings.TrimSpace(name) == "" {
		return fmt.Errorf("client name is required")
	}
	line := action + " " + strings.TrimSpace(name) + "\n"
	path := filepath.Join(a.stateDir, "server.cmd")
	// Append: the server truncates the file after reading, so commands
	// can be queued while it processes earlier ones.
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.WriteString(line)
	return err
}