package main

import (
	"os"
	"path/filepath"
	"testing"
	"time"
)

// appendLog appends lines to the log file (real servers append).
func appendLog(t *testing.T, path string, lines ...string) {
	t.Helper()
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	for _, l := range lines {
		if _, err := f.WriteString(l + "\n"); err != nil {
			t.Fatal(err)
		}
	}
}

// TestNotifyWatcher covers priming (no notifications for existing state),
// real transitions, duplicate markers and log truncation.
func TestNotifyWatcher(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "server.log")

	n := newNotify(logPath)
	var events []string
	// fire runs in a goroutine by design; wait for it with a deadline.
	waitEvents := func(want int) {
		t.Helper()
		deadline := time.Now().Add(2 * time.Second)
		for len(events) < want && time.Now().Before(deadline) {
			time.Sleep(5 * time.Millisecond)
		}
		if len(events) < want {
			t.Fatalf("want >=%d events, got %v", want, events)
		}
	}
	n.fire = func(name string, connected bool) {
		state := "disconnected"
		if connected {
			state = "connected"
		}
		events = append(events, name+"/"+state)
	}

	// 1. Prime: existing connected state must not notify.
	appendLog(t, logPath, "kvmshare-server: layout 1 screens (local: pc), listening on :24800",
		"kvmshare-server: client hp connected")
	n.poll()
	if len(events) != 0 {
		t.Fatalf("prime pass must not notify, got %v", events)
	}
	if n.connectedCount() != 1 {
		t.Fatalf("want 1 client after prime, got %d", n.connectedCount())
	}

	// 2. A second client connects -> one event.
	appendLog(t, logPath, "kvmshare-server: client other connected")
	n.poll()
	waitEvents(1)
	if events[0] != "other/connected" {
		t.Fatalf("want [other/connected], got %v", events)
	}
	if n.connectedCount() != 2 {
		t.Fatalf("want 2 clients, got %d", n.connectedCount())
	}

	// 3. Duplicate marker (server re-wrote the tail) -> no new event.
	appendLog(t, logPath, "kvmshare-server: client other connected")
	n.poll()
	if len(events) != 1 {
		t.Fatalf("duplicate marker must not notify, got %v", events)
	}

	// 4. Disconnect -> event + count drops.
	appendLog(t, logPath, "kvmshare-server: client hp disconnected")
	n.poll()
	waitEvents(2)
	if events[1] != "hp/disconnected" {
		t.Fatalf("want [other/connected hp/disconnected], got %v", events)
	}
	if n.connectedCount() != 1 {
		t.Fatalf("want 1 client after disconnect, got %d", n.connectedCount())
	}

	// 5. Truncated log (server restarted fresh) -> re-prime without events.
	if err := os.WriteFile(logPath, []byte("kvmshare-server: client hp connected\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	n.poll() // truncation detected here: re-prime
	n.poll() // now primed on the new file
	// give any stray goroutine a moment; re-prime must not notify
	time.Sleep(20 * time.Millisecond)
	if len(events) != 2 {
		t.Fatalf("re-prime must not notify, got %v", events)
	}
	if n.connectedCount() != 1 {
		t.Fatalf("want 1 client after re-prime, got %d", n.connectedCount())
	}
}

// TestNotifyNoLog is a no-crash check: a missing log must be a silent no-op.
func TestNotifyNoLog(t *testing.T) {
	n := newNotify(filepath.Join(t.TempDir(), "does-not-exist.log"))
	n.poll()
	if n.connectedCount() != 0 {
		t.Fatalf("want 0 clients, got %d", n.connectedCount())
	}
	// run() + stop() must not deadlock or panic.
	n.run()
	close(n.stop)
	time.Sleep(10 * time.Millisecond)
}
