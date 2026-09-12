//go:build windows

package main

// Discovery beacons and pairing ride a UDP socket that Windows Firewall
// often silently drops — without inbound UDP the GUI never hears other
// kvmshare machines, so discovery shows "nothing on this network" and
// "connect here" requests go unanswered.
//
// The inbound rules are created **by the installer**, which runs
// elevated once at install/update time. This startup check verifies they
// are still present and self-heals through the elevated installer CLI
// when they are not (a machine restore, a policy wipe) — one UAC
// prompt, only when actually needed. The GUI itself runs as the plain
// user: a privileged GUI cannot autostart (Windows skips elevated
// Run-key entries at logon), which is exactly the bug this split fixes.

import (
	"log/slog"

	"kvmshare/gui/internal/discovery"
	"kvmshare/gui/internal/installer"
)

// ensureFirewall verifies kvmshare's inbound port rules and restores
// them via the elevated installer when missing. Best-effort: failures
// are logged, never fatal (manual addresses still work).
func (a *App) ensureFirewall() {
	if installer.FirewallRulesPresent(defaultPort, discovery.Port) {
		slog.Info("firewall: inbound rules present", "sessionPort", defaultPort, "discoveryPort", discovery.Port)
		return
	}
	slog.Info("firewall: inbound rules missing — restoring via the elevated installer")
	if err := installer.EnsureFirewallViaInstaller(defaultPort, discovery.Port); err != nil {
		slog.Warn("firewall: could not open inbound ports (manual addresses still work)", "err", err)
		return
	}
	slog.Info("firewall: inbound rules ready", "sessionPort", defaultPort, "discoveryPort", discovery.Port)
}
