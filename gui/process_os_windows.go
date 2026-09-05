//go:build windows

package main

// Windows process and lock primitives. There are no process groups or
// signals on Windows, so "stop" terminates the process handle directly,
// and role/instance locks use LockFileEx (also released automatically on
// process death).

import (
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"syscall"

	"golang.org/x/sys/windows"
)

// CREATE_NEW_PROCESS_GROUP gives the child its own console process
// group, and HideWindow keeps the console hidden in GUI sessions.
const createNewProcessGroup = 0x00000200

// processGroupAttrs puts the spawned child in its own process group.
func processGroupAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{CreationFlags: createNewProcessGroup, HideWindow: true}
}

// restartAttrs is like processGroupAttrs (Windows has no sessions); used
// when the GUI restarts itself into a new version.
func restartAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{CreationFlags: createNewProcessGroup, HideWindow: true}
}

// No signals on Windows: both the graceful and forced stop map to
// TerminateProcess. Keep the names so the shared code reads the same.
const (
	signalTerm = syscall.Signal(0)
	signalKill = syscall.Signal(0)
)

// signalGroup terminates the process (no groups to signal on Windows).
func signalGroup(pid int, _ syscall.Signal) error {
	return terminateProcess(pid)
}

// signalPid terminates a single process by pid.
func signalPid(pid int) error {
	return terminateProcess(pid)
}

// raiseInstance is not yet implemented on Windows (no POSIX signals; a
// registered window message would do it). A second launch on Windows
// falls back to the plain "already running" message.
func raiseInstance(pid int) error {
	return fmt.Errorf("raise not supported on windows")
}

// watchRaiseSignal is a no-op on Windows (see raiseInstance).
func watchRaiseSignal(func()) {}

func terminateProcess(pid int) error {
	h, err := windows.OpenProcess(windows.PROCESS_TERMINATE, false, uint32(pid))
	if err != nil {
		return err
	}
	defer windows.CloseHandle(h)
	return windows.TerminateProcess(h, 1)
}

// forceKillPid kills a process that rejected a direct handle: the
// graceful path first, then `taskkill /F` (which walks up to the
// process's own privilege level the way a plain OpenProcess cannot — an
// elevated target under a non-elevated GUI, for instance).
func forceKillPid(pid int) error {
	if err := terminateProcess(pid); err == nil {
		return nil
	}
	out, err := exec.Command("taskkill", "/F", "/PID", strconv.Itoa(pid)).CombinedOutput()
	if err != nil {
		return fmt.Errorf("taskkill %d: %v (%s)", pid, err, strings.TrimSpace(string(out)))
	}
	return nil
}

// killRoleByName force-kills every process with the role binary's image
// name. The fallback when the lock file carries no pid (a crash between
// locking and writing, or an old binary): the role lock may be held by a
// process we cannot address by pid, but its name is stable.
func killRoleByName(bin string) error {
	out, err := exec.Command("taskkill", "/F", "/IM", bin).CombinedOutput()
	if err != nil {
		return fmt.Errorf("taskkill /IM %s: %v (%s)", bin, err, strings.TrimSpace(string(out)))
	}
	return nil
}

// tryLockFile takes a non-blocking exclusive byte-range lock on the whole
// file. Returns an error if another process holds it. The lock is
// released when the handle is closed or the process dies.
func tryLockFile(f *os.File) error {
	ol := new(windows.Overlapped)
	return windows.LockFileEx(
		windows.Handle(f.Fd()),
		windows.LOCKFILE_EXCLUSIVE_LOCK|windows.LOCKFILE_FAIL_IMMEDIATELY,
		0, 1, 0, ol,
	)
}

// unlockFile releases a lock taken by tryLockFile.
func unlockFile(f *os.File) {
	ol := new(windows.Overlapped)
	_ = windows.UnlockFileEx(windows.Handle(f.Fd()), 0, 1, 0, ol)
}
