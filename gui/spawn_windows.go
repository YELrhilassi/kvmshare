//go:build windows

package main

// Role spawning on Windows: the elevated task path first, a direct
// spawn as the fallback. See elevation_windows.go for why the roles
// must run elevated (UIPI) and why the GUI must not (autostart).

import (
	"log/slog"
	"path/filepath"
	"strings"
	"time"

	"golang.org/x/sys/windows"

	"kvmshare/gui/internal/installer"
)

// elevatedAliveTimeout bounds the wait for a task-started role to
// actually appear (the role takes its lock at startup; the task start
// itself only queues the process). Short: a role that cannot come up
// fails visibly in seconds, not minutes.
const elevatedAliveTimeout = 5 * time.Second

// startRoleProcess starts `bin args...` as this machine's role:
//
//  1. When this process is already elevated (a hand-elevated GUI), the
//     child inherits elevation — spawn directly.
//  2. Otherwise go through the scheduled task (schtasks /Run starts it
//     elevated, no prompt: the grant was accepted at task creation).
//  3. When the task path is unavailable — a standard user, a hardened
//     policy, schtasks broken — spawn directly (non-elevated) so the
//     product still works for normal windows, and log why.
//
// Returns the reaped child handle like spawn does — nil for the task
// path, where the process belongs to the task scheduler and liveness
// is proven by the role lock before this returns.
func (a *App) startRoleProcess(bin, logPath string, env []string, args ...string) (*proc, error) {
	if elevated() {
		return a.spawn(bin, logPath, env, args...)
	}
	// Task spawn: no environment, no stderr. The args therefore carry
	// --log-file so the logger lands where the GUI's tailer looks, and
	// the revoked-ids list was written to the state dir at spawn time.
	roleArgs := append(append([]string{}, args...), "--log-file", logPath)
	role := roleFromBinName(bin)
	alive := func() bool { return a.roleActive(role) }
	err := spawnRoleElevated(a.stateDir, bin, roleArgs, alive, elevatedAliveTimeout)
	if err == nil {
		return nil, nil
	}
	slog.Warn("elevated task spawn unavailable — starting the role non-elevated instead",
		"bin", filepath.Base(bin), "err", err)
	// Fallback: direct spawn with the full environment, exactly as
	// before this layer existed.
	return a.spawn(bin, logPath, env, args...)
}

// roleFromBinName maps a role binary path to the role name the lock
// files use (kvmshare-server[.exe] → server).
func roleFromBinName(bin string) string {
	base := filepath.Base(bin)
	base = strings.TrimSuffix(base, ".exe")
	return strings.TrimPrefix(base, "kvmshare-")
}

// elevationStatus is the user-facing summary of how this machine's role
// processes get their input privileges.
type elevationStatus struct {
	// Elevated reports whether the role processes run with an elevated
	// token (needed to control elevated windows — Task Manager,
	// installers, UAC prompts).
	Elevated bool `json:"elevated"`
	// CanElevate reports whether this account can start elevated roles
	// (administrators can; standard users get the non-elevated
	// fallback, which works for normal windows only).
	CanElevate bool `json:"canElevate"`
	// Detail is the one sentence the UI shows when elevated is false.
	Detail string `json:"detail,omitempty"`
}

// RoleElevation reports the role-elevation picture for this machine.
// Bound for the frontend; a no-argument read, safe from any goroutine.
func (a *App) RoleElevation() elevationStatus {
	if elevated() {
		return elevationStatus{Elevated: true, CanElevate: true}
	}
	// A task we can query is a task we can run: its grant was accepted
	// at creation, so /Run needs no new consent.
	if _, err := schtasks("/Query", "/TN", elevationTaskName()); err == nil {
		return elevationStatus{Elevated: true, CanElevate: true}
	}
	st := elevationStatus{CanElevate: adminUser()}
	if st.CanElevate {
		st.Detail = "starts elevated on the next role start (Windows grants it without a prompt)"
	} else {
		st.Detail = "standard user — elevated windows (Task Manager, installers) ignore the shared input; an administrator session enables full control"
	}
	return st
}

// adminUser reports whether the current user holds Administrators
// membership on its (possibly filtered) token — the predictor for
// whether task creation with RunLevel HIGHEST succeeds.
func adminUser() bool {
	if installer.IsElevated() {
		return true
	}
	admin, err := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid)
	if err != nil {
		return false
	}
	member, err := windows.GetCurrentProcessToken().IsMember(admin)
	return err == nil && member
}
