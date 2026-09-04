//go:build linux

package main

// Linux system integration for the installer: input-device access.
//
// Kernel input isolation (EVIOCGRAB — see crates/platform) needs to read
// /dev/input/event*, which is root:input on most distros. The installer
// grants it the platform way: a udev rule that tags input devices with
// `uaccess`. On elogind/logind systems that makes the active seat user
// get an ACL on the spot (no group change, no re-login); the `input`
// group addition is the fallback for runit/other init systems. Writing
// to /etc and calling usermod need root, so the step re-executes itself
// through pkexec (the desktop's standard privilege prompt — the same
// one-shot consent as the Windows UAC). If no privilege agent is
// available the install still succeeds; the server just runs grab-only.

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
)

// The udev rule granting the desktop user access to input devices. Must
// stay in sync with packaging/99-kvmshare-input.rules.
const udevRule = `# kvmshare: grant the active desktop user read access to physical
# input devices (kernel input isolation for the shared keyboard/mouse).
KERNEL=="event*", SUBSYSTEM=="input", MODE="0660", GROUP="input", TAG+="uaccess"
`

const rulePath = "/etc/udev/rules.d/99-kvmshare-input.rules"

// integrateDesktop runs after a successful install: grant input access.
// Failure is a warning, never an install failure — without the rule the
// software still works, only raw-event leaks to raw-reading apps remain.
func integrateDesktop(dir string) error {
	if os.Geteuid() == 0 {
		return integrateInputAccess()
	}
	return integrateViaPkexec()
}

// removeDesktopIntegration undoes integrateDesktop (used by --uninstall).
func removeDesktopIntegration(dir string) error {
	if os.Geteuid() != 0 {
		// Best-effort without elevation: leave the rule (harmless) but
		// report nothing — uninstall of the user's files already worked.
		return nil
	}
	return os.Remove(rulePath)
}

// launchGUI is a no-op on Linux (the GUI is launched from the desktop
// entry; main.go prints the command).
func launchGUI(string) error { return nil }

// integrateViaPkexec re-executes this installer as root with
// --input-access, so the user consents through the desktop's standard
// privilege prompt instead of a shell command.
func integrateViaPkexec() error {
	if _, err := exec.LookPath("pkexec"); err != nil {
		return fmt.Errorf("input access not granted (no pkexec): %v", err)
	}
	exe, err := os.Executable()
	if err != nil {
		return fmt.Errorf("input access not granted: %v", err)
	}
	cmd := exec.Command("pkexec", exe, "--input-access")
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("input access not granted (pkexec declined?): %v: %s", err, strings.TrimSpace(string(out)))
	}
	return nil
}

// integrateInputAccess runs as root (either directly or via pkexec):
// install the udev rule, add the invoking user to the input group
// (non-logind fallback), and make udev apply everything right now.
func integrateInputAccess() error {
	if err := os.WriteFile(rulePath, []byte(udevRule), 0o644); err != nil {
		return fmt.Errorf("write %s: %w", rulePath, err)
	}
	// The user who asked for elevation: SUDO_USER when run via sudo,
	// PKEXEC_UID when run via pkexec, else the real uid.
	user := os.Getenv("SUDO_USER")
	if user == "" {
		if uid := os.Getenv("PKEXEC_UID"); uid != "" && uid != "0" {
			if u, err := userFromUID(uid); err == nil {
				user = u
			}
		}
	}
	if user != "" {
		_ = exec.Command("usermod", "-aG", "input", user).Run()
	}
	_ = exec.Command("udevadm", "control", "--reload-rules").Run()
	_ = exec.Command("udevadm", "trigger", "--subsystem-match=input").Run()
	return nil
}

// userFromUID resolves a numeric uid to a login name (os/user is cgo-free
// here; this is the portable fallback).
func userFromUID(uid string) (string, error) {
	out, err := exec.Command("id", "-nu", uid).Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}
