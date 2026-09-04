//go:build !windows && !linux

package main

// Desktop integration on platforms without a native hook (macOS, BSDs):
// no-ops. Windows is handled by integrate_windows.go, Linux (input-device
// access) by integrate_linux.go.

// integrateDesktop is a no-op outside Windows/Linux.
func integrateDesktop(string) error { return nil }

// removeDesktopIntegration is a no-op outside Windows/Linux.
func removeDesktopIntegration(string) error { return nil }

// launchGUI is a no-op outside Windows (Linux installs print instructions
// instead; the GUI is launched from the desktop entry).
func launchGUI(string) error { return nil }
