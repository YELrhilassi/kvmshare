//go:build !windows && !linux

package installer

import "fmt"

// Desktop integration on platforms without a native hook (macOS, BSDs):
// no-ops. Windows is handled by integrate_windows.go, Linux (input-device
// access) by integrate_linux.go.

// IsElevated and SelfElevate: no elevation concept needed on platforms
// with no privileged steps; the uninstall simply removes the user files.
func IsElevated() bool           { return true }
func SelfElevate([]string) error { return nil }

// integrateDesktop is a no-op outside Windows/Linux.
func integrateDesktop(string) error { return nil }

// ensureInputAccess is unsupported outside Linux.
func ensureInputAccess() error { return fmt.Errorf("--input-access is Linux-only") }

// removeDesktopIntegration is a no-op outside Windows/Linux.
func removeDesktopIntegration(string) error { return nil }

// launchGUI is a no-op outside Windows (Linux installs print instructions
// instead; the GUI is launched from the desktop entry).
func launchGUI(string) error { return nil }
