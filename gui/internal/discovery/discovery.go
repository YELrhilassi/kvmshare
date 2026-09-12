// Package discovery lets machines running kvmshare on the local network
// find each other without typing IPs or ports.
//
// Two discovery channels, deliberately:
//
//   - Broadcast — every 2 s each machine sends a tiny UDP datagram to
//     the subnet broadcast address. Broadcast is forwarded by
//     essentially every home/office router, whereas mDNS multicast is
//     frequently blocked (AP client isolation, smart-switch filtering).
//     This is the primary channel.
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
// The engine owns networking and peer state only. Everything that is
// policy — what role this machine advertises, whether a pairing request
// is honored, where a pairing connect actually goes — lives behind the
// Host interface. That keeps the engine free of application state and
// the application free of sockets, and makes each side testable alone.
package discovery

import (
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
// lifetime. Start launches the loops; List and Refresh are safe from
// any goroutine.
type Service struct {
	host Host

	mu    sync.Mutex
	peers map[string]*Peer     // keyed by machine id
	seen  map[string]time.Time // last contact per id, any channel
	reg   *zeroconf.Server

	// beaconConn sends broadcasts; listenConn receives beacons, probes
	// and pairing commands on the same port. Both are owned by the loops
	// that created them: a dead loop closes and nils its socket so the
	// diagnostics watchLoop can see the gap and heal it, instead of the
	// whole channel silently dying for the process lifetime.
	listenConn *net.UDPConn
	beaconConn *net.UDPConn

	started sync.Once
	active  atomic.Bool   // set by Start; gates re-advertising before then
	stop    chan struct{} // closed never today (the engine lives for the process); loops select on it

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
	return &Service{
		host:      host,
		peers:     map[string]*Peer{},
		seen:      map[string]time.Time{},
		stop:      make(chan struct{}),
		pairQueue: make(chan pairJob, pairQueueLen),
	}
}

// Start launches beaconing, the receiver, the unicast subnet probe,
// the mDNS channel and the diagnostics watch. Idempotent; later calls
// are no-ops (a role change re-advertises via Republish instead).
func (s *Service) Start() {
	s.started.Do(func() {
		s.active.Store(true)
		go s.pairWorker()
		go s.beaconLoop()
		go s.listenLoop() // self-healing wrapper around the receive path
		go s.probeLoop()  // unicast fallback where broadcast/multicast is filtered
		go s.browse()     // mDNS: best-effort second channel
		go s.watchLoop()  // diagnostics: loud when discovery is unhealthy
		s.Republish()
	})
}

// Refresh clears discovery state and forces an immediate sweep: peers
// re-announced now, subnet re-probed now, interfaces re-enumerated now.
// Backs the UI's refresh button — the user-visible answer to "the list
// looks stale", without waiting out the next probe interval.
func (s *Service) Refresh() {
	s.invalidateTargets()
	s.mu.Lock()
	s.peers = map[string]*Peer{}
	s.seen = map[string]time.Time{}
	beacon := s.beaconConn
	listener := s.listenConn
	s.mu.Unlock()
	// Announce and ask immediately — the next regular ticks are a full
	// interval away and a refresh should feel instant.
	if beacon != nil {
		s.beaconTick(beacon)
	}
	if listener != nil {
		s.probeOnce()
	}
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

// Close stops every loop the engine started and releases the mDNS
// registration. The engine is single-shot for the process lifetime in
// the GUI, but a clean shutdown matters for embedders and tests.
func (s *Service) Close() {
	select {
	case <-s.stop:
		return // already closed
	default:
	}
	close(s.stop)
	s.mu.Lock()
	reg := s.reg
	s.reg = nil
	listen := s.listenConn
	beacon := s.beaconConn
	s.mu.Unlock()
	if reg != nil {
		reg.Shutdown()
	}
	if listen != nil {
		listen.Close()
	}
	if beacon != nil {
		beacon.Close()
	}
}

// itoa is strconv.Itoa, kept under one name for the wire helpers.
func itoa(n int) string { return strconv.Itoa(n) }
