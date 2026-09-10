package main

// peers_api.go — the Wails-bound surface for the discovery engine: the
// live peer list, the manual refresh behind the UI's refresh button,
// and "connect here" from the server's Home page.

import (
	"fmt"

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
// interfaces re-enumerated, announce + probe sent immediately. Backs
// the UI refresh button. Returns the fresh peer list so the click gives
// instant feedback even before the next pushed state event.
func (a *App) RefreshDiscovery() ([]discovery.Peer, error) {
	if a.disc == nil {
		return nil, fmt.Errorf("discovery not started")
	}
	a.disc.Refresh()
	return a.disc.List(), nil
}

// SendConnectRequest asks a discovered client (identified by its full
// or short id) to connect to this machine's server. Used from the
// server's Home page: pick a nearby machine, tell it to connect here.
func (a *App) SendConnectRequest(peerID string) error {
	if a.disc == nil {
		return fmt.Errorf("discovery not started")
	}
	return a.disc.SendConnectRequest(peerID)
}

// StartDiscovery advertises this machine and browses for peers. Runs
// for the whole GUI lifetime; a role switch re-advertises under the
// new role.
func (a *App) StartDiscovery() {
	a.disc.Start()
}

// ReAdvertise re-publishes the mDNS record under the current role.
func (a *App) ReAdvertise() {
	if a.disc != nil {
		a.disc.Republish()
	}
}
