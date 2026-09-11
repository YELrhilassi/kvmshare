//go:build windows

package main

// Discovery beacons and pairing ride a UDP socket that Windows Firewall
// often silently drops even when the KVM session port was allowed by the
// server's first-run prompt. Without inbound UDP the GUI never hears
// other kvmshare machines, so discovery shows "nothing on this network"
// and "connect here" requests go unanswered. This registers explicit
// inbound allow rules for both ports at startup — the GUI is elevated,
// the rules are idempotent, and a network that refuses rule creation
// simply falls back to manual addresses.

import (
	"log/slog"

	"kvmshare/gui/internal/discovery"
	"kvmshare/gui/internal/installer"
)

// ensureFirewall opens kvmshare's inbound ports: the KVM session port
// (defaultPort, matching the Rust server's DEFAULT_PORT) and the
// discovery/pairing port (discovery.Port). Idempotent and best-effort:
// failures are logged, never fatal.
func (a *App) ensureFirewall() {
	if err := installer.EnsureFirewall(defaultPort, discovery.Port); err != nil {
		slog.Warn("firewall: could not open inbound ports (manual addresses still work)", "err", err)
		return
	}
	slog.Info("firewall: inbound rules ready", "sessionPort", defaultPort, "discoveryPort", discovery.Port)
}
