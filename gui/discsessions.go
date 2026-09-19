package main

import "kvmshare/gui/internal/discovery"

// discsessions.go — the App's side of the discovery duty cycle.
//
// The engine no longer runs free; the App tells it when discovery is a
// live question and when it is over:
//
//   - a role starts → Seek: this machine is on the market (a server
//     wants clients, a client wants its server);
//   - a session attaches (a client connects, a client lands on this
//     server) → SetConnected: stay visible to the partner at the low
//     connected rate, and nothing more;
//   - the session drops while the role still runs → Seek again (the
//     question is live once more);
//   - the role stops with no counterpart to stay visible to → SetIdle:
//     every loop ends now, and the UI says the engine is idle instead
//     of pretending to listen.
//
// All four are called from the transitions that matter — role starts
// and stops, and the state loop's session-attachment changes — so the
// engine's traffic always has a reason to exist and a moment it ends.

// seekDiscovery starts (or extends) a seeking session. Safe to call
// from any goroutine; a nil engine (tests) is a no-op.
func (a *App) seekDiscovery() {
	if a.disc != nil {
		a.disc.Seek()
	}
}

// connectedDiscovery switches the engine to the connected session rate.
func (a *App) connectedDiscovery() {
	if a.disc != nil {
		a.disc.SetConnected()
	}
}

// idleDiscovery stops every discovery session.
func (a *App) idleDiscovery() {
	if a.disc != nil {
		a.disc.SetIdle()
	}
}

// discoverySessionSync is called once per state-loop tick: it maps the
// machine's actual session state onto the engine's duty cycle. Reading
// the cheap local sources and calling the engine's setters costs
// nothing (the engine dedupes its own transitions), and it means every
// state change — a client dropping, a client landing, a role stop — is
// reflected within one tick no matter which code path caused it.
func (a *App) discoverySessionSync() {
	a.mu.Lock()
	server := a.serverProc.running() || a.roleActive(roleServer)
	client := a.clientProc.running() || a.roleActive(roleClient)
	a.mu.Unlock()

	switch {
	case client && a.reconciledClientState(client).Status == "connected":
		a.connectedDiscovery()
	case client || server:
		a.seekDiscovery()
	default:
		// No role running: the engine owes nobody packets. (Pairing
		// invites are still answered — the listener is process-scoped —
		// but nothing is broadcast or probed.)
		a.idleDiscovery()
	}
}

// DiscoveryStatus exposes the duty-cycle state to the frontend: what
// the engine is doing (idle/seeking/connected), how many consecutive
// sessions ended without hearing anyone, and how long the current
// session has left. The UI renders this verbatim — "discovery stops
// when it fails" is something the user can see.
func (a *App) DiscoveryStatus() discovery.Status {
	if a.disc == nil {
		return discovery.Status{State: discovery.SessionIdle.String()}
	}
	return a.disc.Status()
}
