package discovery

import (
	"time"
)

// coordinator.go — the duty-cycle owner.
//
// The engine's loops are no longer free-running: every loop is born from
// and dies with a *session*, and this coordinator is the only code that
// starts or ends sessions. That gives the guarantees the product needs:
//
//   - a session always ends (deadline), successful or not — nothing runs
//     forever in the background;
//   - ending a session is synchronous: when Seek/SetConnected/refresh
//     return, the old session's loops have been torn down and a new one
//     is already announcing — no overlap, no gap without an owner;
//   - a session that never hears a peer is *grounded*: the engine stops
//     (Idle) and says so, instead of silently churning.

// Seek starts a seeking session at the full discovery rate, replacing
// whatever is running. Called when the machine starts a role and when
// auto-connect wants candidates — the moments "who is out there" becomes
// a live question. Idempotent in effect: seeking while seeking just
// extends the current session.
func (s *Service) Seek() { s.startSession(SessionSeeking, defaultSession) }

// SetConnected switches to the connected session rate: this machine has
// a partner, so discovery's job shrinks to staying visible to it.
// Called on connect; the session self-renews while connected and is what
// keeps the partner's peer entry (and probe replies) alive.
func (s *Service) SetConnected() { s.startSession(SessionConnected, connectedSession) }

// SetIdle stops every session. Called when the machine drops its role
// with no counterpart to stay visible to — the loops end *now*, and the
// engine says it is idle instead of pretending to listen.
func (s *Service) SetIdle() { s.stopSessionLocked("idle") }

// startSession replaces the current session synchronously: stop the
// old one (loops torn down), then start the new one before returning.
// Requesting the state already running only extends the budget — the
// sync path calls these setters every state-loop tick, and re-spinning
// loops that are doing exactly what is asked would be churn, not
// discovery.
func (s *Service) startSession(state Session, duration time.Duration) {
	s.sess.mu.Lock()
	defer s.sess.mu.Unlock()
	if s.sess.state == state {
		s.sess.deadline = time.Now().Add(duration)
		return
	}
	s.stopLoopsTransient()
	s.sess.state = state
	s.sess.deadline = time.Now().Add(duration)
	s.sess.expired = 0
	s.startLoopsTransient(state)
}

// stopSessionLocked ends the current session: loops down, state Idle,
// the outcome recorded. Callers hold s.sess.mu.
func (s *Service) stopSessionLocked(reason string) {
	if s.sess.state == SessionIdle {
		return
	}
	s.stopLoopsTransient()
	s.sess.state = SessionIdle
	s.sess.stoppedAt = time.Now()
	s.warns.session.log("discovery: session ended ("+reason+") — going idle", nil, warnCooldown)
}

// stopLoopsTransient and startLoopsTransient are the coordinator's
// handles on the loop lifecycle: they take the session-transition lock
// (loopsMu) so callers holding sess.mu cannot violate the
// sess.mu → loopsMu ordering the rest of the package relies on.
func (s *Service) stopLoopsTransient() {
	s.loopsMu.Lock()
	s.stopLoopsLocked()
	s.loopsMu.Unlock()
}

func (s *Service) startLoopsTransient(state Session) {
	s.loopsMu.Lock()
	s.startLoopsLocked(state)
	s.loopsMu.Unlock()
}

// sessionLoop enforces the deadline: when a session outlives its budget
// the outcome decides what happens next.
//
//   - Seeking and a peer was heard (or a session is connected): renew at
//     the same rate — the network is alive and worth listening to.
//   - Seeking and nothing was ever heard: ground the engine. expired
//     counts the consecutive failures; one more Seek resets it.
func (s *Service) sessionLoop() {
	t := time.NewTicker(sessionCheckInterval)
	defer t.Stop()
	for {
		select {
		case <-s.rootCtx.Done():
			return
		case <-t.C:
		}
		s.sess.mu.Lock()
		if s.sess.state == SessionIdle || time.Now().Before(s.sess.deadline) {
			s.sess.mu.Unlock()
			continue
		}
		connected := s.sess.state == SessionConnected
		heard := s.anyPeerHeard()
		if connected || heard {
			s.sess.deadline = time.Now().Add(connectedSession)
			s.sess.mu.Unlock()
			continue
		}
		s.sess.expired++
		s.stopSessionLocked("no answer")
		s.sess.mu.Unlock()
	}
}

// anyPeerHeard reports whether any live peer has been contacted inside
// the current TTL window — the "the session found something" signal the
// renewal decision needs. Read-only on the peer map.
func (s *Service) anyPeerHeard() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.seen) > 0
}

// Status is the operator-visible truth about the duty cycle: what the
// engine is doing right now and what the last ended session concluded.
// The UI renders this verbatim — the "stops when it fails" guarantee is
// something the user can see, not a promise in a comment.
type Status struct {
	// State: idle (nothing running), seeking (hunting at full rate),
	// connected (partner present, low rate).
	State string `json:"state"`
	// Consecutive failed sessions: 0 while healthy, ≥1 after silent
	// sessions with no peer heard.
	FailedSessions int `json:"failedSessions"`
	// Seconds left in the current session (0 when idle).
	SecondsLeft int `json:"secondsLeft"`
}

// Status snapshots the duty-cycle state.
func (s *Service) Status() Status {
	s.sess.mu.Lock()
	defer s.sess.mu.Unlock()
	st := Status{State: s.sess.state.String(), FailedSessions: s.sess.expired}
	if !s.sess.deadline.IsZero() {
		if d := time.Until(s.sess.deadline); d > 0 {
			st.SecondsLeft = int(d.Seconds())
		}
	}
	return st
}
