package main

// The machine id is a stable, random per-installation identifier stored
// in the state directory as `machine.id` (16 bytes, hex — 32 chars).
// The Rust binaries read and write the *same* file (crates/app/src/
// machine_id.rs), so the id a user sees in this GUI is exactly what goes
// on the wire: the client sends it in `Hello` (the server's trusted-ids
// policy and client list use it) and the server sends its own in
// `Welcome` (the client's trusted-server list uses it). This Go copy
// only reads-or-creates — the format must stay byte-identical to the
// Rust side.

import (
	"crypto/rand"
	"encoding/hex"
	"os"
	"path/filepath"
	"strings"
)

// GetMachineId returns this machine's stable id, creating it on first
// use. The write is atomic (tmp + rename); a lost race just reads the
// winner's file.
func (a *App) GetMachineId() string {
	path := filepath.Join(a.stateDir, "machine.id")
	if raw, err := os.ReadFile(path); err == nil {
		id := strings.TrimSpace(string(raw))
		if id != "" {
			return id
		}
	}
	bytes := make([]byte, 16)
	_, _ = rand.Read(bytes)
	id := hex.EncodeToString(bytes)
	_ = os.MkdirAll(a.stateDir, 0o755)
	tmp := path + ".tmp"
	if os.WriteFile(tmp, []byte(id), 0o644) == nil {
		_ = os.Rename(tmp, path)
	}
	return id
}