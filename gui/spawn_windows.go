//go:build windows

package main

// Role spawning on Windows: the elevation task first (created once by
// the elevated installer; running it is unprivileged), a direct spawn
// as the fallback. See internal/installer/elevation_windows.go for the
// full model and gui/elevation_windows.go for the GUI side.

import (
	"log/slog"
	"path/filepath"
	"strings"

	"kvmshare/gui/internal/installer"
)

// startRoleProcess starts the role (server or client) with the
// platform's privileges:
//
//  1. When this process is already elevated (a hand-elevated GUI),
//     the child inherits elevation — spawn directly.
//  2. Otherwise start the role's elevation task (unprivileged): stage
//     the args file, schtasks /Run, wait for the role lock.
//  3. When the task is missing, relay its creation through the
//     elevated installer (one UAC prompt) and start again.
//  4. When that is refused — a standard user without the tasks — spawn
//     directly (non-elevated) so the product still works for normal
//     windows, and log why.
//
// Returns the reaped child handle like spawn does — nil for the task
// path, where the process belongs to the task scheduler and liveness
// is proven by the role lock before this returns.
func (a *App) startRoleProcess(bin, logPath string, env []string, args ...string) (*proc, error) {
	role := roleFromBinName(bin)
	if elevated() {
		return a.spawn(bin, logPath, env, args...)
	}
	// Task spawn: the task carries no custom environment, so anything
	// env-dependent for the child must ride the args file instead. The
	// full argv (flags included) goes in; --log-file lands the logger
	// where the GUI's tailer looks; the revoked-ids list is already in
	// the state dir as the client's fallback channel.
	full := append(append([]string{}, args...), "--log-file", logPath)
	alive := func() bool { return a.roleActive(role) }
	err := startRoleElevated(a.stateDir, role, full, alive)
	if err == nil {
		return nil, nil
	}
	// Task missing or refused to run: try creating it through the
	// elevated installer once (one UAC prompt, only when actually
	// needed), then start again.
	slog.Warn("elevation task start failed — relaying creation through the elevated installer", "err", err)
	if relayErr := installer.EnsureElevationTasksViaInstaller(a.stateDir); relayErr == nil {
		if err2 := startRoleElevated(a.stateDir, role, full, alive); err2 == nil {
			return nil, nil
		}
	}
	// Fallback: direct spawn with the full environment, exactly as
	// before this layer existed.
	slog.Warn("elevated start unavailable — starting the role non-elevated (elevated windows will ignore shared input)")
	return a.spawn(bin, logPath, env, args...)
}

// roleFromBinName maps a role binary path to the role name the lock
// files and task names use (kvmshare-server[.exe] → server).
func roleFromBinName(bin string) string {
	base := filepath.Base(bin)
	base = strings.TrimSuffix(base, ".exe")
	return strings.TrimPrefix(base, "kvmshare-")
}
