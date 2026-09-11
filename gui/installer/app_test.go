package main

// Regression tests for the installer service's locking and progress
// reporting. The original bug: run() held in.mu across logf (which
// locks again — sync.Mutex is not reentrant), deadlocking the service
// the moment a run finished; the page then froze on its last rendered
// frame ("Installing 70%") even though the binaries were on disk.
// The second bug: progress only moved if the page polled, and WebView2
// throttles page timers — the service now pushes every state change.

import (
	"strings"
	"sync"
	"testing"
	"time"
)

// newTestInstaller returns a service wired to a test sink. The sink
// records every pushed snapshot and the time of the last push.
type testSink struct {
	mu     sync.Mutex
	snaps  []Snapshot
	last   time.Time
	onPush func() // optional hook, run inside the sink lock
}

func (s *testSink) push(snap Snapshot) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.snaps = append(s.snaps, snap)
	s.last = time.Now()
	if s.onPush != nil {
		s.onPush()
	}
}

func (s *testSink) count() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.snaps)
}

func (s *testSink) lastSnapshot() Snapshot {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.snaps[len(s.snaps)-1]
}

// newTestInstaller builds an Installer whose emissions go to sink. The
// zero service has no wails event manager — exactly the shape the
// attachEvents-less tests need; the emitter stands in for it.
func newTestInstaller(sink *testSink) *Installer {
	in := NewInstaller()
	in.mu.Lock()
	in.emitter = sink.push
	in.mu.Unlock()
	return in
}

// TestLaunchDoesNotDeadlock is the regression test for the freeze: the
// old Launch held in.mu and then called logf, which locks the same
// mutex — with the service locked, every Snapshot call blocked forever
// and the UI never updated again. Launch must complete (and emit) while
// the lock is free.
func TestLaunchDoesNotDeadlock(t *testing.T) {
	// Point the install dir at an empty prefix (the documented
	// KVMSHARE_PREFIX override) so the test never touches (or launches)
	// a real install.
	t.Setenv("KVMSHARE_PREFIX", t.TempDir())
	sink := &testSink{}
	in := newTestInstaller(sink)

	done := make(chan struct{})
	go func() {
		defer close(done)
		in.Launch()
	}()

	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatal("Launch deadlocked (logf while holding the service lock)")
	}

	// Whatever the platform's launchGUI decided (Windows errors on a
	// missing exe; Linux is a no-op), the outcome must have been logged
	// through the push channel — the deadlock made both impossible.
	if sink.count() == 0 {
		t.Fatal("Launch must push its outcome, nothing was emitted")
	}
	last := sink.lastSnapshot()
	if len(last.Log) == 0 {
		t.Fatalf("Launch must log its outcome, log empty (error=%q)", last.Error)
	}
	_ = strings.TrimSpace // keep the import honest if assertions evolve
}

// TestRunLogsAndFinishes drives the full state machine through a fake
// action that logs while running (as the install engine does) and must
// end Done with the log delivered — under the old code the success path
// deadlocked on its own logf after the action returned.
func TestRunLogsAndFinishes(t *testing.T) {
	sink := &testSink{}
	in := newTestInstaller(sink)

	finished := make(chan error, 1)
	go func() {
		// run is unexported and synchronous — call it via InstallLatest's
		// machinery by invoking run directly.
		in.run("Install", func() error {
			in.logf("fake phase line") // the call that used to deadlock the success path
			return nil
		})
		finished <- nil
	}()

	select {
	case <-finished:
	case <-time.After(5 * time.Second):
		t.Fatal("run deadlocked on its success path")
	}

	snap := in.Snapshot()
	if !snap.Done || snap.Busy {
		t.Fatalf("run must end done and idle, got busy=%v done=%v", snap.Busy, snap.Done)
	}
	if snap.Phase != "Done" {
		t.Fatalf("phase must be Done, got %q", snap.Phase)
	}
	found := false
	for _, l := range snap.Log {
		if l == "fake phase line" {
			found = true
		}
	}
	if !found {
		t.Fatalf("log line lost: %v", snap.Log)
	}
	if sink.count() == 0 {
		t.Fatal("state changes must be pushed, none were")
	}
}

// TestProgressIsThrottledAndFlushed checks the emission contract: rapid
// progress ticks coalesce (at most one per interval), but the final tick
// of a phase is never lost to the throttle.
func TestProgressIsThrottledAndFlushed(t *testing.T) {
	sink := &testSink{}
	in := newTestInstaller(sink)

	in.mu.Lock()
	// A burst of byte-progress callbacks inside one interval.
	in.phase = "Downloading v-test"
	for i := 0; i <= 100; i++ {
		in.phasefLocked("Downloading v-test", float64(i)/100.0)
	}
	burstPushes := sink.count()
	in.flushProgressLocked()
	in.mu.Unlock()

	snap := in.Snapshot()
	if snap.Progress != 1.0 {
		t.Fatalf("flushed progress must be the freshest (1.0), got %v", snap.Progress)
	}
	// Coalescing bound: the burst is ~250 ms of wall time; pushes must be
	// far fewer than ticks (a 1 ms burst could legitimately push a
	// couple, hence the generous bound).
	if burstPushes > 10 {
		t.Fatalf("progress ticks must coalesce: %d pushes for 101 ticks", burstPushes)
	}
}

// TestPhaseChangeEmitsImmediately verifies a phase label change always
// pushes (never throttled away) — the user must see Downloading →
// Verifying → Installing → Done even if each lasts under one interval.
func TestPhaseChangeEmitsImmediately(t *testing.T) {
	sink := &testSink{}
	in := newTestInstaller(sink)

	for _, p := range []struct {
		label string
		frac  float64
	}{{"Downloading v-test", 0.5}, {"Verifying checksum", 0.6}, {"Installing", 0.7}, {"Done", 1.0}} {
		in.mu.Lock()
		in.phasefLocked(p.label, p.frac)
		in.mu.Unlock()
	}

	want := []string{"Downloading v-test", "Verifying checksum", "Installing", "Done"}
	got := 0
	var labels []string
	sink.mu.Lock()
	for _, s := range sink.snaps {
		if len(labels) == 0 || labels[len(labels)-1] != s.Phase {
			labels = append(labels, s.Phase)
		}
	}
	sink.mu.Unlock()
	for _, w := range want {
		if len(labels) > got && labels[got] == w {
			got++
		}
	}
	if got != len(want) {
		t.Fatalf("phase transitions must each emit; wanted %v, saw %v", want, labels)
	}
}
