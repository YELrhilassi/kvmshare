//go:build !windows

package main

// Desktop integration on non-Windows platforms: Linux integration (desktop
// entry, icon, sample config) is handled by the selfupdate package itself,
// so the installer's Windows-only hooks are no-ops here.

// integrateDesktop is a no-op outside Windows.
func integrateDesktop(string) error { return nil }

// removeDesktopIntegration is a no-op outside Windows.
func removeDesktopIntegration(string) error { return nil }

// launchGUI is a no-op outside Windows (Linux installs print instructions
// instead; the GUI is launched from the desktop entry).
func launchGUI(string) error { return nil }