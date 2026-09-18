//go:build windows

package main

// Small platform helpers the elevation layer needs. Kept beside
// elevation_windows.go because each exists only for its one caller.

import (
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"unicode/utf16"
)

// hiddenSysProcAttr is the console-suppression attributes for schtasks
// (a console tool spawned by a windowsgui binary — without it every
// task operation would flash a cmd window).
func hiddenSysProcAttr() *syscall.SysProcAttr {
	return &syscall.SysProcAttr{CreationFlags: createNoWindow, HideWindow: true}
}

// writeFileUTF16LE writes s as UTF-16LE with a BOM — the encoding
// schtasks requires for /XML task definitions.
func writeFileUTF16LE(path, s string) error {
	u16 := utf16.Encode([]rune(s))
	buf := make([]byte, 2+len(u16)*2)
	buf[0], buf[1] = 0xFF, 0xFE // BOM: little-endian marker
	for i, r := range u16 {
		buf[2+i*2] = byte(r)
		buf[3+i*2] = byte(r >> 8)
	}
	return os.WriteFile(path, buf, 0o600)
}

// revokedIdsFile is how the operator's revoked-server list reaches an
// elevated client. The task spawn carries no custom environment, so the
// env channel (KVMSHARE_REVOKED_IDS) is unavailable on that path; the
// list is written to the state dir instead — a file only this user (and
// their elevated processes) can affect, consumed by the client at
// startup. The env channel stays for direct spawns: it avoids a file
// round-trip and cannot go stale.
const revokedIdsFile = "revoked-ids.txt"

// writeRevokedIdsFileLocked persists the revoked-server list for the
// elevated client spawn. Empty list → the file is removed, so a stale
// revocation can never outlive the setting. Callers hold a.mu (the list
// lives in settings). Best-effort: the client treats a missing file as
// "no revocations".
func (a *App) writeRevokedIdsFileLocked() {
	path := filepath.Join(a.stateDir, revokedIdsFile)
	ids := a.settings.RevokedServers
	if len(ids) == 0 {
		_ = os.Remove(path)
		return
	}
	_ = os.WriteFile(path, []byte(strings.Join(ids, ",")+"\n"), 0o600)
}
