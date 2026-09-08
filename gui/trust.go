package main

// Trust and auto-connect: the two sides of "set it up once, connect
// automatically after".
//
//   - Server side — a server operator sees a nearby machine in the
//     discovery list and clicks "trust": its machine id is added to the
//     config's `[network] trusted_ids`, so the allowlist accepts it even
//     before a layout screen is pinned for it. Entries accept the short
//     id (8 chars) or the full 32-char id — matching is by prefix.
//   - Client side — the GUI's settings carry a trusted-servers list and
//     an auto-connect flag. When auto-connect is on and a server whose
//     id is trusted (or whose address matches the last-used server)
//     appears on the network, the client connects by itself.

import (
	"fmt"
	"strings"
	"time"
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

// RevokeServer removes a server machine id from this machine's
// trusted-servers list — its connection requests are refused again
// (unless pairing is enabled). Idempotent; accepts short or full ids.
func (a *App) RevokeServer(id string) error {
	id = strings.TrimSpace(id)
	a.mu.Lock()
	defer a.mu.Unlock()
	kept := a.settings.TrustedServers[:0]
	for _, t := range a.settings.TrustedServers {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			continue
		}
		kept = append(kept, t)
	}
	a.settings.TrustedServers = kept
	a.saveSettingsLocked()
	return nil
}

// TrustServer adds a server machine id to this machine's trusted-servers
// list (the client accepts connection requests from it). Idempotent;
// accepts short or full ids.
func (a *App) TrustServer(id string) error {
	id = strings.TrimSpace(id)
	if len(id) < 4 {
		return fmt.Errorf("machine id looks too short to be real (use the short id shown in the GUI)")
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	for _, t := range a.settings.TrustedServers {
		if idTrusted([]string{t}, id) || idTrusted([]string{id}, t) {
			return nil
		}
	}
	a.settings.TrustedServers = append(a.settings.TrustedServers, id)
	a.saveSettingsLocked()
	return nil
}

// autoConnectBlocked reports whether auto-connect must stay quiet right
// now. It is checked at the top of each tick AND again immediately
// before connecting, because the decision goes stale fast: the user can
// switch modes or start a role while the peer scan runs. In particular
// a RUNNING SERVER blocks auto-connect: connecting as a client would
// stop it (one role per machine), so "switch to server, click share"
// could otherwise end with the fresh server killed and the machine
// reconnecting as a client a moment later — the user's explicit choice
// must always win over convenience.
func (a *App) autoConnectBlocked() bool {
	s := a.GetSettings()
	if s.Mode != ModeClient || !s.AutoConnect {
		return true
	}
	// A role is running locally — a client (already connected) or a
	// server (explicitly shared). Auto-connecting over either would
	// fight the user.
	return a.ClientRunning() || a.ServerRunning()
}

// AutoConnectLoop watches discovery: when auto-connect is on, a client
// that is not running connects to a trusted server (or the last used
// server) as soon as it appears. Cheap: it only acts on a *transition*
// (server newly seen), and it never fights the user — see
// autoConnectBlocked.
func (a *App) AutoConnectLoop() {
	go func() {
		var lastSeen map[string]bool // peer id -> present
		ticker := time.NewTicker(2 * time.Second)
		defer ticker.Stop()
		for range ticker.C {
			if a.autoConnectBlocked() {
				lastSeen = nil
				continue
			}
			s := a.GetSettings()
			peers := a.DiscoverPeers()
			now := map[string]bool{}
			for _, p := range peers {
				if p.Role != "server" {
					continue
				}
				now[p.ID] = true
				if lastSeen[p.ID] {
					continue // seen before; don't re-trigger
				}
				if idTrusted(s.TrustedServers, p.ID) || a.peerMatchesClientAddr(p) {
					// The scan takes time; re-validate so a mode switch
					// or an explicit start that happened meanwhile wins.
					if a.autoConnectBlocked() {
						break
					}
					addr := fmt.Sprintf("%s:%d", p.Addr, portOrDefault(p.Port))
					_ = a.ConnectToServer(addr)
					break
				}
			}
			lastSeen = now
		}
	}()
}

// peerMatchesClientAddr reports whether a discovered server matches the
// address the user last connected to (same host, any port).
func (a *App) peerMatchesClientAddr(p Peer) bool {
	addr := strings.TrimSpace(a.GetSettings().ClientAddr)
	host := addr
	if i := strings.LastIndex(host, ":"); i > 0 {
		host = host[:i]
	}
	return host != "" && host == p.Addr
}

func portOrDefault(p int) int {
	if p <= 0 || p > 65535 {
		return defaultPort
	}
	return p
}