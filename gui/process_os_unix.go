//go:build !windows

package main

// Unix (Linux/macOS) process and lock primitives. The GUI spawns role
// processes in their own process group so stopping one never touches
// anything else, and takes advisory flock locks for the instance and
// role files (they die with the process — no stale locks).

import (
	"os"
	"syscall"
)

// processGroupAttrs puts the spawned child in its own process group.
func processGroupAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Setpgid: true}
}

// The graceful and forced termination signals.
const (
	signalTerm = syscall.SIGTERM
	signalKill = syscall.SIGKILL
)

// signalGroup signals the whole process group (`-pid`).
func signalGroup(pid int, sig syscall.Signal) error {
	return syscall.Kill(-pid, sig)
}

// signalPid signals a single process (used to stop an instance the GUI
// did not spawn itself — the pid recorded in the role lock file).
func signalPid(pid int) error {
	return syscall.Kill(pid, syscall.SIGTERM)
}

// tryLockFile takes a non-blocking exclusive flock. Returns an error if
// another process holds the lock. The lock is released when the file is
// closed or the process dies.
func tryLockFile(f *os.File) error {
	return syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
}

// unlockFile releases a lock taken by tryLockFile.
func unlockFile(f *os.File) {
	_ = syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
}
