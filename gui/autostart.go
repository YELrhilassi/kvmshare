package main

// autostart.go — "launch at startup" as one cross-platform contract:
// EnableLaunchAtStartup / DisableLaunchAtStartup / LaunchAtStartupEnabled.
// Platform files implement it with the mechanism users of that platform
// already expect (XDG autostart on Linux, the per-user Run key on
// Windows) — no init-system or desktop assumptions beyond what those
// standards provide.

import (
	"fmt"
	"os"
	"path/filepath"
)

// selfExe is this GUI's absolute path (symlinks resolved).
func selfExe() (string, error) {
	exe, err := os.Executable()
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(exe)
}

// EnableLaunchAtStartup makes the GUI start when the user logs in.
func (a *App) EnableLaunchAtStartup() error {
	exe, err := selfExe()
	if err != nil {
		return fmt.Errorf("locate self: %w", err)
	}
	if err := enableAutostart(exe); err != nil {
		return err
	}
	a.mu.Lock()
	a.settings.LaunchAtStartup = true
	a.saveSettingsLocked()
	a.mu.Unlock()
	return nil
}

// DisableLaunchAtStartup removes the startup entry.
func (a *App) DisableLaunchAtStartup() error {
	err := disableAutostart()
	a.mu.Lock()
	a.settings.LaunchAtStartup = false
	a.saveSettingsLocked()
	a.mu.Unlock()
	return err
}

// LaunchAtStartupEnabled reports whether a startup entry currently
// exists for this GUI (the file/registry is the truth — the settings
// flag mirrors it, never overrides it).
func (a *App) LaunchAtStartupEnabled() bool {
	if !autostartPresent() {
		return false
	}
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings.LaunchAtStartup
}
