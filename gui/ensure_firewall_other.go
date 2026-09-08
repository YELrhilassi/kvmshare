//go:build !windows

package main

// Windows Firewall is a Windows mechanism — no-op elsewhere. Linux
// systems typically accept inbound UDP without per-app rules, and the
// installer handles any privileged steps there.
func (a *App) ensureFirewall() {}