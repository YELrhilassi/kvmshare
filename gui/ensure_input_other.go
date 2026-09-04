//go:build !linux

package main

// Input isolation reads /dev/input — a Linux mechanism. No-op elsewhere.
func (a *App) ensureInputAccess() {}
