package main

import (
	"os"
	"path/filepath"
	"testing"
)

// The live-state stream must be quiet: an unchanged snapshot emits
// nothing (no event, no re-render), and any change emits exactly once.
func TestShouldEmitDedupes(t *testing.T) {
	a, _ := newTestApp(t)
	base := LiveSnapshot{Mode: ModeServer, Running: runningSnapshot{Server: false, Client: false}}

	if !a.shouldEmit(base) {
		t.Fatal("first snapshot should emit (nothing sent yet)")
	}
	if a.shouldEmit(base) {
		t.Fatal("unchanged snapshot must not emit")
	}

	changed := base
	changed.Running.Server = true
	if !a.shouldEmit(changed) {
		t.Fatal("changed snapshot should emit")
	}
	if a.shouldEmit(changed) {
		t.Fatal("repeated changed snapshot must not emit again")
	}
}

// The client state must never lie: a stale "connected" state file with
// no client process must read as disconnected, and a running client
// that has not written a state file yet must read as connecting.
func TestReconciledClientState(t *testing.T) {
	a, _ := newTestApp(t)
	statePath := filepath.Join(a.stateDir, "client.state")

	// No process, stale file → disconnected (the file outlives a kill).
	if err := os.WriteFile(statePath, []byte("status=connected\nserver=192.168.1.86:24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if cs := a.reconciledClientState(false); cs.Status != "disconnected" {
		t.Fatalf("no process + stale file = %q, want disconnected", cs.Status)
	}

	// Process running, file says connected → connected.
	if cs := a.reconciledClientState(true); cs.Status != "connected" {
		t.Fatalf("running + connected file = %q, want connected", cs.Status)
	}

	// Process running, no file yet → connecting.
	_ = os.Remove(statePath)
	if cs := a.reconciledClientState(true); cs.Status != "connecting" {
		t.Fatalf("running + no file = %q, want connecting", cs.Status)
	}
}

// The client's real connection state comes from client.state (written by
// the Rust client). Missing file means "disconnected".
func TestClientStatusReadsStateFile(t *testing.T) {
	a, _ := newTestApp(t)

	if st := a.ClientStatus(); st.Status != "disconnected" {
		t.Fatalf("missing state file should read as disconnected, got %+v", st)
	}

	statePath := filepath.Join(a.stateDir, "client.state")
	if err := os.WriteFile(statePath, []byte("status=connected\nserver=192.168.1.86:24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	st := a.ClientStatus()
	if st.Status != "connected" || st.Server != "192.168.1.86:24800" {
		t.Fatalf("state file parse = %+v", st)
	}

	if err := os.WriteFile(statePath, []byte("status=bogus\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if st := a.ClientStatus(); st.Status != "disconnected" {
		t.Fatalf("unknown status should fall back to disconnected, got %+v", st)
	}
}
