//go:build linux

package main

// Input-device access on Linux. While this machine runs as the server the
// backend isolates the physical input devices at the kernel (EVIOCGRAB)
// so no app can react to forwarded input — reading those devices needs a
// one-time system grant that the installer performs (a udev rule with the
// user's uid baked in as OWNER, plus a live chown — no logind, no group
// membership, no re-login). This file triggers that step from the GUI:
// `kvmshare-install --input-access` is grant-only-if-missing and
// self-elevating, so nothing ever happens once access exists and at most
// one privilege prompt appears the first time.

import (
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

// ensureInputAccess checks — in the background, never blocking startup —
// whether this machine can isolate its input devices, and grants access
// through the sibling installer when it cannot. Silent in every case:
// errors just mean the server runs without isolation (it reports that
// itself).
func (a *App) ensureInputAccess() {
	if a.settings.Mode != ModeServer {
		return // isolation is server-side only; clients need nothing
	}
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
