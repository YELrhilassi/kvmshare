//go:build windows

package main

// The GUI's side of the role-elevation contract. The tasks themselves —
// names, XML, creation — live in internal/installer (elevation_windows.go
// there): the elevated installer is what creates them, and the GUI only
// ever *runs* them (an unprivileged operation by design). This file holds
// what the GUI uniquely needs: starting a role through its task, the
// user-facing elevation status for the Settings page, and the sweep of
// leftover debug tasks older builds created.

import (
	"fmt"
	"os"
	"path/filepath"
	"time"

	"kvmshare/gui/internal/installer"
)

// elevated reports whether THIS process holds an elevated token. The
// GUI usually does not (by design — an elevated GUI cannot autostart);
// a hand-elevated one can, and then children simply inherit — no task
// needed.
func elevated() bool {
	return installer.IsElevated()
}

// elevationAliveTimeout bounds the wait for a task-started role to
// actually appear (the task start itself only queues the process; the
// role takes its lock at startup). Short: a role that cannot come up
// fails visibly in seconds, not minutes.
const elevationAliveTimeout = 5 * time.Second

// startRoleElevated stages args (one argv token per line — the Rust
// side's merged_argv contract) and runs the role's task, waiting until
// the role is actually alive (polling `alive`, which checks the role
// lock). Returns an error when the task is missing — the caller relays
// creation through the elevated installer or falls back.
func startRoleElevated(stateDir, role string, args []string, alive func() bool) error {
	if err := writeArgsFile(filepath.Join(stateDir, role+"-args.txt"), args); err != nil {
		return fmt.Errorf("stage role args: %w", err)
	}
	if err := installer.RunElevationTask(role); err != nil {
		return err
	}
	deadline := time.Now().Add(elevationAliveTimeout)
	for time.Now().Before(deadline) {
		if alive() {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}
	return fmt.Errorf("elevated role did not appear within %s", elevationAliveTimeout)
}

// writeArgsFile encodes the spawn contract: one argv token per line,
// nothing else — the exact encoding kvmshare_app::args::merged_argv
// parses. Written for the role's own user (the task runs as this user,
// elevated); 0o600 keeps other local users out of a file that can name
// a server address.
func writeArgsFile(path string, args []string) error {
	var b []byte
	for _, a := range args {
		b = append(b, a...)
		b = append(b, '\n')
	}
	return os.WriteFile(path, b, 0o600)
}

// elevationStatus is the user-facing summary of how this machine's role
// processes get their input privileges.
type elevationStatus struct {
	// Elevated reports whether the role processes run with an elevated
	// token (needed to control elevated windows — Task Manager,
	// installers, UAC prompts).
	Elevated bool `json:"elevated"`
	// CanElevate reports whether this account can start elevated roles
	// (the tasks exist, or the user can install them).
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
	// A task we can query is a task we can run: running is
	// unprivileged by design. The server task stands for both — they
	// are created (and wiped) together.
	if installer.ElevationTaskPresent("server") {
		return elevationStatus{Elevated: true, CanElevate: true}
	}
	st := elevationStatus{CanElevate: adminUser()}
	if st.CanElevate {
		st.Detail = "the elevation tasks are missing — they are restored on the next role start (one Windows prompt)"
	} else {
		st.Detail = "no elevation tasks — roles run without elevated input (elevated windows ignore shared input); reinstall to enable"
	}
	return st
}

// cleanupElevationTasks deletes leftover scheduled tasks. The debug
// tasks are the ones earlier builds created to launch the GUI itself
// elevated — an elevated GUI breaks the elevation model (non-elevated
// Run-key GUI vs elevated task GUI racing over one state dir). Safe at
// every GUI start: missing tasks are simply not there.
func (a *App) cleanupElevationTasks() {
	installer.SweepDebugTasks()
}

// adminUser reports whether the current user holds Administrators
// membership on its (possibly filtered) token — the predictor for
// whether the elevation-task relay will be able to create the tasks.
func adminUser() bool {
	return installer.AdminUser()
}
