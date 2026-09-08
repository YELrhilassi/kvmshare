//go:build windows

package main

// Discovery beacons and pairing ride a UDP socket (24801) that Windows
// Firewall often silently drops even when the KVM session port (24800)
// was allowed by the server's first-run prompt. Without inbound UDP the
// GUI never hears other kvmshare machines, so discovery shows "nothing
// on this network" and "connect here" requests go unanswered. This
// registers explicit inbound allow rules for both ports at startup —
// the GUI is elevated, the rules are idempotent, and a network that
// refuses rule creation simply falls back to manual addresses.

import (
	"log/slog"

	"kvmshare/gui/internal/installer"
)

// ensureFirewall opens kvmshare's inbound ports (24800 + 24801).
// Idempotent and best-effort: failures are logged, never fatal.
func (a *App) ensureFirewall() {
	if err := installer.EnsureFirewall(); err != nil {
		slog.Warn("firewall: could not open inbound ports (manual addresses still work)", "err", err)
		return
	}
	slog.Info("firewall: inbound rules ready for session (24800) and discovery (24801)")
}