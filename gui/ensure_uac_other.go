//go:build !windows

package main

// UAC is a Windows mechanism — no-op elsewhere.
func (a *App) ensureUacAnswerable() {}
