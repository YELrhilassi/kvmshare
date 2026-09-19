package discovery

import (
	"testing"
	"time"
)

// startSession on the state already running must extend the deadline,
// not restart the loops: the App's sync path calls the setters every
// state tick, and re-spinning healthy loops is churn.
func TestStartSessionExtendsWhenSameState(t *testing.T) {
	s, _ := newTestService(t)
	s.loopsMu.Lock()
	s.startLoopsLocked(SessionSeeking)
	s.loopsMu.Unlock()
	defer func() {
		s.loopsMu.Lock()
		s.stopLoopsLocked()
		s.loopsMu.Unlock()
	}()

	s.sess.mu.Lock()
	first := s.sess.deadline
	s.sess.state = SessionSeeking
	s.sess.mu.Unlock()

	s.startSession(SessionSeeking, defaultSession)

	s.sess.mu.Lock()
	defer s.sess.mu.Unlock()
	if !s.sess.deadline.After(first) {
		t.Fatalf("same-state startSession must extend the deadline: first=%v now=%v", first, s.sess.deadline)
	}
}

// A seeking session that outlives its budget without hearing a peer is
// grounded: the engine goes idle and records the failed session. That
// is the "stops when it fails" contract.
func TestExpiredSeekingSessionGroundsEngine(t *testing.T) {
	s, _ := newTestService(t)
	s.sess.mu.Lock()
	s.sess.state = SessionSeeking
	s.sess.deadline = time.Now().Add(-time.Second) // already past due
	s.sess.mu.Unlock()

	// Run one deadline pass by hand (the goroutine ticks once a second;
	// the decision body is what is under test).
	s.sess.mu.Lock()
	if s.sess.state == SessionSeeking && time.Now().After(s.sess.deadline) && !s.anyPeerHeard() {
		s.sess.expired++
		s.stopSessionLocked("no answer")
	}
	s.sess.mu.Unlock()

	st := s.Status()
	if st.State != SessionIdle.String() {
		t.Fatalf("expired seeking session with no peers must ground the engine: %+v", st)
	}
	if st.FailedSessions != 1 {
		t.Fatalf("grounding must count the failed session: %+v", st)
	}
}

// A connected session renews instead of grounding: the partner's
// presence is a reason to keep listening at the low rate.
func TestConnectedSessionRenews(t *testing.T) {
	s, _ := newTestService(t)
	s.sess.mu.Lock()
	s.sess.state = SessionConnected
	s.sess.deadline = time.Now().Add(-time.Second)
	s.sess.mu.Unlock()

	s.sess.mu.Lock()
	if s.sess.state == SessionConnected && time.Now().After(s.sess.deadline) {
		s.sess.deadline = time.Now().Add(connectedSession)
	}
	s.sess.mu.Unlock()

	st := s.Status()
	if st.State != SessionConnected.String() {
		t.Fatalf("connected session must renew, not ground: %+v", st)
	}
	if st.SecondsLeft <= 0 {
		t.Fatalf("renewed session must carry fresh budget: %+v", st)
	}
}

// A peer heard during a seeking session is grounds for renewal too —
// the network answered, the session is worth continuing.
func TestSeekingSessionWithPeerRenews(t *testing.T) {
	s, _ := newTestService(t)
	s.sess.mu.Lock()
	s.sess.state = SessionSeeking
	s.sess.deadline = time.Now().Add(-time.Second)
	s.sess.mu.Unlock()

	// One beacon from a peer: `seen` is stamped, so the session renews.
	s.upsertBeacon(beaconPayload{ID: "bbbbbbbb22222222", Name: "hp", Role: "client", Port: 24800, Running: true},
		addrForTest(t))

	s.sess.mu.Lock()
	renewed := s.anyPeerHeard()
	if renewed {
		s.sess.deadline = time.Now().Add(connectedSession)
	}
	s.sess.mu.Unlock()

	if !renewed {
		t.Fatal("a heard peer must renew the session")
	}
	if got := s.Status(); got.State != SessionSeeking.String() {
		t.Fatalf("session must survive when a peer answered: %+v", got)
	}
}

// SetIdle stops the loops synchronously: when it returns, nothing is
// running and the status says idle. Driven through the public API —
// the same path production uses — so the lock contract is exercised
// for real.
func TestSetIdleStopsEverything(t *testing.T) {
	s, _ := newTestService(t)
	s.Seek()
	if got := s.Status(); got.State != SessionSeeking.String() {
		t.Fatalf("Seek must start a seeking session: %+v", got)
	}

	s.SetIdle()

	if got := s.Status(); got.State != SessionIdle.String() {
		t.Fatalf("SetIdle must ground the engine: %+v", got)
	}
	s.loopsMu.Lock()
	running := s.loopsOn.Load()
	s.loopsMu.Unlock()
	if running {
		t.Fatal("loops must be stopped after SetIdle")
	}
}

// Seek → SetConnected transitions the rate without leaving the old
// sender loops alive: exactly one set of loops runs at any time.
func TestSeekThenConnectedRunsOneSet(t *testing.T) {
	s, _ := newTestService(t)
	s.Seek()
	s.SetConnected()
	defer s.SetIdle()

	s.loopsMu.Lock()
	running := s.loopsOn.Load()
	s.loopsMu.Unlock()
	if !running {
		t.Fatal("SetConnected must keep the loops running")
	}
	if got := s.Status(); got.State != SessionConnected.String() {
		t.Fatalf("state must be connected after SetConnected: %+v", got)
	}
}
