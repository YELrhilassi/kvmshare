package main

// Double-click detection for Windows. kvmshare-install.exe is a console
// binary: launching it from a terminal is exactly right (progress lines
// in the terminal), but double-clicking it in Explorer allocates a bare
// console that shows nothing during a long download — users read the
// frozen black box as a hang.
//
// The distinction is ownership of the console: a terminal launch runs
// inside the shell's console (GetConsoleProcessList lists the shell and
// this process), while a double-click creates a fresh console whose only
// attached process is this one. That is the moment to hand off to the
// GUI installer — same engine, visible progress — and let the console
// die with this short-lived process.

import (
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
)

var (
	kernel32                  = windows.NewLazySystemDLL("kernel32.dll")
	procGetConsoleProcessList = kernel32.NewProc("GetConsoleProcessList")
)

// consoleProcessCount returns how many processes are attached to this
// process's console (0 when it has none).
func consoleProcessCount() int {
	var buf [2]uint32
	n, _, _ := procGetConsoleProcessList.Call(uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)))
	if n == 0 || n > uintptr(len(buf)) {
		return int(n)
	}
	return int(n)
}

// doubleClicked reports whether this install was started by double-click
// rather than from a terminal: no arguments, and this process is the sole
// owner of a freshly allocated console.
func doubleClicked() bool {
	if flag.NArg() > 0 || flag.NFlag() > 0 {
		return false
	}
	// GetConsoleProcessList returns 0 when this process has no console
	// at all (a windowsgui parent, a service) — not a double-click.
	if consoleProcessCount() == 0 {
		return false
	}
	return consoleProcessCount() == 1
}

// launchGUIInstaller starts the GUI installer (the sibling
// kvmshare-installer.exe) detached from this process and exits. The GUI
// installer is a windowsgui binary — its window appears with the live
// progress bar; the bare console disappears with this process.
func launchGUIInstaller() bool {
	exe, err := os.Executable()
	if err != nil {
		return false
	}
	installer := filepath.Join(filepath.Dir(exe), "kvmshare-installer.exe")
	if _, err := os.Stat(installer); err != nil {
		// No sibling GUI installer (a bare CLI download): keep the
		// console flow — it works, it just lacks the pretty bar.
		fmt.Fprintln(os.Stderr, "kvmshare-installer.exe not found next to this binary; continuing in the console")
		return false
	}
	// No HideWindow/CREATE_NO_WINDOW here: this child is a windowsgui
	// binary whose whole point is a visible window, and STARTF_USESHOW-
	// WINDOW could suppress its first paint. It has no console anyway.
	cmd := exec.Command(installer)
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: syscall.CREATE_NEW_PROCESS_GROUP}
	if err := cmd.Start(); err != nil {
		fmt.Fprintf(os.Stderr, "could not open the installer window (%v); continuing in the console\n", err)
		return false
	}
	return true
}
