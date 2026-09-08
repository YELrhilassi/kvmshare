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
	"fmt"
	"net"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
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
// peerTTL comfortably exceeds the probe interval: on networks where
// broadcast/multicast is filtered, the unicast probe reply is the only
// signal keeping the peer alive.
const beaconInterval = 2 * time.Second
const probeInterval = 10 * time.Second
const peerTTL = 15 * time.Second

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
	// pairing commands on the same port.
	listenConn *net.UDPConn
	started    sync.Once
	active     atomic.Bool
	stop       chan struct{}
}

func newDiscovery(core *App) *discovery {
	return &discovery{
		core:  core,
		peers: map[string]*Peer{},
		seen:  map[string]time.Time{},
		stop:  make(chan struct{}),
	}
}

// start launches beaconing, the receiver, the unicast subnet probe, and
// (best-effort) mDNS. Safe to call once; a later mode change just
// re-advertises.
func (d *discovery) start() {
	d.started.Do(func() {
		d.active.Store(true)
		go d.beaconLoop()
		go d.listen()
		go d.probeLoop() // unicast fallback where broadcast/multicast is filtered
		go d.browse()    // mDNS: best-effort second channel
		d.republish()
	})
}

// ---------------------------------------------------------------------------
// Broadcast channel
// ---------------------------------------------------------------------------

// broadcastAddr returns the destination(s) for beacons: the limited
// broadcast plus this machine's subnet broadcast (some routers only
// forward the subnet-directed form).
func broadcastAddrs() []*net.UDPAddr {
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

// beaconLoop broadcasts this machine's presence every beaconInterval.
func (d *discovery) beaconLoop() {
	// An UNCONNECTED socket: each beacon goes to every broadcast
	// destination (limited + subnet-directed). A dialed (connected)
	// socket can only ever reach its single dialed address, and
	// WriteToUDP on it fails with "use of WriteTo with pre-connected
	// connection" — which used to silently kill every beacon, leaving
	// discovery empty on networks where mDNS multicast is filtered.
	conn, err := net.ListenUDP("udp4", nil)
	if err != nil {
		return // no network — discovery is best-effort
	}
	defer conn.Close()
	// Sending to broadcast addresses needs SO_BROADCAST.
	if raw, err := conn.SyscallConn(); err == nil {
		_ = raw.Control(func(fd uintptr) {
			_ = setBroadcast(fd)
		})
	}

	ticker := time.NewTicker(beaconInterval)
	defer ticker.Stop()
	for {
		select {
		case <-d.stop:
			return
		case <-ticker.C:
			s := d.core.GetSettings()
			payload, _ := json.Marshal(beaconPayload{
				ID:      d.core.GetMachineId(),
				Name:    d.core.displayName(),
				Role:    string(s.Mode),
				Port:    d.core.serverPort(),
				Running: d.core.roleActive(string(s.Mode)),
			})
			for _, dst := range broadcastAddrs() {
				_, _ = conn.WriteToUDP(payload, dst)
			}
		}
	}
}

// listen receives beacons (→ peer list) and pairing commands on the
// same UDP port.
func (d *discovery) listen() {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: discoveryPort})
	if err != nil {
		return // port busy — discovery is best-effort
	}
	d.mu.Lock()
	d.listenConn = conn
	d.mu.Unlock()

	buf := make([]byte, 2048)
	for {
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil {
			return
		}
		d.handleDatagram(buf[:n], from)
	}
}

// handleDatagram classifies one datagram by its `cmd` field — pairing
// requests ("connect") and subnet probes ("probe") carry one, beacons
// do not. A pairing request also carries `id`, so the beacon shape
// (id + role + port) alone cannot tell them apart (this very ambiguity
// used to swallow "connect here" requests as beacons).
func (d *discovery) handleDatagram(data []byte, from *net.UDPAddr) {
	var probe struct {
		Cmd string `json:"cmd"`
	}
	if json.Unmarshal(data, &probe) == nil {
		switch probe.Cmd {
		case "connect":
			d.handlePairing(data, from)
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

// ---------------------------------------------------------------------------
// Unicast probe channel (fallback for filtered broadcast/multicast)
// ---------------------------------------------------------------------------

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
	for _, dst := range probeTargets() {
		_, _ = conn.WriteToUDP(payload, dst)
	}
}

// probeTargets lists every candidate host on this machine's /24 subnets
// (all hosts minus ourselves and the broadcast addresses). Only /24
// subnets are probed — the sweep stays at 254 packets per interval, and
// anything larger is left to the broadcast/mDNS channels (and manual
// addresses). The limited broadcast and each subnet's directed
// broadcast are covered by the beacon channel instead.
func probeTargets() []*net.UDPAddr {
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
	s := d.core.GetSettings()
	payload, _ := json.Marshal(beaconPayload{
		ID:      d.core.GetMachineId(),
		Name:    d.core.displayName(),
		Role:    string(s.Mode),
		Port:    d.core.serverPort(),
		Running: d.core.roleActive(string(s.Mode)),
	})
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

// expire drops peers that went silent (their beacons or probe replies
// stopped). mDNS peers are handled by the library's own TTL and are
// left alone; broadcast and probe peers are refreshed by our own
// packets and aged here.
func (d *discovery) expire() {
	d.mu.Lock()
	defer d.mu.Unlock()
	cutoff := time.Now().Add(-peerTTL)
	for id, p := range d.peers {
		if p.Source == "discovery" {
			continue
		}
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
	s := d.core.GetSettings()
	port := d.core.serverPort()
	id := d.core.GetMachineId()

	reg, err := zeroconf.Register(
		"kvmshare-"+id,
		discoveryService,
		"local.",
		port,
		[]string{
			"id=" + id,
			"name=" + d.core.displayName(),
			"role=" + string(s.Mode),
			"port=" + itoa(port),
			"running=" + strconv.FormatBool(d.core.roleActive(string(s.Mode))),
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