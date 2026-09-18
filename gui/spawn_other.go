//go:build !windows

package main

// Role spawning on Unix: a direct spawn. Linux needs no elevation
// machinery — device access is a one-time grant (udev rule), not a
// per-process token, so the GUI spawns the roles exactly as it always
// did. The bool return keeps the call sites platform-uniform (on Unix
// the answer "is the role privileged for input" is simply yes).

// startRoleProcess starts `bin args...` in its own process group with
// the GUI's reaper attached (see process.go spawn). Same shape as the
// Windows elevated path — the handle is always live here.
func (a *App) startRoleProcess(bin, logPath string, env []string, args ...string) (*proc, error) {
	return a.spawn(bin, logPath, env, args...)
}

// writeRevokedIdsFileLocked is the Windows file-fallback for the
// client's revoked-server list; Unix passes the list through the
// environment at every spawn, so there is nothing to persist.
func (a *App) writeRevokedIdsFileLocked() {}

// cleanupElevationTask is the Windows leftover sweep; Unix has no task.
func (a *App) cleanupElevationTask() {}
