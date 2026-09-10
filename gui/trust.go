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

// TrustClient adds a machine id to the server config's trusted_ids list
// (persisted; the running server hot-reloads it). Idempotent. Accepts a
// short (8-char) or full id. Deliberately does NOT hold a.mu while
// saving: SaveConfig takes the lock itself, and holding it here would
// deadlock.
func (a *App) TrustClient(id string) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	cfg, err := a.LoadConfig()
	if err != nil {
		return err
	}
	for _, t := range cfg.Network.TrustedIDs {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			return nil // already trusted (full or prefix)
		}
	}
	cfg.Network.TrustedIDs = append(cfg.Network.TrustedIDs, id)
	return a.SaveConfig(cfg)
}

// RevokeClient removes a machine id from the server config's trusted_ids
// (the machine must be trusted by name/layout from now on, like any
// other). Idempotent; accepts the short or full id. Like TrustClient it
// avoids holding a.mu across SaveConfig (which locks it itself).
func (a *App) RevokeClient(id string) error {
	id = strings.TrimSpace(id)
	cfg, err := a.LoadConfig()
	if err != nil {
		return err
	}
	kept := cfg.Network.TrustedIDs[:0]
	for _, t := range cfg.Network.TrustedIDs {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			continue // this entry IS the id (full or prefix) — drop it
		}
		kept = append(kept, t)
	}
	cfg.Network.TrustedIDs = kept
	return a.SaveConfig(cfg)
}

// RevokeServer refuses a server machine id on this machine. It is
// removed from the trusted list AND added to the revoked list — the two
// together mean "never accept this server again, not even by pairing"
// and, critically, "never auto-connect to it, not even because its
// address matches the last connection". If the client is currently
// connected to that server, the session is stopped: revocation should
// take effect now, not at the next restart.
//
// Idempotent; accepts short or full ids.
func (a *App) RevokeServer(id string) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	a.mu.Lock()
	a.settings.TrustedServers = dropID(a.settings.TrustedServers, id)
	if !idRevoked(a.settings.RevokedServers, id) {
		a.settings.RevokedServers = append(a.settings.RevokedServers, id)
	}
	a.saveSettingsLocked()
	a.mu.Unlock()

	// If we are connected to the machine we just revoked, end it now.
	if addr, ok := a.livePeerAddr(id); ok && a.clientTargets(addr) {
		return a.ClientStop()
	}
	return nil
}

// TrustServer adds a server machine id to this machine's trusted-servers
// list (the client accepts connection requests from it). Trusting an id
// is the explicit opposite of revoking it, so a revoked id is cleared —
// otherwise a re-trust would be silently dead (the revoke check wins
// everywhere). Idempotent; accepts short or full ids.
func (a *App) TrustServer(id string) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	a.settings.RevokedServers = dropID(a.settings.RevokedServers, id)
	if !idTrusted(a.settings.TrustedServers, id) {
		a.settings.TrustedServers = append(a.settings.TrustedServers, id)
	}
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
	host := a.settings.ClientAddr
	if i := strings.LastIndex(host, ":"); i > 0 {
		host = host[:i]
	}
	host = strings.TrimSpace(host)
	if host == "" || addr == "" {
		return false
	}
	want := addr
	if i := strings.LastIndex(want, ":"); i > 0 {
		want = want[:i]
	}
	return host == want
}

func portOrDefault(p int) int {
	if p <= 0 || p > 65535 {
		return defaultPort
	}
	return p
}
