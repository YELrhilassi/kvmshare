package main

// Trust, revocation and the trust matchers shared by pairing and
// auto-connect (the auto-connect policy itself lives in autoconnect.go).
//
//   - Server side — a server operator sees a nearby machine in the
//     discovery list and clicks "trust": its machine id is added to the
//     config's `[network] trusted_ids`, so the allowlist accepts it even
//     before a layout screen is pinned for it. Entries accept the short
//     id (8 chars) or the full 32-char id — matching is by prefix.
//   - Client side — machine ids the operator accepts requests from
//     (TrustedServers) and ids they explicitly refused (RevokedServers).
//     Revocation is sticky and outranks every convenience path.

import (
	"fmt"
	"net"
	"strconv"
	"strings"

	"kvmshare/gui/internal/ids"
)

// TrustClient sets whether a machine id is trusted on this server
// (persisted; the running server hot-reloads policy changes live).
// `false` removes the entry. Idempotent, accepts a short (8-char) or full
// id. Trust and revocation are independent: trusting an id does NOT clear
// a revocation — the two lists can name the same machine, and a revoked
// id is always refused.
//
// Deliberately does NOT hold a.mu while saving: SaveConfig takes the lock
// itself, and holding it here would deadlock.
func (a *App) TrustClient(id string, trusted bool) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		return err
	}
	cfg.Network.TrustedIDs = setID(cfg.Network.TrustedIDs, id, trusted)
	return a.SaveConfig(cfg)
}

// RevokeClient sets whether a machine id is revoked on this server. A
// revoked id may never connect — the running server refuses it in the
// handshake (before the layout and the trusted list) and drops it
// immediately if it is connected right now. Idempotent; `false`
// un-revokes. Accepts the short or full id. Like TrustClient it avoids
// holding a.mu across SaveConfig (which locks it itself).
func (a *App) RevokeClient(id string, revoked bool) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		return err
	}
	cfg.Network.RevokedIDs = setID(cfg.Network.RevokedIDs, id, revoked)
	return a.SaveConfig(cfg)
}

// setID adds `id` to (on=true) or removes it from (on=false) `list`,
// matching by prefix the same way trust does. Returns a fresh slice so the
// original backing array is never aliased.
func setID(list []string, id string, on bool) []string {
	kept := make([]string, 0, len(list)+1)
	for _, t := range list {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			continue // drop any existing form of this id first
		}
		kept = append(kept, t)
	}
	if on {
		kept = append(kept, id)
	}
	return kept
}

// RevokeServer sets whether a server machine id is revoked on this
// machine. A revoked server is refused outright: no pairing request is
// honored, no auto-connect selects it, and the client itself refuses the
// session (the server's id arrives in `Welcome`, which the client checks
// against the list the GUI passes it).
//
// Trust and revocation are independent — the same id may be in both lists,
// and revoke always wins — so this does not touch the trusted list. If the
// client is currently connected to that server, the session is stopped:
// revocation takes effect now, not at the next restart. Idempotent;
// `false` un-revokes. Accepts short or full ids.
func (a *App) RevokeServer(id string, revoked bool) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	a.mu.Lock()
	a.settings.RevokedServers = setID(a.settings.RevokedServers, id, revoked)
	a.saveSettingsLocked()
	a.mu.Unlock()

	if !revoked {
		return nil // un-revoking never disturbs a running session
	}
	// If we are connected to the machine we just revoked, end it now.
	if addr, ok := a.livePeerAddr(id); ok && a.clientTargets(addr) {
		return a.ClientStop()
	}
	return nil
}

// TrustServer sets whether a server machine id is trusted on this machine
// (the client accepts connection requests from it). Independent of
// revocation, like TrustClient: trusting does not clear a revoke, so the
// two can coexist and revoke still wins. Idempotent; `false` removes the
// entry. Accepts short or full ids.
func (a *App) TrustServer(id string, trusted bool) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	a.settings.TrustedServers = setID(a.settings.TrustedServers, id, trusted)
	a.saveSettingsLocked()
	return nil
}

// idRevoked reports whether id has been explicitly refused. Same prefix
// matching as trust (short or full ids, both directions), so revoking
// the short id also refuses the full one.
func idRevoked(revoked []string, id string) bool {
	return ids.Trusted(revoked, id)
}

// dropID returns `list` without the entry that matches `id` (prefix
// match both ways, like trust). It returns a closed-over new slice so
// the original backing array is never aliased.
func dropID(list []string, id string) []string {
	kept := make([]string, 0, len(list))
	for _, t := range list {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			continue
		}
		kept = append(kept, t)
	}
	return kept
}

// livePeerAddr resolves a discovered peer's address by machine id, so a
// revoke can tell whether the running session belongs to that machine.
func (a *App) livePeerAddr(id string) (string, bool) {
	if a.disc == nil {
		return "", false
	}
	p, ok := a.disc.PeerByID(id)
	if !ok {
		return "", false
	}
	return net.JoinHostPort(p.Addr, strconv.Itoa(portOrDefault(p.Port))), true
}

// clientTargets reports whether the client's configured server address
// points at `addr` (same host, any port).
func (a *App) clientTargets(addr string) bool {
	return addr != "" && sameHost(a.settings.ClientAddr, addr)
}

func portOrDefault(p int) int {
	if p <= 0 || p > 65535 {
		return defaultPort
	}
	return p
}
