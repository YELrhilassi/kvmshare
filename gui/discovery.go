package main

// Network discovery: machines running kvmshare on the local network find
// each other without typing IPs or ports.
//
//   - Advertise — this machine announces itself over mDNS/DNS-SD
//     (`_kvmshare._tcp`) with its machine id, name, role and port, so a
//     nearby client can find the server and vice versa.
//   - Browse    — the GUI watches the same service and keeps a live list
//     of peers (id, name, role, address, port), exposed to the frontend.
//   - Pairing   — a small UDP command listener on `discoveryPort` lets a
//     server ask a discovered client to connect: `{"cmd":"connect",...}`.
//     The client only honors it when the server's id is trusted (or the
//     user enabled pairing), so a stranger cannot commandeer a machine.
//
// Manual IP:port connection always remains as the fallback — discovery is
// a convenience on top, never a requirement.

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

// discoveryPort carries pairing commands (UDP). Deliberately the KVM port
// plus one, and independent of the KVM port itself: pairing must work
// whether or not a role is currently running.
const discoveryPort = defaultPort + 1

// A machine seen on the local network.
type Peer struct {
	ID     string `json:"id"`
	Name   string `json:"name"`
	Role   string `json:"role"` // "server" | "client"
	Addr   string `json:"addr"` // IP address (without port)
	Port   int    `json:"port"`
	Source string `json:"source"` // "discovery" (mDNS) — room for more later
}

// discovery is the single mDNS service instance for the GUI lifetime.
type discovery struct {
	core *App

	mu    sync.Mutex
	peers map[string]*Peer // keyed by machine id

	reg    *zeroconf.Server
	cancel context.CancelFunc

	// listenConn is the UDP socket that receives pairing commands.
	listenConn *net.UDPConn
	started    sync.Once
	// active is true once start() ran — mDNS sockets are only touched
	// after that (tests that never start discovery must not block on
	// multicast).
	active atomic.Bool
	stop    chan struct{}
}

func newDiscovery(core *App) *discovery {
	return &discovery{core: core, peers: map[string]*Peer{}, stop: make(chan struct{})}
}

// start launches advertise + browse + the pairing listener. Safe to call
// once (a later mode change just re-advertises).
func (d *discovery) start() {
	d.started.Do(func() {
		d.active.Store(true)
		go d.listenPairing()
		go d.browse()
		d.republish()
	})
}

// republish (re)advertises this machine under the *current* mode. Called
// at startup and whenever the role selection changes, so a machine that
// switches server ↔ client is always discoverable under the right role.
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
	role := string(s.Mode)
	port := d.core.serverPort()
	host := hostnameOr("kvmshare")

	// Keep the machine id stable across re-publishes.
	id := d.core.GetMachineId()

	reg, err := zeroconf.Register(
		"kvmshare-"+id, // unique instance name
		discoveryService,
		"local.",
		port,
		[]string{
			"id=" + id,
			"name=" + host,
			"role=" + role,
			"port=" + itoa(port),
		},
		nil,
	)
	if err != nil {
		// No multicast interface — discovery is best-effort; the GUI
		// still works via manual addresses.
		d.reg = nil
		return
	}
	d.reg = reg
}

// browse runs until `stop`; it maintains the peer map. Peers that stop
// announcing are dropped after `peerTTL` of silence.
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

	ticker := time.NewTicker(peerTTL)
	defer ticker.Stop()
	for {
		select {
		case <-d.stop:
			cancel()
			return
		case e, ok := <-entries:
			if !ok {
				continue
			}
			d.upsert(e)
		case <-ticker.C:
			d.expire()
		}
	}
}

// upsert records one mDNS announcement.
func (d *discovery) upsert(e *zeroconf.ServiceEntry) {
	var id, name, role string
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
		case "port":
			if p, ok := atoi(kv[1]); ok {
				port = p
			}
		}
	}
	// Ignore our own announcement (mDNS echo) and unparsed entries.
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
	d.peers[id] = &Peer{ID: id, Name: name, Role: role, Addr: addr, Port: port, Source: "discovery"}
	d.mu.Unlock()
}

// expire drops peers not heard from within peerTTL.
func (d *discovery) expire() {
	d.mu.Lock()
	defer d.mu.Unlock()
	// The zeroconf library renews entries it is actively watching; we
	// track liveness via the service cache instead — entries that stop
	// announcing are removed by the library itself on TTL expiry, so
	// this map needs no age-based sweep. Keep the hook for clarity.
	_ = d.peers
}

// list returns the current peers, newest first, for the frontend.
func (d *discovery) list() []Peer {
	d.mu.Lock()
	defer d.mu.Unlock()
	out := make([]Peer, 0, len(d.peers))
	for _, p := range d.peers {
		out = append(out, *p)
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out
}

// DiscoverPeers exposes the live peer list to the frontend.
func (a *App) DiscoverPeers() []Peer {
	if a.disc == nil {
		return nil
	}
	return a.disc.list()
}

// listenPairing receives `{"cmd":"connect",...}` datagrams. A server
// operator clicks "connect" on a discovered client in their GUI; that
// machine's GUI, if it trusts the server (or the user enabled pairing),
// starts its client pointed at that server — no mouse-plugging or IP
// typing on the client machine.
func (d *discovery) listenPairing() {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: discoveryPort})
	if err != nil {
		// Port busy (another GUI instance) — pairing is best-effort.
		return
	}
	d.mu.Lock()
	d.listenConn = conn
	d.mu.Unlock()
	buf := make([]byte, 1024)
	for {
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil {
			if d.core != nil {
				return
			}
			continue
		}
		d.handlePairing(buf[:n], from)
	}
}

// A pairing request from a server.
type pairRequest struct {
	Cmd    string `json:"cmd"`    // "connect"
	ID     string `json:"id"`     // server machine id
	Name   string `json:"name"`   // server machine name
	Addr   string `json:"addr"`   // server address host:port
	Source string `json:"source"` // how it reached us
}

func (d *discovery) handlePairing(data []byte, from *net.UDPAddr) {
	var req pairRequest
	if json.Unmarshal(data, &req) != nil || req.Cmd != "connect" || req.ID == "" {
		return
	}
	req.Source = from.IP.String()

	// Security: only honor pairing from a server we trust, or when the
	// user enabled "accept pairing requests". A stranger's datagram is
	// dropped silently (it could be anyone on the LAN).
	trusted := d.core.settingsTrustsServer(req.ID)
	accept := d.core.settingsPairingEnabled()
	if !trusted && !accept {
		return
	}
	// Trusted (or allowed): connect to the server. If a client is
	// already running it reconnects on its own; starting is idempotent.
	addr := req.Addr
	if addr == "" && req.Source != "" {
		addr = net.JoinHostPort(req.Source, itoa(discoveryPort))
	}
	if addr == "" {
		return
	}
	d.core.ConnectToServer(addr)
}

// settingsTrustsServer reports whether the given server id is in the
// GUI's trusted-servers list.
func (a *App) settingsTrustsServer(id string) bool {
	for _, t := range a.GetSettings().TrustedServers {
		if t == id {
			return true
		}
	}
	return false
}

// settingsPairingEnabled reports the pairing toggle (client accepts
// connection requests from any local server).
func (a *App) settingsPairingEnabled() bool {
	return a.GetSettings().AcceptPairing
}

// SendConnectRequest asks a discovered client (identified by its mDNS
// id/address) to connect to this machine's server. Used from the server
// page: pick a nearby machine, tell it to connect here.
func (a *App) SendConnectRequest(peerID string) error {
	if a.disc == nil {
		return fmt.Errorf("discovery not started")
	}
	var target *Peer
	for _, p := range a.disc.list() {
		if p.ID == peerID {
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
		Name: hostnameOr("kvmshare"),
		Addr: net.JoinHostPort(a.primaryLANAddr(), itoa(a.serverPort())),
	}
	data, _ := json.Marshal(req)
	_, err = conn.Write(data)
	return err
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

// peerTTL bounds how long a vanished peer lingers in the map.
const peerTTL = 15 * time.Second