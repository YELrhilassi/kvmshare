package main

// Network discovery: machines running kvmshare on the local network find
// each other without typing IPs or ports.
//
// Two discovery channels, deliberately:
//
//   - Broadcast — every 2 s each machine sends a tiny UDP datagram to the
//     subnet broadcast address (`discoveryPort`): its id, name, role and
//     port. Broadcast is forwarded by essentially every home/office
//     router, whereas mDNS multicast is frequently blocked (AP client
//     isolation, smart-switch filtering). This is the primary channel.
//   - mDNS     — DNS-SD (`_kvmshare._tcp`) announce + browse, kept as a
//     second channel for networks where multicast works and broadcast
//     is filtered (some corporate networks do the opposite). Peers from
//     either channel land in the same map.
//
// Pairing: the same UDP port carries a small command channel. A server
// operator clicks \"connect here\" on a discovered client, and the client
// (if it accepts pairing) starts its own client pointed at that server —
// no mouse-plugging or IP typing on the client machine. Trust is
// first-use: the first request from a server is accepted and that
// server's id is remembered, so the second time (and every time after)
// it is already trusted. The user can disable pairing entirely in
// Client settings (acceptPairing off + no trusted servers → strangers'
// requests are dropped silently).
//
// Manual IP:port connection always remains as the fallback — discovery
// is a convenience on top, never a requirement.

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/grandcat/zeroconf"
)

// The mDNS service type all kvmshare machines share.
const discoveryService = "_kvmshare._tcp"

// discoveryPort carries discovery beacons and pairing commands (UDP).
// Deliberately the KVM port plus one, and independent of the KVM port
// itself: discovery must work whether or not a role is currently running.
const discoveryPort = defaultPort + 1

// How often a beacon is broadcast, how often the subnet is probed over
// unicast, and how long a peer may go silent before it is dropped.
// peerTTL is several beacon periods times a margin for burst loss: on
// Wi-Fi, broadcast frames are sent at the base rate and unacked, so a
// short burst of interference used to expire a live peer (visible as
// the peer list flapping between populated and empty). The unicast
// probe channel stamps the same liveness map, so a peer that survives
// on either channel never ages out.
const beaconInterval = 2 * time.Second
const probeInterval = 10 * time.Second
const peerTTL = 45 * time.Second

// Pairing work runs on its own goroutine so the listener never blocks
// on it. Depth covers a brief connect stall; overflow drops (senders
// retry naturally with their next request), and each job carries a
// deadline so a queued request can't act on stale state.
const pairQueueLen = 8

// How long a queued pairing request stays actionable.
const pairJobMaxAge = 10 * time.Second

// Health-watch cadence and thresholds. quietAfter must comfortably
// exceed beaconInterval: two live machines exchange beacons every 2 s,
// so a healthy session is never quiet this long.
const watchInterval = 30 * time.Second
const quietAfter = 90 * time.Second
const warnCooldown = 10 * time.Minute

// How long a cached set of broadcast/probe targets stays fresh. The
// interface enumeration used to run on every 2 s beacon tick; caching
// makes steady-state discovery cost almost no syscalls, and a refresh
// forces a re-enumeration so a network change is picked up within one
// interval instead of at process restart.
const targetsCacheTTL = 60 * time.Second

// A machine seen on the local network.
type Peer struct {
	ID     string `json:"id"`
	Name   string `json:"name"`
	Role   string `json:"role"` // "server" | "client"
	Addr   string `json:"addr"` // IP address (without port)
	Port   int    `json:"port"`
	Source string `json:"source"` // "broadcast" | "probe" | "discovery" (mDNS)
	// Active reports whether the peer's advertised role is actually
	// running there (its GUI is up but no server/client process →
	// inactive). A machine with all services stopped must not appear as
	// a live "nearby" machine.
	Active bool `json:"active"`
}

// shortID is the human-facing form of a machine id: its first 8 chars.
// Trusted-id entries accept this short form (prefix match on both the
// Go side and the Rust server), so users never type 32 hex chars.
func shortID(id string) string {
	if len(id) > 8 {
		return id[:8]
	}
	return id
}

// idTrusted reports whether `id` is in a trusted list (full ids or
// short prefixes, minimum 4 chars so a typo can't trust everything).
func idTrusted(trusted []string, id string) bool {
	for _, t := range trusted {
		t = strings.TrimSpace(t)
		if len(t) < 4 {
			continue
		}
		if id == t || strings.HasPrefix(id, t) {
			return true
		}
	}
	return false
}

// The beacon payload sent on the wire (JSON, one line).
type beaconPayload struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Role string `json:"role"`
	Port int    `json:"port"`
	// Running tells listeners whether the advertised role is actually
	// running here, so a GUI that is open but not sharing anything never
	// shows up as a live "nearby" machine.
	Running bool `json:"running"`
}

// discovery is the single discovery service instance for the GUI lifetime.
type discovery struct {
	core *App

	mu    sync.Mutex
	peers map[string]*Peer  // keyed by machine id
	seen  map[string]time.Time // last beacon heard per id (liveness)

	reg    *zeroconf.Server
	cancel context.CancelFunc

	// beaconConn sends broadcasts; listenConn receives beacons and
	// pairing commands on the same port. Both are owned by the loops
	// that created them: a dead loop closes and nils its socket so the
	// diagnostics watchLoop can see the gap and heal it, instead of the
	// whole channel silently dying for the GUI's lifetime.
	listenConn *net.UDPConn
	beaconConn *net.UDPConn

	started sync.Once
	active  atomic.Bool
	stop    chan struct{}

	// pairQueue carries pairing requests off the listener goroutine.
	// Handling a "connect" inline used to run the whole client-start
	// flow on the socket-read loop; a slow connect stalled every read
	// until the kernel buffer overflowed and discovery went deaf. The
	// worker drops a request when the queue is full (the sender retries
	// with its next beacon-period request) — never blocks the listener.
	pairQueue chan pairJob

	// Bounded diagnostic log: one warning per condition per cooldown,
	// so a broken network costs a handful of log lines per hour instead
	// of one every tick.
	lastBeaconWarn atomic.Int64 // unix nanos of last "no beacons" warning
	lastListenWarn atomic.Int64 // unix nanos of last "cannot listen" warning
	lastQuietWarn  atomic.Int64 // unix nanos of last "hearing nothing" warning
	netCacheMu     sync.Mutex
	netCacheAt     time.Time
	netCacheBcast  []*net.UDPAddr
	netCacheProbe  []*net.UDPAddr
}

func newDiscovery(core *App) *discovery {
	return &discovery{
		core:      core,
		peers:     map[string]*Peer{},
		seen:      map[string]time.Time{},
		stop:      make(chan struct{}),
		pairQueue: make(chan pairJob, pairQueueLen),
	}
}

// start launches beaconing, the receiver, the unicast subnet probe, and
// (best-effort) mDNS. Safe to call once; a later mode change just
// re-advertises.
func (d *discovery) start() {
	d.started.Do(func() {
		d.active.Store(true)
		go d.pairWorker()
		go d.beaconLoop()
		go d.listenLoop() // self-healing wrapper around the receive path
		go d.probeLoop()  // unicast fallback where broadcast/multicast is filtered
		go d.browse()     // mDNS: best-effort second channel
		go d.watchLoop()  // diagnostics: loud when discovery is unhealthy
		d.republish()
	})
}

// ---------------------------------------------------------------------------
// Network targets (cached)
// ---------------------------------------------------------------------------

// computeBroadcastAddrs enumerates the actual destinations: the limited
// broadcast plus each interface's subnet-directed broadcast (some
// routers only forward one of the two forms).
func computeBroadcastAddrs() []*net.UDPAddr {
	out := []*net.UDPAddr{{IP: net.IPv4bcast, Port: discoveryPort}}
	// Subnet-directed broadcast per interface, derived from the address
	// with a /24 mask (the overwhelmingly common home/office case).
	ifs, _ := net.Interfaces()
	for _, ifc := range ifs {
		addrs, err := ifc.Addrs()
		if err != nil {
			continue
		}
		for _, a := range addrs {
			ipn, ok := a.(*net.IPNet)
			if !ok {
				continue
			}
			ip := ipn.IP.To4()
			if ip == nil || ip.IsLoopback() {
				continue
			}
			bcast := net.IPv4(ip[0], ip[1], ip[2], 255)
			out = append(out, &net.UDPAddr{IP: bcast, Port: discoveryPort})
		}
	}
	return out
}

// broadcastAddrs returns the destinations for beacons: the limited
// broadcast plus this machine's subnet broadcast (some routers only
// forward the subnet-directed form). Results are cached for
// targetsCacheTTL — the interface enumeration used to run on every 2 s
// tick for no benefit on a stable network.
func (d *discovery) broadcastAddrs() []*net.UDPAddr {
	d.netCacheMu.Lock()
	defer d.netCacheMu.Unlock()
	if d.netCacheBcast == nil || time.Since(d.netCacheAt) > targetsCacheTTL {
		d.netCacheBcast = computeBroadcastAddrs()
		d.netCacheAt = time.Now()
	}
	return d.netCacheBcast
}

// probeTargets is the cached form of computeProbeTargets (see
// broadcastAddrs for the caching rationale).
func (d *discovery) probeTargets() []*net.UDPAddr {
	d.netCacheMu.Lock()
	defer d.netCacheMu.Unlock()
	if d.netCacheProbe == nil || time.Since(d.netCacheAt) > targetsCacheTTL {
		d.netCacheProbe = computeProbeTargets()
		d.netCacheAt = time.Now()
	}
	return d.netCacheProbe
}

// invalidateTargets drops the cached interface enumeration — called on
// an explicit refresh, and cheap enough to call after any network
// change suspicion (a wrong cache costs one stale interval, not data).
func (d *discovery) invalidateTargets() {
	d.netCacheMu.Lock()
	d.netCacheBcast = nil
	d.netCacheProbe = nil
	d.netCacheAt = time.Time{}
	d.netCacheMu.Unlock()
}

// stopped reports whether shutdown has been requested.
func (d *discovery) stopped() bool {
	select {
	case <-d.stop:
		return true
	default:
		return false
	}
}

// isFatalUDPError classifies receive/send errors that mean the socket
// itself is gone (closed under us, interface removed) and must be
// rebuilt — as opposed to transient errors (routes settling, buffers
// momentarily full) that a healthy socket rides out.
func isFatalUDPError(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, net.ErrClosed) {
		return true
	}
	var oe *net.OpError
	if errors.As(err, &oe) {
		// EBADF/EINVAL on a live socket means it was closed under us.
		var se syscall.Errno
		if errors.As(oe.Err, &se) {
			return se == syscall.EBADF || se == syscall.EINVAL
		}
	}
	return false
}

// beaconLoop broadcasts this machine's presence every beaconInterval.
// The socket is owned by this loop: on fatal send failure it is closed
// and re-created, so a transient network error (interface flap, sleep/
// resume) cannot kill discovery for the GUI's lifetime.
func (d *discovery) beaconLoop() {
	for {
		if d.stopped() {
			return
		}
		conn, err := net.ListenUDP("udp4", nil)
		if err != nil {
			d.warnOnce(&d.lastBeaconWarn, "discovery: no UDP socket for beacons: ", err, 5*time.Minute)
			select {
			case <-d.stop:
				return
			case <-time.After(beaconInterval):
			}
			continue
		}
		if raw, err := conn.SyscallConn(); err == nil {
			_ = raw.Control(func(fd uintptr) {
				_ = setBroadcast(fd)
			})
		}
		d.mu.Lock()
		d.beaconConn = conn
		d.mu.Unlock()

		d.beaconTick(conn)

		ticker := time.NewTicker(beaconInterval)
		for healthy := true; healthy; {
			select {
			case <-d.stop:
				ticker.Stop()
				d.mu.Lock()
				d.beaconConn = nil
				d.mu.Unlock()
				conn.Close()
				return
			case <-ticker.C:
				healthy = d.beaconTick(conn)
			}
		}
		ticker.Stop()
		// Socket went bad: close and rebuild on the next pass.
		d.mu.Lock()
		d.beaconConn = nil
		for id := range d.seen {
			delete(d.seen, id)
		}
		d.mu.Unlock()
		conn.Close()
	}
}

// advertisedRole reports the role this machine should announce: what is
// *actually running here*, not what the GUI's mode dropdown says. The
// dropdown is an intention; the beacon is a fact. Advertising the
// dropdown made a machine running a server but set to "client" (the
// common case right after a role switch) broadcast "client, not
// running" — every other machine then rendered it as an idle ghost.
// Server wins the (unsupported, but defensive) both-running case.
func (d *discovery) advertisedRole() (role string, running bool) {
	switch {
	case d.core.roleActive("server"):
		return "server", true
	case d.core.roleActive("client"):
		return "client", true
	default:
		s := d.core.GetSettings()
		return string(s.Mode), false
	}
}

// beaconPayloadFor builds this machine's current announcement.
func (d *discovery) beaconPayloadFor() beaconPayload {
	role, running := d.advertisedRole()
	return beaconPayload{
		ID:      d.core.GetMachineId(),
		Name:    d.core.displayName(),
		Role:    role,
		Port:    d.core.serverPort(),
		Running: running,
	}
}

// beaconTick sends one beacon round. Returns false when the socket is
// no longer usable and the loop should rebuild it.
func (d *discovery) beaconTick(conn *net.UDPConn) bool {
	payload, _ := json.Marshal(d.beaconPayloadFor())
	dead := false
	for _, dst := range d.broadcastAddrs() {
		if _, err := conn.WriteToUDP(payload, dst); err != nil {
			if isFatalUDPError(err) {
				dead = true
			}
		}
	}
	return !dead
}

// listenLoop receives beacons (→ peer list) and pairing commands on the
// same UDP port. The socket is owned by this loop; on a fatal receive
// error the socket is closed and rebuilt, so a transient interface flap
// degrades one interval instead of silencing discovery forever (the old
// code returned on the first error and the GUI never listened again —
// observed live as a 213 KB unread backlog growing for minutes).
func (d *discovery) listenLoop() {
	for {
		if d.stopped() {
			return
		}
		conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: discoveryPort})
		if err != nil {
			d.warnOnce(&d.lastListenWarn, "discovery: cannot bind :24801 (another kvmshare GUI? a snap of this port?): ", err, 5*time.Minute)
			select {
			case <-d.stop:
				return
			case <-time.After(5 * time.Second):
			}
			continue
		}
		d.mu.Lock()
		d.listenConn = conn
		d.mu.Unlock()

		d.listenServe(conn)

		// Serve returned: socket is dead or we are stopping.
		d.mu.Lock()
		d.listenConn = nil
		d.mu.Unlock()
		conn.Close()
	}
}

// listenServe reads until a fatal receive error or shutdown. Non-fatal
// errors (transient) are skipped without rebuilding the socket.
func (d *discovery) listenServe(conn *net.UDPConn) {
	buf := make([]byte, 2048)
	for {
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil {
			if d.stopped() || isFatalUDPError(err) {
				return
			}
			continue // transient — keep reading
		}
		d.handleDatagram(buf[:n], from)
	}
}

// handleDatagram classifies one datagram by its `cmd` field — pairing
// requests ("connect") and subnet probes ("probe") carry one, beacons
// do not. A pairing request also carries `id`, so the beacon shape
// (id + role + port) alone cannot tell them apart (this very ambiguity
// used to swallow "connect here" requests as beacons). Pairing is
// handed to the worker queue: the listener must never block on the
// connect flow (that stall once deafened discovery for minutes).
func (d *discovery) handleDatagram(data []byte, from *net.UDPAddr) {
	var probe struct {
		Cmd string `json:"cmd"`
	}
	if json.Unmarshal(data, &probe) == nil {
		switch probe.Cmd {
		case "connect":
			select {
			case d.pairQueue <- pairJob{data: append([]byte(nil), data...), from: from, at: time.Now()}:
			default:
				// Queue full: drop. The server's operator retries, and a
				// saturating queue means the worker is wedged — dropping
				// beats stalling the listener either way.
			}
			return
		case "probe":
			d.replyProbe(from)
			return
		}
	}
	var bp beaconPayload
	if json.Unmarshal(data, &bp) == nil && bp.ID != "" {
		d.upsertBeacon(bp, from)
	}
}

// pairJob is one queued pairing request.
type pairJob struct {
	data []byte
	from *net.UDPAddr
	at   time.Time
}

// pairWorker drains the pairing queue off the listener goroutine. Jobs
// older than pairJobMaxAge are discarded: a request that waited out its
// sender's retry cycle could act on a peer that has already gone away.
func (d *discovery) pairWorker() {
	for {
		select {
		case <-d.stop:
			return
		case job := <-d.pairQueue:
			if time.Since(job.at) > pairJobMaxAge {
				continue
			}
			d.handlePairing(job.data, job.from)
		}
	}
}

// ---------------------------------------------------------------------------
// Health watch + bounded diagnostics
// ---------------------------------------------------------------------------

// watchLoop is the discovery layer's own pulse check. Every watchInterval
// it verifies the invariants that make discovery work — sockets alive,
// something heard recently — and, when they break, says so (once per
// cooldown per condition). This exists because discovery used to fail
// silently: a dead listener looked exactly like an empty network, and
// nobody could tell the difference from the outside.
func (d *discovery) watchLoop() {
	ticker := time.NewTicker(watchInterval)
	defer ticker.Stop()
	for {
		select {
		case <-d.stop:
			return
		case <-ticker.C:
			d.checkHealth()
		}
	}
}

// checkHealth runs one watch pass. Deliberately cheap: two mutex-guarded
// reads, no syscalls.
func (d *discovery) checkHealth() {
	d.mu.Lock()
	listening := d.listenConn != nil
	beaconing := d.beaconConn != nil
	var last time.Time
	for _, t := range d.seen {
		if t.After(last) {
			last = t
		}
	}
	nPeers := len(d.peers)
	d.mu.Unlock()
	now := time.Now()

	if !listening {
		d.warnOnce(&d.lastListenWarn, "discovery: receive socket down — rebuilding", nil, warnCooldown)
	}
	if !beaconing {
		d.warnOnce(&d.lastBeaconWarn, "discovery: beacon socket down — rebuilding", nil, warnCooldown)
	}
	// Hearing nothing at all is itself a condition worth one line: with
	// two kvmshare machines on a LAN, beacons arrive every couple of
	// seconds. A long silent stretch with healthy sockets means the
	// network filters the discovery traffic (AP isolation, VLAN) —
	// exactly the case the user needs to know about, because the fix is
	// on the network side (or manual addresses). Only meaningful once
	// this GUI has been up long enough to expect traffic (quietAfter).
	if listening && beaconing && nPeers == 0 && now.Sub(last) > quietAfter {
		d.warnOnce(&d.lastQuietWarn, "discovery: healthy but hearing no beacons — the network may filter broadcast/multicast (AP isolation?); manual addresses still work", nil, warnCooldown)
	}
}

// warnOnce logs a warning at most once per cooldown window (keyed by
// the atomic timestamp), so a persistent condition costs one line per
// cooldown instead of one per tick.
func (d *discovery) warnOnce(key *atomic.Int64, msg string, err error, cooldown time.Duration) {
	now := time.Now().UnixNano()
	last := key.Load()
	if last != 0 && now-last < cooldown.Nanoseconds() {
		return
	}
	if !key.CompareAndSwap(last, now) {
		return // another goroutine won the race
	}
	if err != nil {
		log.Printf("kvmshare-gui: %s%v", msg, err)
		return
	}
	log.Printf("kvmshare-gui: %s", msg)
}

// ---------------------------------------------------------------------------
// Manual refresh
// ---------------------------------------------------------------------------

// refresh clears discovery state and forces an immediate sweep: peers
// re-announced now, subnet re-probed now, interfaces re-enumerated now.
// Backs the UI's refresh button — the user-visible answer to "the list
// looks stale", without waiting out the next probe interval.
func (d *discovery) refresh() {
	if d.stopped() {
		return
	}
	d.invalidateTargets()
	d.mu.Lock()
	d.peers = map[string]*Peer{}
	d.seen = map[string]time.Time{}
	d.mu.Unlock()
	// Announce and ask immediately — the next regular ticks are a
	// full interval away and a refresh should feel instant.
	d.mu.Lock()
	beacon := d.beaconConn
	listener := d.listenConn
	d.mu.Unlock()
	if beacon != nil {
		d.beaconTick(beacon)
	}
	if listener != nil {
		d.probeOnce()
	}
}

// probeLoop sends a "who is kvmshare here?" datagram to every host on
// the local subnets once per probeInterval. Machines reply with a
// regular beacon over unicast, which the listener records — so discovery
// works even on networks (AP client isolation, smart switches) that
// silently drop both broadcast and multicast. Costs a few tiny UDP
// packets per subnet per interval; hosts that never run kvmshare stay
// silent and cost nothing.
func (d *discovery) probeLoop() {
	ticker := time.NewTicker(probeInterval)
	defer ticker.Stop()
	for {
		select {
		case <-d.stop:
			return
		case <-ticker.C:
			d.probeOnce()
		}
	}
}

// probeOnce pings every candidate host from the shared listener socket,
// so replies arrive on the same socket the listener already reads.
// Skips itself when the listener socket is down (replies must arrive on
// it — probing with replies undeliverable is pure waste).
func (d *discovery) probeOnce() {
	d.mu.Lock()
	conn := d.listenConn
	d.mu.Unlock()
	if conn == nil {
		return // listener not up yet — try on the next interval
	}
	payload, _ := json.Marshal(struct {
		Cmd string `json:"cmd"`
		ID  string `json:"id"`
	}{Cmd: "probe", ID: d.core.GetMachineId()})
	for _, dst := range d.probeTargets() {
		_, _ = conn.WriteToUDP(payload, dst)
	}
}

// probeTargets lists every candidate host on this machine's /24 subnets
// (all hosts minus ourselves and the broadcast addresses). Only /24
// subnets are probed — the sweep stays at 254 packets per interval, and
// anything larger is left to the broadcast/mDNS channels (and manual
// addresses). The limited broadcast and each subnet's directed
// broadcast are covered by the beacon channel instead.
func computeProbeTargets() []*net.UDPAddr {
	var out []*net.UDPAddr
	seen := map[string]bool{}
	ifs, _ := net.Interfaces()
	for _, ifc := range ifs {
		if ifc.Flags&net.FlagUp == 0 || ifc.Flags&net.FlagLoopback != 0 {
			continue
		}
		addrs, err := ifc.Addrs()
		if err != nil {
			continue
		}
		for _, a := range addrs {
			ipn, ok := a.(*net.IPNet)
			if !ok {
				continue
			}
			ip := ipn.IP.To4()
			if ip == nil {
				continue
			}
			ones, bits := ipn.Mask.Size()
			if bits != 32 || ones != 24 {
				continue
			}
			for i := 1; i < 255; i++ { // skip network (.0) and broadcast (.255)
				cand := net.IPv4(ip[0], ip[1], ip[2], byte(i))
				if cand.Equal(ip) {
					continue // ourselves
				}
				key := cand.String()
				if seen[key] {
					continue
				}
				seen[key] = true
				out = append(out, &net.UDPAddr{IP: cand, Port: discoveryPort})
			}
		}
	}
	return out
}

// replyProbe answers a "who is kvmshare here?" probe with a normal
// beacon, unicast straight back to the prober.
func (d *discovery) replyProbe(to *net.UDPAddr) {
	payload, _ := json.Marshal(d.beaconPayloadFor())
	conn, err := net.DialUDP("udp4", nil, to)
	if err != nil {
		return
	}
	defer conn.Close()
	_, _ = conn.Write(payload)
}

// upsertBeacon records a peer from a broadcast beacon and stamps its
// liveness.
func (d *discovery) upsertBeacon(bp beaconPayload, from *net.UDPAddr) {
	if bp.ID == d.core.GetMachineId() {
		return // our own echo
	}
	addr := from.IP.String()
	d.mu.Lock()
	d.peers[bp.ID] = &Peer{
		ID:     bp.ID,
		Name:   bp.Name,
		Role:   bp.Role,
		Addr:   addr,
		Port:   bp.Port,
		Source: "broadcast",
		Active: bp.Running,
	}
	d.seen[bp.ID] = time.Now()
	d.mu.Unlock()
}

// expire drops peers that went silent (their beacons, probe replies or
// mDNS announcements stopped). Every channel stamps `seen` on every
// contact, so a live peer keeps refreshing it and a dead one ages out
// after peerTTL — regardless of which channel first discovered it. The
// mDNS library delivers announcements but never tells us when a service
// disappears, so leaving mDNS peers unaged would let a machine that
// closed its GUI linger forever.
func (d *discovery) expire() {
	d.mu.Lock()
	defer d.mu.Unlock()
	cutoff := time.Now().Add(-peerTTL)
	for id := range d.peers {
		if last, ok := d.seen[id]; ok && last.Before(cutoff) {
			delete(d.peers, id)
			delete(d.seen, id)
		}
	}
}

// list returns the current peers for the frontend.
func (d *discovery) list() []Peer {
	d.expire()
	d.mu.Lock()
	defer d.mu.Unlock()
	out := make([]Peer, 0, len(d.peers))
	for _, p := range d.peers {
		out = append(out, *p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// ---------------------------------------------------------------------------
// mDNS channel (secondary, best-effort)
// ---------------------------------------------------------------------------

// republish (re)advertises this machine over mDNS under the *current*
// mode. Called at startup and on role changes.
func (d *discovery) republish() {
	if !d.active.Load() {
		return
	}
	d.mu.Lock()
	defer d.mu.Unlock()
	if d.reg != nil {
		d.reg.Shutdown()
		d.reg = nil
	}
	// Same contract as the UDP beacon: advertise what is actually
	// running (see advertisedRole), never the GUI's mode dropdown.
	adv := d.beaconPayloadFor()
	port := adv.Port
	id := adv.ID

	reg, err := zeroconf.Register(
		"kvmshare-"+id,
		discoveryService,
		"local.",
		port,
		[]string{
			"id=" + id,
			"name=" + adv.Name,
			"role=" + adv.Role,
			"port=" + itoa(port),
			"running=" + strconv.FormatBool(adv.Running),
		},
		nil,
	)
	if err != nil {
		d.reg = nil
		return
	}
	d.reg = reg
}

// browse runs until `stop`, maintaining the peer map from mDNS.
func (d *discovery) browse() {
	ctx, cancel := context.WithCancel(context.Background())
	d.mu.Lock()
	d.cancel = cancel
	d.mu.Unlock()

	entries := make(chan *zeroconf.ServiceEntry, 8)
	resolver, err := zeroconf.NewResolver()
	if err != nil {
		return
	}
	go func() {
		_ = resolver.Browse(ctx, discoveryService, "local.", entries)
	}()

	for {
		select {
		case <-d.stop:
			cancel()
			return
		case e, ok := <-entries:
			if !ok {
				continue
			}
			d.upsertMDNS(e)
		}
	}
}

// upsertMDNS records one mDNS announcement (does not touch broadcast
// liveness — broadcast and mDNS have independent lifetimes).
func (d *discovery) upsertMDNS(e *zeroconf.ServiceEntry) {
	var id, name, role string
	var running bool
	var port = e.Port
	for _, txt := range e.Text {
		kv := strings.SplitN(txt, "=", 2)
		if len(kv) != 2 {
			continue
		}
		switch kv[0] {
		case "id":
			id = kv[1]
		case "name":
			name = kv[1]
		case "role":
			role = kv[1]
		case "running":
			running = kv[1] == "true"
		case "port":
			if p, ok := atoi(kv[1]); ok {
				port = p
			}
		}
	}
	if id == "" || id == d.core.GetMachineId() {
		return
	}
	addr := ""
	if len(e.AddrIPv4) > 0 {
		addr = e.AddrIPv4[0].String()
	} else if len(e.AddrIPv6) > 0 {
		addr = e.AddrIPv6[0].String()
	}
	d.mu.Lock()
	// mDNS wins on fields it knows, but must not resurrect a peer the
	// broadcast/probe channels have declared dead — the peer map is
	// shared.
	if existing, ok := d.peers[id]; ok && existing.Source != "discovery" && time.Since(d.seen[id]) > peerTTL {
		d.mu.Unlock()
		return
	}
	d.peers[id] = &Peer{ID: id, Name: name, Role: role, Addr: addr, Port: port, Source: "discovery", Active: running}
	d.seen[id] = time.Now()
	d.mu.Unlock()
}

// DiscoverPeers exposes the live peer list to the frontend. Always
// returns a non-nil slice so the frontend can safely iterate it.
func (a *App) DiscoverPeers() []Peer {
	if a.disc == nil {
		return []Peer{}
	}
	return a.disc.list()
}

// RefreshDiscovery forces a full discovery sweep: state cleared,
// interfaces re-enumerated, announce + probe sent immediately. Backs
// the UI refresh button. Returns the fresh peer list so the click gives
// instant feedback even before the next pushed state event.
func (a *App) RefreshDiscovery() ([]Peer, error) {
	if a.disc == nil {
		return nil, fmt.Errorf("discovery not started")
	}
	a.disc.refresh()
	return a.disc.list(), nil
}

// ---------------------------------------------------------------------------
// Pairing commands
// ---------------------------------------------------------------------------

// A pairing request from a server.
type pairRequest struct {
	Cmd  string `json:"cmd"`  // "connect"
	ID   string `json:"id"`   // server machine id
	Name string `json:"name"` // server machine name
	Addr string `json:"addr"` // server address host:port
}

func (d *discovery) handlePairing(data []byte, from *net.UDPAddr) {
	var req pairRequest
	if json.Unmarshal(data, &req) != nil || req.Cmd != "connect" || req.ID == "" {
		return
	}
	// A datagram could have been forged; the sender's address is the
	// one we trust for routing (the payload addr is a hint only).
	req.Addr = net.JoinHostPort(from.IP.String(), itoa(reqPort(req.Addr)))

	// Trust on first use: honor the request when the server is trusted
	// OR pairing is enabled. When honored, remember the server's id so
	// the next request is already trusted (and auto-connect sees it).
	if !idTrusted(d.core.GetSettings().TrustedServers, req.ID) && !d.core.settingsPairingEnabled() {
		return
	}
	if !idTrusted(d.core.GetSettings().TrustedServers, req.ID) {
		_ = d.core.TrustServer(req.ID)
	}
	_ = d.core.ConnectToServer(req.Addr)
}

// reqPort extracts the port from an addr hint (host:port), defaulting
// to discoveryPort (the sender of a pairing command is a GUI, which
// listens there — the KVM port is only for actual sessions).
func reqPort(addr string) int {
	if i := strings.LastIndex(addr, ":"); i > 0 {
		if p, err := strconv.Atoi(addr[i+1:]); err == nil && p > 0 {
			return p
		}
	}
	return discoveryPort
}

// SendConnectRequest asks a discovered client (identified by its id) to
// connect to this machine's server. Used from the server's Home page:
// pick a nearby machine, tell it to connect here.
func (a *App) SendConnectRequest(peerID string) error {
	if a.disc == nil {
		return fmt.Errorf("discovery not started")
	}
	var target *Peer
	for _, p := range a.disc.list() {
		if p.ID == peerID || shortID(p.ID) == peerID {
			target = &p
			break
		}
	}
	if target == nil {
		return fmt.Errorf("no discovered machine with id %q", peerID)
	}
	addr := net.JoinHostPort(target.Addr, itoa(discoveryPort))
	conn, err := net.Dial("udp", addr)
	if err != nil {
		return err
	}
	defer conn.Close()
	req := pairRequest{
		Cmd:  "connect",
		ID:   a.GetMachineId(),
		Name: a.displayName(),
		Addr: net.JoinHostPort(a.primaryLANAddr(), itoa(a.serverPort())),
	}
	data, _ := json.Marshal(req)
	_, err = conn.Write(data)
	return err
}

// settingsPairingEnabled reports the pairing toggle (client accepts
// connection requests from any local server on first use).
func (a *App) settingsPairingEnabled() bool {
	return a.GetSettings().AcceptPairing
}

// primaryLANAddr returns this machine's first private IPv4 address (used
// in pairing requests so the client knows where to reach the server).
func (a *App) primaryLANAddr() string {
	ifaces, _ := a.ListInterfaces()
	for _, i := range ifaces {
		for _, addr := range i.Addrs {
			ip := net.ParseIP(addr)
			if ip == nil {
				continue
			}
			v4 := ip.To4()
			if v4 != nil && v4.IsPrivate() {
				return v4.String()
			}
		}
	}
	return "127.0.0.1"
}

// serverPort returns the port the server listens on (from the config),
// falling back to the default.
func (a *App) serverPort() int {
	cfg, err := a.LoadConfig()
	if err != nil || cfg.Port == 0 {
		return defaultPort
	}
	return cfg.Port
}

// itoa / atoi: tiny helpers for the TXT records and ports.
func itoa(n int) string {
	return strconv.Itoa(n)
}

func atoi(s string) (int, bool) {
	n, err := strconv.Atoi(s)
	return n, err == nil
}