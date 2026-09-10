package main

// rolelock.go — role identity on this machine, through the lock files
// the Rust binaries hold. Only a live process holds its flock (it is
// released on process death), so "lock held" == "role running" — the
// GUI's way of adopting instances it did not spawn and of refusing to
// spawn a second one.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// The two roles a machine can run. One at a time — the Rust binaries
// refuse to run alongside the opposite role, and the GUI enforces the
// same rule before starting either.
const (
	roleServer = "server"
	roleClient = "client"
)

func (a *App) roleLockPath(role string) string {
	return filepath.Join(a.stateDir, role+".lock")
}

// rolePidPath returns where the role records its pid. Kept out of the
// lock file on purpose: on Windows the lock is a byte-range LockFileEx,
// which blocks reads of the locked range by other handles — a pid inside
// the lock file would read back as empty (pid 0) and break stop-by-pid.
// (raiseScope lives in instance.go — it names the raise event.)
func (a *App) rolePidPath(role string) string {
	return filepath.Join(a.stateDir, role+".pid")
}

// roleActive reports whether a kvmshare process of `role` is running on
// this machine — ours or not (detected by probing the role's flock).
func (a *App) roleActive(role string) bool {
	f, err := os.OpenFile(a.roleLockPath(role), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return false
	}
	defer f.Close()
	if tryLockFile(f) == nil {
		unlockFile(f) // not held: we own it
		return false
	}
	return true // another process holds it
}

// pidFromLock returns the pid a running instance recorded for its role
// (0 when unknown). Read from the dedicated `.pid` file — the lock file
// itself cannot be read reliably on Windows (byte-range lock), and the
// pid is only consulted while the role lock is actually held, so a
// stale pid file (left by a crash) is never acted on.
func (a *App) pidFromLock(role string) int {
	raw, err := os.ReadFile(a.rolePidPath(role))
	if err != nil {
		return 0
	}
	var pid int
	if _, err := fmt.Sscanf(string(raw), "%d", &pid); err != nil || pid <= 1 {
		return 0
	}
	return pid
}

// logTail returns the last lines of a log file (for start-failure
// messages), or a generic note when the log is missing.
func logTail(path string) string {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "see the log for details"
	}
	lines := strings.Split(strings.TrimRight(string(raw), "\n"), "\n")
	if len(lines) > 3 {
		lines = lines[len(lines)-3:]
	}
	out := strings.Join(lines, " | ")
	if out == "" {
		return "see the log for details"
	}
	return out
}
