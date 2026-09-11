package main

// discovery_host.go — the App's implementation of the discovery
// engine's Host interface: the facts this machine advertises, and the
// one policy decision the engine cannot make itself (honoring a
// pairing request under the trust rules).
//
// Keeping this adapter separate from app.go keeps the directions
// clean: the engine never imports the App type, the App never touches
// sockets, and this file is the only place the two vocabularies meet.

import (
	"net"

	"kvmshare/gui/internal/discovery"
	"kvmshare/gui/internal/ids"
)

// MachineID implements discovery.Host.
func (a *App) MachineID() string { return a.GetMachineId() }

// MachineName implements discovery.Host: the friendly name advertised
// on the network (beacons, pairing requests, mDNS).
func (a *App) MachineName() string { return a.displayName() }

// ServerPort implements discovery.Host.
func (a *App) ServerPort() int { return a.serverPort() }

// LANAddr implements discovery.Host: this machine's first private IPv4
// address, included in pairing requests so the client knows where to
// reach the server.
func (a *App) LANAddr() string { return a.primaryLANAddr() }

// AdvertisedRole implements discovery.Host: what is *actually running
// here*, not what the GUI's mode dropdown says. The dropdown is an
// intention; the beacon is a fact. Advertising the dropdown made a
// machine running a server but set to "client" (the common case right
// after a role switch) broadcast "client, not running" — every other
// machine then rendered it as an idle ghost. Server wins the
// (unsupported, but defensive) both-running case.
func (a *App) AdvertisedRole() (string, bool) {
	switch {
	case a.roleActive(roleServer):
		return "server", true
	case a.roleActive(roleClient):
		return "client", true
	default:
		s := a.GetSettings()
		return string(s.Mode), false
	}
}

// OnPairRequest implements discovery.Host: the trust policy for
// "connect here" requests. Trust on first use — honor the request when
// the server is trusted OR pairing is enabled; when honored, remember
// the server's id so the next request (and auto-connect) already
// trusts it.
//
// A revoked id is refused outright, before the pairing toggle is even
// consulted: revocation must mean "no", not "no unless the convenience
// door happens to be open".
func (a *App) OnPairRequest(req discovery.PairRequest) {
	if idRevoked(a.GetSettings().RevokedServers, req.ID) {
		return
	}
	if !ids.Trusted(a.GetSettings().TrustedServers, req.ID) && !a.settingsPairingEnabled() {
		return
	}
	if !ids.Trusted(a.GetSettings().TrustedServers, req.ID) {
		_ = a.TrustServer(req.ID, true)
	}
	_ = a.ConnectToServer(req.Addr)
}

// settingsPairingEnabled reports the pairing toggle (client accepts
// connection requests from any local server on first use).
func (a *App) settingsPairingEnabled() bool {
	return a.GetSettings().AcceptPairing
}

// serverPort returns the port the server listens on (from the config),
// falling back to the default.
func (a *App) serverPort() int {
	cfg, err := a.LoadConfig()
	if err != nil || cfg.Port == 0 {
		return defaultPort
	}
	return cfg.Port
}

// primaryLANAddr returns this machine's first private IPv4 address,
// or the loopback fallback when nothing private is up (single-machine
// dev setups).
func (a *App) primaryLANAddr() string {
	ifaces, _ := a.ListInterfaces()
	for _, i := range ifaces {
		for _, addr := range i.Addrs {
			ip := net.ParseIP(addr)
			if ip == nil {
				continue
			}
			if v4 := ip.To4(); v4 != nil && v4.IsPrivate() {
				return v4.String()
			}
		}
	}
	return "127.0.0.1"
}

// shortID is the human-facing form of a machine id, re-exported from
// the ids package for the rest of package main.
func shortID(id string) string { return ids.ShortID(id) }

// idTrusted is the shared trust matcher, re-exported from the ids
// package for the rest of package main.
func idTrusted(trusted []string, id string) bool { return ids.Trusted(trusted, id) }
