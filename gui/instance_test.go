package main

import (
	"fmt"
	"os"
	"os/signal"
	"runtime"
	"testing"
)

func TestSingleInstanceLockAndRaise(t *testing.T) {
	// Raising a hidden window via a signal is a Unix mechanism; Windows
	// has no signals and falls back to the plain "already running"
	// error, which TestSingleInstanceLock covers separately.
	if runtime.GOOS == "windows" {
		t.Skip("signal-based raise is Unix-only")
	}
	// A second launch raises the first with SIGUSR2 (never SIGUSR1 —
	// JavaScriptCore uses that for GC). In this test both "instances" are
	// the same process, so ignore the signal (its default action would
	// terminate the test) and assert the raise contract.
	signal.Ignore(raiseSignal)

	a, _ := newTestApp(t)
	if raised, err := a.SingleInstance(); err != nil || raised {
		t.Fatalf("first instance should hold the lock quietly, got raised=%v err=%v", raised, err)
	}
	b := NewApp() // second instance, same HOME -> same lock file
	raised, err := b.SingleInstance()
	if err != nil {
		t.Fatalf("second instance should raise the first and exit quietly, got: %v", err)
	}
	if !raised {
		t.Fatal("second instance must report that it raised the running one")
	}
}

func TestSingleInstanceWritesPid(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.SingleInstance(); err != nil {
		t.Fatalf("first instance should get the lock: %v", err)
	}
	// The pid lives in the dedicated pid file (the byte-range lock on
	// the lock file blocks reads of it on Windows), and must be the
	// value a later instance reads to raise this one.
	raw, err := os.ReadFile(a.rolePidPath("gui"))
	if err != nil {
		t.Fatalf("pid file readable: %v", err)
	}
	var pid int
	if _, err := fmt.Sscanf(string(raw), "%d", &pid); err != nil || pid <= 1 {
		t.Fatalf("pid file should record our pid, got %q", raw)
	}
	if pid != os.Getpid() {
		t.Fatalf("pid file records %d, want %d", pid, os.Getpid())
	}
}
