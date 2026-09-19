package main

// peers_api.go — the Wails-bound surface for the discovery engine: the
// live peer list, the manual refresh behind the UI's refresh button,
// the direct address probe, and "connect here" from the server's Home
// page.

import (
	"fmt"
	"net"
	"strconv"
	"strings"

	"kvmshare/gui/internal/discovery"
)

// DiscoverPeers exposes the live peer list to the frontend. Always
// returns a non-nil slice so the frontend can safely iterate it.
func (a *App) DiscoverPeers() []discovery.Peer {
	if a.disc == nil {
		return []discovery.Peer{}
	}
	return a.disc.List()
}

// RefreshDiscovery forces a full discovery sweep: state cleared,
// interfaces re-enumerated, several announce+probe rounds sent now.
// Backs the UI refresh button. Returns the fresh peer list so the click
// gives instant feedback, and an error when the sweep could not run —
// the UI shows it instead of a silent no-op (the old refresh skipped
// its probe silently when the listener socket was down, which is
// exactly when a user presses it).
func (a *App) RefreshDiscovery() ([]discovery.Peer, error) {
	if a.disc == nil {
		return nil, fmt.Errorf("discovery not started")
	}
	if err := a.disc.Refresh(); err != nil {
		return a.disc.List(), err
	}
	return a.disc.List(), nil
}

// ProbeHost asks one address directly "are you a kvmshare machine?"
// (host:port, port defaulting to the discovery port). The manual path
// of last resort: it works whenever IP reachability works — no
// broadcast, no multicast, no cached state. Returns the peer as
// announced, or an error naming the address and why it did not answer.
func (a *App) ProbeHost(addr string) (discovery.Peer, error) {
	if a.disc == nil {
		return discovery.Peer{}, fmt.Errorf("discovery not started")
	}
	host := strings.TrimSpace(addr)
	if host == "" {
		return discovery.Peer{}, fmt.Errorf("enter an address to probe")
	}
	if !strings.Contains(host, ":") {
		host = net.JoinHostPort(host, strconv.Itoa(discovery.Port))
	}
	return a.disc.Probe(host)
}

// SendConnectRequest asks a discovered client (identified by its full
// or short id) to connect to this machine's server. Used from the
// server's Home page: pick a nearby machine, tell it to connect here.
//
// The request is a UDP datagram: fire-and-forget by nature. When this
// machine is not running a server, the client would dutifully try to
// connect, time out against a port nobody listens on, and retry forever
// — the “pc server ready / waiting for it to come online” stand-off the
// Connect button used to produce. The button only exists to invite a
// peer to *this* server, so refusing it here turns a silent deadlock
// into an error the operator can act on.
func (a *App) SendConnectRequest(peerID string) error {
	if a.disc == nil {
		return fmt.Errorf("discovery not started")
	}
	if !a.ServerRunning() {
		return fmt.Errorf("start this machine's server first — there is nothing for %s to connect to yet", shortID(peerID))
	}
	return a.disc.SendConnectRequest(peerID)
}

// StartDiscovery performs the engine's process-lifetime wiring (pairing
// worker, listener, session watchdog). It starts no traffic: sessions
// begin when a role starts, a partner connects, or the user acts.
func (a *App) StartDiscovery() {
	a.disc.Start()
}

// ReAdvertise re-publishes the mDNS record under the current role.
func (a *App) ReAdvertise() {
	if a.disc != nil {
		a.disc.Republish()
	}
}
