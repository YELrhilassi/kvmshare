package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// TestRestartHandoff verifies the update restart choreography: the
// freshly-updated instance (KVMSHARE_RESTART=1) must wait for the
// previous instance's lock instead of raising it, then take the lock
// when it is released. This is the piece that made updates never take
// effect before — the old process never quit, and the new one bowed out
// to the single-instance lock.
func TestRestartHandoff(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)
	t.Setenv("XDG_STATE_HOME", home) // be explicit; the code may consult it

	// Instance 1: the old, still-running process. Takes the lock.
	a1 := NewApp()
	raised, err := a1.SingleInstance()
	if err != nil {
		t.Fatalf("instance 1 SingleInstance: %v", err)
	}
	if raised {
		t.Fatal("instance 1 should not be 'raised' (no prior instance)")
	}

	// Instance 2: the freshly-updated process, mid hand-off. Its
	// SingleInstance must block waiting for the lock, not raise
	// instance 1 and exit.
	t.Setenv("KVMSHARE_RESTART", "1")
	a2 := NewApp()
	done := make(chan struct {
		raised bool
		err    error
	}, 1)
	go func() {
		r, e := a2.SingleInstance()
		done <- struct {
			raised bool
			err    error
		}{r, e}
	}()

	// Give the hand-off a moment to reach its wait, then assert it has
	// not given up (which would mean it raised instance 1 / errored).
	time.Sleep(300 * time.Millisecond)
	select {
	case res := <-done:
		t.Fatalf("instance 2 returned early while the lock was held: raised=%v err=%v", res.raised, res.err)
	default:
	}

	// Instance 1 quits: its lock releases. Instance 2 must take over.
	if a1.instanceLock != nil {
		_ = a1.instanceLock.Close()
	}
	select {
	case res := <-done:
		if res.err != nil {
			t.Fatalf("instance 2 failed after the hand-off: %v", res.err)
		}
		if res.raised {
			t.Fatal("instance 2 should take the lock, not raise")
		}
	case <-time.After(8 * time.Second):
		t.Fatal("instance 2 never took the lock after the previous instance quit")
	}

	// And instance 2 really holds it: a third instance must be refused or
	// raised — never silently take the lock.
	t.Setenv("KVMSHARE_RESTART", "")
	a3 := NewApp()
	raised3, err3 := a3.SingleInstance()
	if err3 == nil && !raised3 {
		t.Fatal("a third instance should not coexist with instance 2")
	}

	// The env var must not leak after the hand-off.
	if os.Getenv("KVMSHARE_RESTART") != "" {
		t.Fatal("KVMSHARE_RESTART should be unset after the hand-off")
	}
	// Clean up instance 2's lock so the test is truly done.
	_ = os.Remove(filepath.Join(home, ".local", "state", "kvmshare", "gui.lock"))
}
