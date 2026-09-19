//go:build !windows

package main

// The GUI's elevation layer on platforms that need no role elevation:
// Linux grants input-device access through a one-time udev rule (not a
// process privilege), and there is no UIPI to filter injected input —
// so roles run as plain children of the GUI. The Windows side
// (elevation_windows.go) is the real implementation; these stubs keep
// the shared lifecycle code (main.go, roles.go) platform-neutral.

// elevated reports whether this process could pass privileges to its
// children. Everything is a plain child here — reported true so the
// spawn path never reaches for a task mechanism that does not exist.
func elevated() bool { return true }

// startRoleElevated has nothing to elevate on this platform; the spawn
// path calls a.spawn directly and never reaches it. Present so the
// elevation layer's shape stays comparable across platforms.
func startRoleElevated(stateDir, role string, args []string, alive func() bool) error {
	return nil
}

// writeArgsFile: no elevation task consumes an args file here, but the
// Rust side accepts the same argv directly — nothing to stage.
func writeArgsFile(path string, args []string) error { return nil }

// elevationStatus mirrors the Windows type for the bound RoleElevation.
type elevationStatus struct {
	Elevated   bool   `json:"elevated"`
	CanElevate bool   `json:"canElevate"`
	Detail     string `json:"detail,omitempty"`
}

// RoleElevation: roles always run with full input access here (the
// one-time udev grant is what Linux needs, handled elsewhere).
func (a *App) RoleElevation() elevationStatus {
	return elevationStatus{Elevated: true, CanElevate: true}
}

// cleanupElevationTasks: no scheduled tasks exist on this platform.
func (a *App) cleanupElevationTasks() {}
