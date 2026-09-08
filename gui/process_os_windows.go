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
// group; CREATE_NO_WINDOW gives it no console at all — this GUI is a
// windowsgui binary, so a console child (the Rust roles, taskkill)
// would otherwise flash a cmd window. HideWindow is belt-and-braces.
const (
	createNewProcessGroup = 0x00000200
	createNoWindow        = 0x08000000
)

// processGroupAttrs puts the spawned child in its own process group
// with no console window.
func processGroupAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{CreationFlags: createNewProcessGroup | createNoWindow, HideWindow: true}
}

// restartAttrs is like processGroupAttrs (Windows has no sessions); used
// when the GUI restarts itself into a new version.
func restartAttrs() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{CreationFlags: createNewProcessGroup | createNoWindow, HideWindow: true}
}

// hiddenCmd runs a console tool (taskkill) without flashing a window.
func hiddenCmd(name string, args ...string) *exec.Cmd {
	cmd := exec.Command(name, args...)
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: createNoWindow}
	return cmd
}

// No signals on Windows: both the graceful and forced stop map to
// TerminateProcess. Keep the names so the shared code reads the same.
const (
	signalTerm = syscall.Signal(0)
	signalKill = syscall.Signal(0)
	// raiseSignal exists for symmetry with Unix so shared code (and
	// tests) can reference one name; Windows cannot raise (see
	// raiseInstance) so the value is never delivered.
	raiseSignal = syscall.Signal(0)
)

// signalGroup terminates the process (no groups to signal on Windows).
func signalGroup(pid int, _ syscall.Signal) error {
	return terminateProcess(pid)
}

// signalPid terminates a single process by pid.
func signalPid(pid int) error {
	return terminateProcess(pid)
}

// raiseInstance asks a running GUI to show and focus its window. Windows
// has no POSIX signals, so this sets a named event that the running
// instance watches (see watchRaiseSignal); the event name is scoped to
// the install, so two installs never cross-talk. The pid is unused (the
// event addresses the instance directly) but kept for signature parity
// with the Unix implementation.
func raiseInstance(pid int, scope string) error {
	ev, err := openRaiseEvent(scope)
	if err != nil {
		return err
	}
	defer windows.CloseHandle(ev)
	return windows.SetEvent(ev)
}

// watchRaiseSignal calls `onRaise` when another instance asks us to come
// forward by setting the named raise event. Runs for the process
// lifetime; the event handle is process-owned and dies with it. A
// missing event (older instance) simply never fires.
func watchRaiseSignal(scope string, onRaise func()) {
	ev, err := openRaiseEvent(scope)
	if err != nil {
		return
	}
	go func() {
		for {
			wait, err := windows.WaitForSingleObject(ev, windows.INFINITE)
			if err != nil || wait == windows.WAIT_FAILED {
				return
			}
			onRaise()
		}
	}()
}

// raiseEventName names the raise event in the Local\ namespace (per
// logon session — right for a per-user GUI), scoped to the install so
// different installs or users never signal each other.
func raiseEventName(scope string) string {
	return `Local\kvmshare-raise-` + scope
}

// openRaiseEvent opens (creating if needed) the auto-reset raise event:
// each SetEvent wakes exactly one waiter and the event resets, so a
// burst of second launches maps to a burst of raises.
func openRaiseEvent(scope string) (windows.Handle, error) {
	name, err := windows.UTF16PtrFromString(raiseEventName(scope))
	if err != nil {
		return 0, err
	}
	return windows.CreateEvent(nil, 0, 0, name)
}

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
	out, err := hiddenCmd("taskkill", "/F", "/PID", strconv.Itoa(pid)).CombinedOutput()
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
	out, err := hiddenCmd("taskkill", "/F", "/IM", bin).CombinedOutput()
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
