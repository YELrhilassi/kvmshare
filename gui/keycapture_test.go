package main

import (
	"testing"
	"time"
)

// fakeSink records every emit so tests can assert what the page would
// have received.
type fakeSink struct {
	events []string // "name" per emit, in order
}

func (f *fakeSink) Emit(name string, _ ...any) bool {
	f.events = append(f.events, name)
	return false
}

func newFakeSink() *fakeSink { return &fakeSink{} }

// A session armed with a live sink suppresses under its token and
// reports every decision through that sink.
func TestArmSuppressesAndReports(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	if err := r.arm("tok", sink); err != nil {
		t.Fatalf("arm: %v", err)
	}
	token, suppress, got, expired := r.decideFor(time.Now())
	if token != "tok" || !suppress || expired || got == nil {
		t.Fatalf("decideFor = %q %v <nil=%v> %v, want tok/true/false", token, suppress, got == nil, expired)
	}
	r.report("tok", keyEvent{Token: "tok", VK: 0x5B})
	if len(sink.events) != 1 || sink.events[0] != "keycapture:tok" {
		t.Fatalf("sink events = %v, want one keycapture:tok", sink.events)
	}
}

// Arming without a token or without a sink is refused: a session that
// could not report would only swallow keys silently.
func TestArmRefusesBadRequests(t *testing.T) {
	r := newCaptureRegistry()
	if err := r.arm("", newFakeSink()); err != errCaptureTokenRequired {
		t.Fatalf("empty token: got %v", err)
	}
	if err := r.arm("tok", nil); err != errCaptureSinkUnavailable {
		t.Fatalf("nil sink: got %v", err)
	}
	if _, suppress, _, _ := r.decideFor(time.Now()); suppress {
		t.Fatal("refused requests must never suppress")
	}
}

// Disarm ends suppression immediately — the keystroke after Stop is
// the user's again.
func TestDisarmStopsSuppression(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	_ = r.arm("tok", sink)
	r.disarm("tok")
	if _, suppress, _, _ := r.decideFor(time.Now()); suppress {
		t.Fatal("suppression survived disarm")
	}
	// A stale token disarm is a no-op, not an error, and must not end
	// a newer session.
	_ = r.arm("tok2", sink)
	r.disarm("tok")
	if _, suppress, _, _ := r.decideFor(time.Now()); !suppress {
		t.Fatal("stale-token disarm killed the newer session")
	}
}

// A new arm replaces the old session: one session at a time, the
// newest token wins.
func TestArmReplacesSession(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	_ = r.arm("old", sink)
	_ = r.arm("new", sink)
	token, suppress, _, _ := r.decideFor(time.Now())
	if token != "new" || !suppress {
		t.Fatalf("decideFor = %q %v, want new/true", token, suppress)
	}
}

// A lapsed session ends on the next key event: suppression stops on
// that very event and the page is told once.
func TestTTLExpiresOnNextKey(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	_ = r.arm("tok", sink)
	past := time.Now().Add(captureTTL + time.Minute)
	token, suppress, got, expired := r.decideFor(past)
	if token != "tok" || suppress || !expired || got == nil {
		t.Fatalf("lapsed decideFor = %q suppress=%v expired=%v", token, suppress, expired)
	}
	if _, suppress, _, _ := r.decideFor(time.Now()); suppress {
		t.Fatal("suppression survived the lapse")
	}
}

// The watchdog ends a lapsed session with no keys in flight — the
// armed-but-page-gone case must still return the keyboard.
func TestWatchdogExpiresIdleSession(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	_ = r.arm("tok", sink)
	if _, _, ok := r.lapsed(time.Now()); ok {
		t.Fatal("fresh session lapsed early")
	}
	token, got, ok := r.lapsed(time.Now().Add(captureTTL + time.Minute))
	if !ok || token != "tok" || got == nil {
		t.Fatalf("lapsed = %q ok=%v", token, ok)
	}
	if _, suppress, _, _ := r.decideFor(time.Now()); suppress {
		t.Fatal("suppression survived the watchdog lapse")
	}
}

// Renew keeps a live session alive across the lease window.
func TestRenewExtends(t *testing.T) {
	sink := newFakeSink()
	r := newCaptureRegistry()
	_ = r.arm("tok", sink)
	future := time.Now().Add(captureTTL - time.Second)
	r.renew("tok")
	if _, suppress, _, expired := r.decideFor(future); !suppress || expired {
		t.Fatalf("renewed session lapsed early (suppress=%v expired=%v)", suppress, expired)
	}
	// A stale token must not renew a newer session's rival.
	_ = r.arm("tok2", sink)
	r.renew("tok")
	if _, suppress, _, _ := r.decideFor(time.Now()); !suppress {
		t.Fatal("stale renew killed the newer session")
	}
}
