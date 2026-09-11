package notify

import (
	"os"
	"path/filepath"
	"sync"
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

// eventRecorder collects fire callbacks safely: the watcher fires each
// event in its own goroutine (a slow notification sink must never block
// the poll loop), so the plain slice the old test read back was a data
// race the race detector rightly flagged.
type eventRecorder struct {
	mu     sync.Mutex
	events []string
}

func (r *eventRecorder) record(name string, connected bool) {
	state := "disconnected"
	if connected {
		state = "connected"
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	r.events = append(r.events, name+"/"+state)
}

func (r *eventRecorder) len() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.events)
}

func (r *eventRecorder) at(i int) string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.events[i]
}

func (r *eventRecorder) all() []string {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]string(nil), r.events...)
}

// waitEvents blocks until at least `want` events arrived (or fails).
func (r *eventRecorder) waitEvents(t *testing.T, want int) {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for r.len() < want && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if r.len() < want {
		t.Fatalf("want >=%d events, got %v", want, r.all())
	}
}

// TestNotifyWatcher covers priming (no notifications for existing state),
// real transitions, duplicate markers and log truncation.
func TestNotifyWatcher(t *testing.T) {
	dir := t.TempDir()
	logPath := filepath.Join(dir, "server.log")

	n := New(logPath)
	var rec eventRecorder
	n.fire = rec.record

	// 1. Prime: existing connected state must not notify.
	appendLog(t, logPath, "kvmshare-server: layout 1 screens (local: pc), listening on :24800",
		"kvmshare-server: client hp connected")
	n.poll()
	if got := rec.len(); got != 0 {
		t.Fatalf("prime pass must not notify, got %v", rec.all())
	}
	if n.ConnectedCount() != 1 {
		t.Fatalf("want 1 client after prime, got %d", n.ConnectedCount())
	}

	// 2. A second client connects -> one event.
	appendLog(t, logPath, "kvmshare-server: client other connected")
	n.poll()
	rec.waitEvents(t, 1)
	if got := rec.at(0); got != "other/connected" {
		t.Fatalf("want [other/connected], got %v", rec.all())
	}
	if n.ConnectedCount() != 2 {
		t.Fatalf("want 2 clients, got %d", n.ConnectedCount())
	}

	// 3. Duplicate marker (server re-wrote the tail) -> no new event.
	appendLog(t, logPath, "kvmshare-server: client other connected")
	n.poll()
	// let any in-flight callback land before counting
	time.Sleep(20 * time.Millisecond)
	if got := rec.len(); got != 1 {
		t.Fatalf("duplicate marker must not notify, got %v", rec.all())
	}

	// 4. Disconnect -> event + count drops.
	appendLog(t, logPath, "kvmshare-server: client hp disconnected")
	n.poll()
	rec.waitEvents(t, 2)
	if got := rec.at(1); got != "hp/disconnected" {
		t.Fatalf("want [other/connected hp/disconnected], got %v", rec.all())
	}
	if n.ConnectedCount() != 1 {
		t.Fatalf("want 1 client after disconnect, got %d", n.ConnectedCount())
	}

	// 5. Truncated log (server restarted fresh) -> re-prime without events.
	if err := os.WriteFile(logPath, []byte("kvmshare-server: client hp connected\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	n.poll() // truncation detected here: re-prime
	n.poll() // now primed on the new file
	// give any stray callback a moment; re-prime must not notify
	time.Sleep(20 * time.Millisecond)
	if got := rec.len(); got != 2 {
		t.Fatalf("re-prime must not notify, got %v", rec.all())
	}
	if n.ConnectedCount() != 1 {
		t.Fatalf("want 1 client after re-prime, got %d", n.ConnectedCount())
	}
}

// TestNotifyNoLog is a no-crash check: a missing log must be a silent no-op.
func TestNotifyNoLog(t *testing.T) {
	n := New(filepath.Join(t.TempDir(), "does-not-exist.log"))
	n.poll()
	if n.ConnectedCount() != 0 {
		t.Fatalf("want 0 clients, got %d", n.ConnectedCount())
	}
	// run() + stop() must not deadlock or panic.
	n.Run()
	close(n.stop)
	time.Sleep(10 * time.Millisecond)
}
