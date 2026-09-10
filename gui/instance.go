package main

// instance.go — one GUI per machine. A second launch raises the running
// instance's window and exits quietly: from dmenu or a launcher there
// is no terminal, so "already running" must *show the window*, not die
// silently. The flock releases automatically when the process dies —
// no stale state after a crash.

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"time"
)

// SingleInstance takes the GUI's instance lock. Returns whether another
// instance was running and has been asked to come forward (raised,
// err == nil), so the caller can exit quietly.
func (a *App) SingleInstance() (raised bool, err error) {
	a.restartHandoff()
	f, err := os.OpenFile(a.instanceLockPath, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return false, fmt.Errorf("open instance lock: %w", err)
	}
	if err := tryLockFile(f); err != nil {
		f.Close()
		// Someone else holds the lock. Ask that instance (its pid is
		// recorded in the pid file) to show itself; give it a moment to
		// write the pid in case it is still starting up.
		for i := 0; i < 10; i++ {
			if pid := a.pidFromLock("gui"); pid > 0 {
				if raiseInstance(pid, a.raiseScope()) == nil {
					return true, nil
				}
				break
			}
			time.Sleep(50 * time.Millisecond)
		}
		return false, fmt.Errorf("kvmshare is already running on this machine")
	}
	// We hold the lock: record our pid in the dedicated pid file so a
	// later launch can raise us (the lock file itself is lock-only — its
	// byte-range lock on Windows blocks reads by other handles).
	_ = os.WriteFile(a.rolePidPath("gui"), []byte(fmt.Sprintf("%d\n", os.Getpid())), 0o644)
	a.instanceLock = f
	return false, nil
}

// restartHandoff is the second half of an in-place update (see
// update.go): the freshly-updated process was spawned by the old one,
// which is still alive and still holds the instance lock. Wait for that
// lock to be released instead of raising the old instance — it is on its
// way out and will never come forward. Bounded: if the lock is still
// held after the grace period, something else is genuinely running, and
// the caller falls through to the normal "already running" handling.
func (a *App) restartHandoff() {
	if os.Getenv("KVMSHARE_RESTART") == "" {
		return
	}
	deadline := time.Now().Add(6 * time.Second)
	for time.Now().Before(deadline) {
		f, err := os.OpenFile(a.instanceLockPath, os.O_CREATE|os.O_RDWR, 0o644)
		if err == nil {
			if tryLockFile(f) == nil {
				unlockFile(f) // probe only — the caller takes the lock
				f.Close()
				break
			}
			f.Close()
		}
		time.Sleep(100 * time.Millisecond)
	}
	// The handoff env must not leak into processes this instance spawns
	// later (a role, or a future restart of its own).
	_ = os.Unsetenv("KVMSHARE_RESTART")
}

// raiseScope returns a stable per-install string used to name the
// "show your window" event between GUI instances (Windows; Unix ignores
// it). Both instances of the same install (same state dir) compute the
// same name; a different install or user never collides.
func (a *App) raiseScope() string {
	sum := sha256.Sum256([]byte(a.stateDir))
	return hex.EncodeToString(sum[:6])
}
