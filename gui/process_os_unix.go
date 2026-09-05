//go:build !windows

package main

// Unix (Linux/macOS) process and lock primitives. The GUI spawns role
// processes in their own process group so stopping one never touches
// anything else, and takes advisory flock locks for the instance and
// role files (they die with the process — no stale locks).

import (
	"os"
	"os/exec"
	"os/signal"
	"syscall"
)

// processGroupAttrs puts the spawned child in its own process group.
func processGroupAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Setpgid: true}
}

// restartAttrs is like processGroupAttrs but detaches the child into its
// own session too — used when the GUI restarts itself into a new version
// (the child must survive the parent exiting).
func restartAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{Setpgid: true, Setsid: true}
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

// forceKillPid is the last-resort kill for a process that ignored the
// graceful signal.
func forceKillPid(pid int) error {
	return syscall.Kill(pid, syscall.SIGKILL)
}

// killRoleByName force-kills every process with the role binary's name.
// The fallback when the lock file carries no pid (a crash between
// locking and writing): the role lock may be held by a process we cannot
// address by pid, but its name is stable.
func killRoleByName(bin string) error {
	return exec.Command("pkill", "-9", "-x", bin).Run()
}

// raiseSignal is the "show your window" signal between GUI instances.
//
// Deliberately NOT SIGUSR1: JavaScriptCore (WebKit's JS engine) uses
// SIGUSR1 for its garbage collector on Linux, so claiming it here
// corrupts the webview (verified: a SIGUSR1-based raise crashed the GUI
// with a SIGSEGV inside GTK after WebKit warned "Overriding existing
// handler for signal 10"). SIGUSR2 is unclaimed by the graphics stack.
const raiseSignal = syscall.SIGUSR2

// raiseInstance asks a running GUI to show and focus its window. A second
// launch uses this so "already running" never looks like "nothing
// happened" (the classic invisible-ghost trap: the window was hidden to
// the tray, so a dmenu relaunch died silently).
func raiseInstance(pid int) error {
	return syscall.Kill(pid, raiseSignal)
}

// watchRaiseSignal calls `onRaise` when another instance asks us to come
// forward. Signals are only meaningful on Unix; Windows gets a no-op.
func watchRaiseSignal(onRaise func()) {
	ch := make(chan os.Signal, 1)
	signal.Notify(ch, raiseSignal)
	go func() {
		for range ch {
			onRaise()
		}
	}()
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
