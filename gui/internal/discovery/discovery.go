// Package discovery lets machines running kvmshare on the local network
// find each other without typing IPs or ports.
//
// Two discovery channels, deliberately:
//
//   - Broadcast — each machine sends a tiny UDP datagram to the subnet
//     broadcast address. Broadcast is forwarded by essentially every
//     home/office router, whereas mDNS multicast is frequently blocked
//     (AP client isolation, smart-switch filtering). This is the primary
//     channel.
//   - mDNS — DNS-SD (_kvmshare._tcp) announce + browse, kept as a second
//     channel for networks where multicast works and broadcast is
//     filtered (some corporate networks do the opposite). Peers from
//     either channel land in the same map.
//
// On top of the beacons, the same UDP port carries a small command
// channel: subnet probes ("who is kvmshare here?", answered with a
// unicast beacon — the fallback for networks that filter broadcast AND
// multicast) and pairing requests ("connect here", delivered to the Host
// which owns the trust policy).
//
// The engine is duty-cycled, not free-running: all traffic lives inside
// bounded *sessions* owned by the coordinator (see coordinator.go). A
// session starts when the machine takes a role or the user acts, ends
// when its budget expires, and its outcome is reported — a session that
// hears nothing grounds the engine (idle) instead of churning forever.
// The loop bodies (engine.go, socket plumbing in targets.go/net.go)
// never decide lifetime; they only serve the current session.
//
// The engine owns networking and peer state only. Everything that is
// policy — what role this machine advertises, whether a pairing request
// is honored, where a pairing connect actually goes — lives behind the
// Host interface. That keeps the engine free of application state and
// the application free of sockets, and makes each side testable alone.
package discovery

import (
	"context"
	"fmt"
	"net"
	"sort"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"kvmshare/gui/internal/discovery/zeroconf"

	"kvmshare/gui/internal/ids"
)

// Port is the UDP port carrying discovery beacons, subnet probes and
// pairing commands. Deliberately the KVM port plus one, and independent
// of the KVM port itself: discovery must work whether or not a role is
// currently running. Keep in sync with config.defaultPort in the GUI.
const Port = 24801

// The mDNS service type all kvmshare machines share.
const serviceType = "_kvmshare._tcp"

// Peer sources, in the order channels prefer them.
const (
	SourceBroadcast = "broadcast"
	SourceProbe     = "probe" // reserved; beacons answer probes
	SourceMDNS      = "discovery"
)

// refreshRounds is how many announce+probe rounds one Refresh performs,
// and refreshRoundGap the spacing between them. Two rounds spaced by a
// couple of seconds cover a lost datagram or a peer that was mid-restart
// during the first — the manual action must succeed against ordinary
// packet loss, not just against a perfect network.
const (
	refreshRounds   = 2
	refreshRoundGap = 3 * time.Second
)

// sessionCheckInterval is how often the coordinator looks for a session
// past its deadline. One second: the finest visible timing is whole
// seconds of session budget, and the check is two mutex-guarded reads.
const sessionCheckInterval = time.Second

// A machine seen on the local network.
type Peer struct {
	ID     string `json:"id"`
	Name   string `json:"name"`
	Role   string `json:"role"` // "server" | "client"
	Addr   string `json:"addr"` // IP address (without port)
	Port   int    `json:"port"`
	Source string `json:"source"` // "broadcast" | "discovery" (mDNS)
	// Active reports whether the peer's advertised role is actually
	// running there (its GUI is up but no server/client process →
	// inactive). A machine with all services stopped must not appear as
	// a live "nearby" machine.
	Active bool `json:"active"`
}

// Host is the application the engine serves: the facts this machine
// advertises, and the single policy decision the engine cannot make —
// whether a pairing request should be honored.
type Host interface {
	// MachineID is this machine's stable id. The engine uses it to skip
	// its own announcements (every channel echoes them back).
	MachineID() string
	// MachineName is the friendly name advertised on the network.
	MachineName() string
	// ServerPort is the KVM port advertised to peers.
	ServerPort() int
	// LANAddr is this machine's first private IPv4 address, included in
	// pairing requests so the client knows where to reach the server.
	LANAddr() string
	// AdvertisedRole reports what is actually running here — what the
	// beacon must say, never what a mode dropdown says. A machine
	// running a server while set to "client" that broadcast "client,
	// not running" rendered as an idle ghost on every other machine.
	AdvertisedRole() (role string, running bool)
	// OnPairRequest delivers one pairing request ("connect here") from
	// the network. The engine has already normalized Addr to the
	// sender's real address; the host owns trust and the connect.
	// Called off the listener goroutine — it may block.
	OnPairRequest(req PairRequest)
}

// PairRequest is one "connect here" request, normalized for the host:
// Addr is the sender's address as routed (IP from the datagram, port
// from the payload hint), which is the only trustworthy addressing —
// a payload address could be forged.
type PairRequest struct {
	ID   string // the requesting server's machine id
	Name string // its display name, for messages
	Addr string // host:port to connect the client to
}

// Service is the discovery engine: one instance for the process
// lifetime. Sessions drive the loops (Seek/SetConnected/SetIdle);
// List, Refresh, Probe and Status are safe from any goroutine.
type Service struct {
	host Host

	mu    sync.Mutex
	peers map[string]*Peer     // keyed by machine id
	seen  map[string]time.Time // last contact per id, any channel
	reg   *zeroconf.Server

	// beaconConn sends broadcasts; listenConn receives beacons, probes
	// and pairing commands on the same port. Both are owned by the
	// current session's loops: a dead loop closes and nils its socket so
	// Refresh can see the gap and *rebuild it synchronously* — a manual
	// refresh never silently no-ops because a background loop had died
	// (the old Refresh skipped probing when the listener was down, which
	// is exactly when the user presses it).
	listenConn *net.UDPConn
	beaconConn *net.UDPConn

	// wg counts the loops of the current session. Stopping a session
	// waits for every loop to observe the cancel and exit before
	// returning, so two sessions can never overlap: no double beacons,
	// no torn-down socket used by a straggler.
	loopsWG sync.WaitGroup
	loopsMu sync.Mutex // serializes session start/stop vs loop rebuilds
	loopsOn atomic.Bool

	// sess is the duty-cycle state owned by the coordinator.
	sess sessionState

	// rootCtx spans the process lifetime: the listener and the pairing
	// worker select on it, so Close ends everything the engine started.
	rootCtx    context.Context
	rootCancel context.CancelFunc
	startOnce  sync.Once

	// The session's loop context (minted per session by loopContext) and
	// its guard. loopsCtxMu also serializes mint-vs-cancel so two
	// sessions can never hold live contexts at once.
	loopsCtxMu sync.Mutex
	loopsCancel context.CancelFunc

	// pairQueue carries pairing requests off the listener goroutine.
	// Handling a "connect" inline used to run the whole client-start
	// flow on the socket-read loop; a slow connect stalled every read
	// until the kernel buffer overflowed and discovery went deaf (a
	// 213 KB unread backlog was once observed live). The worker drops a
	// request when the queue is full (the sender retries with its next
	// beacon-period request) — never blocks the listener.
	pairQueue chan pairJob

	// Diagnostics state (see health.go) and the cached network targets
	// (see targets.go).
	warns    warnCounters
	netCache networkCache
}

// New creates the engine for `host`.
func New(host Host) *Service {
	ctx, cancel := context.WithCancel(context.Background())
	return &Service{
		host:       host,
		peers:      map[string]*Peer{},
		seen:       map[string]time.Time{},
		pairQueue:  make(chan pairJob, pairQueueLen),
		rootCtx:    ctx,
		rootCancel: cancel,
	}
}

// Start performs the process-lifetime wiring: the pairing worker (which
// must answer whenever a datagram arrives, whatever the session state)
// and the session-deadline watchdog. It does NOT start any traffic
// loops — a GUI that never takes a role stays silent by design. The
// host decides when discovery runs: Seek on role start, SetConnected
// once a partner is attached, SetIdle when nothing is running.
func (s *Service) Start() {
	s.startOnce.Do(func() {
		go s.pairWorker()
		go s.listenLoop(s.rootCtx)
		go s.watchLoop(s.rootCtx)
		go s.sessionLoop()
	})
}

// startLoopsLocked launches the current session's sender loops (the
// listener is process-scoped and already running). Called with
// s.loopsMu held (the session transition lock); the peer-map mutex is
// NOT held, so loops can record contacts immediately.
func (s *Service) startLoopsLocked(state Session) {
	if s.loopsOn.Swap(true) {
		return // already running — a rebuild path calls this too
	}
	ctx := s.loopContext()
	s.loopsWG.Add(3)
	go s.beaconLoop(ctx, state)
	go s.probeLoop(ctx)
	go s.browse(ctx) // mDNS: best-effort second channel
}

// stopLoopsLocked ends the current session's loops and waits for them.
// Called with s.loopsMu held. The wait is bounded by the loops'
// granularity: every loop selects on ctx.Done() at its tick, so the
// wait is milliseconds, not seconds.
func (s *Service) stopLoopsLocked() {
	if !s.loopsOn.Swap(false) {
		return // nothing running
	}
	s.cancelLoops()
	s.loopsWG.Wait()
	s.clearSockets()
}

// refresh locks the session transition and runs the full sweep while
// holding it: no loop rebuild can interleave, and the caller returns
// only after the fresh state is on the wire.
func (s *Service) refresh() error {
	s.loopsMu.Lock()
	defer s.loopsMu.Unlock()
	return s.sweepLocked()
}

// sweepLocked is the one true refresh body: rebuild whatever socket the
// session left broken, then announce + probe now. Returns an error
// instead of silently skipping — the manual path must never lie.
// Callers hold s.loopsMu.
func (s *Service) sweepLocked() error {
	// The listener is the socket probes are sent from and replies must
	// arrive on; without it a sweep is theater. Rebuild it on the spot.
	s.mu.Lock()
	listener := s.listenConn
	s.mu.Unlock()
	if listener == nil {
		if err := s.rebuildListener(); err != nil {
			return err
		}
	}
	s.invalidateTargets() // re-enumerate interfaces: networks change
	s.mu.Lock()
	s.peers = map[string]*Peer{}
	s.seen = map[string]time.Time{}
	s.mu.Unlock()
	// Announce and ask immediately — the next regular ticks are a full
	// interval away and a refresh should feel instant. The extra rounds
	// ride out ordinary packet loss without another user click.
	beacon := s.getBeaconSocket()
	if beacon == nil {
		return fmt.Errorf("discovery: no UDP socket available for announcing")
	}
	s.beaconTick(beacon)
	s.probeOnce()
	for i := 1; i < refreshRounds; i++ {
		time.Sleep(refreshRoundGap)
		s.beaconTick(beacon)
		s.probeOnce()
	}
	return nil
}

// Refresh forces a full discovery sweep and returns an error when the
// sweep could not run — the UI shows it instead of a silent no-op. The
// peer map is cleared first: what comes back is this sweep's truth, not
// a merge with the last session's guesses.
func (s *Service) Refresh() error {
	if err := s.refresh(); err != nil {
		return err
	}
	// A refresh is a statement of intent ("look for machines now");
	// make sure a session is actually listening for the answers.
	s.ensureSession()
	return nil
}

// ensureSession promotes an idle engine into a seeking session: a
// user-visible action (refresh) implies "discovery should be working".
// If a session is already running it simply continues.
func (s *Service) ensureSession() {
	s.sess.mu.Lock()
	defer s.sess.mu.Unlock()
	if s.sess.state != SessionIdle {
		return
	}
	s.sess.state = SessionSeeking
	s.sess.deadline = time.Now().Add(defaultSession)
	s.sess.expired = 0
	s.loopsMu.Lock()
	s.startLoopsLocked(SessionSeeking)
	s.loopsMu.Unlock()
}

// List returns the current peers, sorted by name for stable rendering.
func (s *Service) List() []Peer {
	s.expire()
	s.mu.Lock()
	defer s.mu.Unlock()
	out := make([]Peer, 0, len(s.peers))
	for _, p := range s.peers {
		out = append(out, *p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// PeerByID finds a peer by full or short (8-char) id, for actions that
// address a machine from the UI.
func (s *Service) PeerByID(id string) (Peer, bool) {
	for _, p := range s.List() {
		if p.ID == id || ids.ShortID(p.ID) == id {
			return p, true
		}
	}
	return Peer{}, false
}

// Close stops every session and releases the mDNS registration. The
// engine is single-shot for the process lifetime in the GUI, but a
// clean shutdown matters for embedders and tests.
func (s *Service) Close() {
	s.rootCancel()
	s.loopsMu.Lock()
	s.stopLoopsLocked()
	s.loopsMu.Unlock()
	s.sess.mu.Lock()
	s.sess.state = SessionIdle
	s.sess.mu.Unlock()
	s.mu.Lock()
	reg := s.reg
	s.reg = nil
	s.mu.Unlock()
	if reg != nil {
		reg.Shutdown()
	}
}

// itoa is strconv.Itoa, kept under one name for the wire helpers.
func itoa(n int) string { return strconv.Itoa(n) }
