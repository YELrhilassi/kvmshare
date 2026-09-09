//go:build linux

package main

// Device access on Linux, covering everything the product needs:
//
//   - server role: read on /dev/input/event* so the backend can isolate
//     the physical input devices at the kernel (EVIOCGRAB) while the
//     shared cursor is on another machine.
//   - client role (and server): write on /dev/uinput so the per-session
//     wheel daemon can create its virtual mouse — injected scroll then
//     goes through the real kernel pipeline, the only path every
//     application accepts (GLFW apps like kitty ignore XTest-emulated
//     wheel).
//
// Both grants are one udev rule installed by the installer (`kvmshare-install
// --input-access`): grant-only-if-missing, self-elevating through the
// desktop's standard privilege prompt, silent forever once access exists.
// This file triggers that step from the GUI, so the user never sees a
// shell command — at most one consent prompt on first use, exactly like
// the Windows UAC prompt. If the prompt is declined (or no polkit agent
// exists) everything keeps working with reduced fidelity: the server
// reports isolation as unavailable, and wheel falls back to XTest in
// the apps that still accept it.

import (
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// ensureInputAccess checks — in the background, never blocking startup —
// whether this machine has the device access it needs for its role, and
// grants it through the sibling installer when it cannot. Silent in
// every case: errors just mean reduced fidelity (reported by the roles
// themselves, never a GUI error).
func (a *App) ensureInputAccess() {
	// The portable installer and make install place kvmshare-install next
	// to the role binaries. Without it (a bare copy of the GUI) there is
	// nothing to run.
	installer := filepath.Join(filepath.Dir(a.serverPath), "kvmshare-install")
	if _, err := os.Stat(installer); err != nil {
		return
	}
	go func() {
		time.Sleep(2 * time.Second) // never contend with startup
		cmd := exec.Command(installer, "--input-access")
		_ = cmd.Run()
	}()
}
