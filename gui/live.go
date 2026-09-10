package main

// Live state, pushed to the frontend as one event stream.
//
// The page used to re-query the bridge on a 2 s timer from two
// components — a handful of RPC round-trips per second just to render
// "am I running / who is nearby". Instead the GUI now owns the whole
// picture and pushes it: one goroutine re-checks the cheap local
// sources (role locks, the tiny client.state file, the peers map,
// clients.json) once a second and emits a single `kvmshare:state`
// event — but only when the snapshot actually changed. The frontend
// subscribes once and re-renders on real transitions; a quiet network
// costs nothing. After a user action the page may also call refresh()
// once for instant feedback — a response to a click, not polling.

import (
	"encoding/json"
	"time"

	"github.com/wailsapp/wails/v3/pkg/application"

	"kvmshare/gui/internal/discovery"
)

// The one event name the frontend subscribes to.
const stateEventName = "kvmshare:state"

// How often the local sources are re-checked. One second keeps every
// transition (role crash, client connect, peer arrival) feeling live
// while staying trivial — local file reads, no network.
const stateInterval = time.Second

// runningSnapshot mirrors RunningStatus on the wire.
type runningSnapshot struct {
	Server bool `json:"server"`
	Client bool `json:"client"`
}

// LiveSnapshot is the whole live picture the Home dashboard renders.
// A single JSON blob means the page can never show torn state (e.g.
// "connected" from one source while "not running" from another).
// ClientName lets the client status say "connected to pc, as hp".
// Trusted are the machine ids this machine trusts (server config ids
// for a server, trusted-server ids for a client) — the page uses them
// to tell "nearby" machines from "trusted but not running" ones.
type LiveSnapshot struct {
	Mode        Mode              `json:"mode"`
	ClientName  string            `json:"clientName"`
	Running     runningSnapshot   `json:"running"`
	ClientState ClientState       `json:"clientState"`
	Peers       []discovery.Peer  `json:"peers"`
	Clients     []ConnectedClient `json:"clients"`
	Trusted     []string          `json:"trusted"`
}

// snapshot assembles the current picture. Locking is deliberately
// non-reentrant: each source takes what it needs and releases, so this
// can be called from anywhere (including after an action that held
// a.mu) without deadlocking.
func (a *App) snapshot() LiveSnapshot {
	a.mu.Lock()
	mode := a.settings.Mode
	clientName := a.settings.ClientName
	server := a.serverProc.running() || a.roleActive(roleServer)
	client := a.clientProc.running() || a.roleActive(roleClient)
	a.mu.Unlock()

	snap := LiveSnapshot{
		Mode:        mode,
		ClientName:  clientName,
		Running:     runningSnapshot{Server: server, Client: client},
		ClientState: a.reconciledClientState(client),
		Peers:       a.DiscoverPeers(),
		Clients:     a.ListClients(),
	}
	// The ids this machine trusts, matching the peer map by prefix: the
	// server trusts what is in its config, the client what is in its
	// settings (the Rust side uses the same lists).
	if mode == ModeServer {
		if cfg, err := a.LoadConfig(); err == nil {
			snap.Trusted = nonNilStrings(cfg.Network.TrustedIDs)
		}
	} else {
		a.mu.Lock()
		snap.Trusted = nonNilStrings(a.settings.TrustedServers)
		a.mu.Unlock()
	}
	return snap
}

// reconciledClientState makes the client state truthful against the
// process itself, because the state file outlives the process: a killed
// or crashed client leaves a stale "connected" file behind, which used
// to make the Home page claim a connection that did not exist. The
// process is the source of truth:
//
//   - no client process → disconnected, whatever the file says
//   - client running, not (yet) connected → connecting
//   - client running and the file says connected → connected
func (a *App) reconciledClientState(clientRunning bool) ClientState {
	cs := a.ClientStatus()
	if !clientRunning {
		cs.Status = "disconnected"
		return cs
	}
	if cs.Status != "connected" {
		cs.Status = "connecting"
	}
	return cs
}

// emitState pushes the snapshot only when it changed since the last
// emission (see shouldEmit — comparing JSON is exact and cheap at these
// sizes, and it is what makes the whole stream quiet).
func (a *App) emitState() {
	snap := a.snapshot()
	if a.shouldEmit(snap) && a.events != nil {
		a.events.Emit(stateEventName, snap)
	}
}

// shouldEmit reports whether `snap` differs from the last snapshot this
// App emitted, recording it when it does. No change, no event, no
// re-render — a quiet network costs nothing. Exported as a method (not
// a free function) so the dedupe contract is unit-testable.
func (a *App) shouldEmit(snap LiveSnapshot) bool {
	raw, err := json.Marshal(snap)
	if err != nil {
		return false
	}
	a.stateMu.Lock()
	defer a.stateMu.Unlock()
	if string(raw) == a.lastState {
		return false
	}
	a.lastState = string(raw)
	return true
}

// stateLoop runs the single re-check loop. Started once from main.go
// after the application exists (idempotent via stateOnce).
func (a *App) stateLoop() {
	a.stateOnce.Do(func() {
		go func() {
			// Seed the page immediately; the first event then arrives
			// within one interval for anything that changes after.
			a.emitState()
			ticker := time.NewTicker(stateInterval)
			defer ticker.Stop()
			for range ticker.C {
				a.emitState()
			}
		}()
	})
}

// attachEvents hands the service the Wails event manager, so emitState
// can reach the frontend. Called from main.go once the application is
// built (before then, emission is a no-op).
func (a *App) attachEvents(em *application.EventManager) {
	a.events = em
}
