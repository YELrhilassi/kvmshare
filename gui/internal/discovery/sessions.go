package discovery

import (
	"sync"
	"time"
)

// Session timing. The engine does not beacon forever: discovery runs in
// bounded sessions, and a session has exactly one owner at a time.
//
//	defaultSession is long enough to ride out a beacon-loss burst and
//	still find a machine that is seconds from appearing — the same
//	budget a user implicitly grants by opening the app or pressing
//	Refresh. When nothing answers by then the session ends and says so;
//	it never leaves loops running in the background hoping.
//
//	connectedSession is the steady-state "I have what I came for" rate:
//	one beacon per interval keeps the partner's peer entry truthful and
//	answers its probes, at a packet rate that is unmeasurable.
//
//	Budget arithmetic: one session is 30 beacon periods, each sweep
//	reaches every subnet host, so a reachable machine that answers
//	anything at all is found within one session — three full subnet
//	sweeps inside the window. A failing session is *silent after it
//	ends*, not silent-and-burning-CPU forever.
const (
	defaultSession    = 60 * time.Second
	connectedSession  = 60 * time.Second
	beaconInterval    = 2 * time.Second
	probeInterval     = 10 * time.Second
	peerTTL           = 45 * time.Second
	connectedInterval = 15 * time.Second
)

// listenRetry is how long listenLoop waits before rebuilding its socket
// after a bind failure (port taken by another app — including another
// kvmshare instance a previous session left behind). Long enough to let
// the conflicting process exit, short enough that recovery feels
// automatic.
const listenRetry = 5 * time.Second

// Pairing work runs on its own goroutine so the listener never blocks
// on it. Depth covers a brief connect stall; overflow drops (senders
// retry naturally with their next request), and each job carries a
// deadline so a queued request can't act on stale state.
const (
	pairQueueLen  = 8
	pairJobMaxAge = 10 * time.Second
)

// Session is the duty-cycle state. The zero value is Idle: nothing is
// running and the engine owes nobody a packet. The state machine is
// linear — every state is entered through Seek/SetConnected/refresh and
// left through session expiry or the same setters — so each transition
// is a two-line rule below and the whole machine fits in one screen.
type Session int

const (
	// SessionIdle — no loops; the last session ended and said why.
	SessionIdle Session = iota
	// SessionSeeking — full-rate beaconing + probing, hunting peers.
	SessionSeeking
	// SessionConnected — a session is live at the connected rate.
	SessionConnected
)

// String renders the session state for the status API.
func (s Session) String() string {
	switch s {
	case SessionSeeking:
		return "seeking"
	case SessionConnected:
		return "connected"
	default:
		return "idle"
	}
}

// sessionState is the authoritative duty-cycle state, owned by the
// coordinator. `deadline` bounds the current session; `expired` counts
// the consecutive sessions that ended with no peer ever heard — the
// "stops when it fails" ground the user can see in Status.
type sessionState struct {
	mu        sync.Mutex
	state     Session
	deadline  time.Time
	expired   int
	stoppedAt time.Time // when the last session ended (for status wording)
}
