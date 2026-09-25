package discovery

import (
	"encoding/json"
	"net"
	"testing"
	"time"

	"kvmshare/gui/internal/discovery/zeroconf"
)

// fakeHost records what the engine asks and hands back pairing
// requests for inspection.
type fakeHost struct {
	id    string
	name  string
	port  int
	role  string
	run   bool
	lan   string
	pairs []PairRequest
}

func (h *fakeHost) MachineID() string              { return h.id }
func (h *fakeHost) MachineName() string            { return h.name }
func (h *fakeHost) ServerPort() int                { return h.port }
func (h *fakeHost) AdvertisedRole() (string, bool) { return h.role, h.run }
func (h *fakeHost) LANAddr() string                { return h.lan }
func (h *fakeHost) OnPairRequest(req PairRequest)  { h.pairs = append(h.pairs, req) }

func newTestService(t *testing.T) (*Service, *fakeHost) {
	t.Helper()
	h := &fakeHost{id: "aaaaaaaa11111111", name: "pc", port: 24800, role: "server", run: true, lan: "192.168.1.86"}
	s := New(h)
	t.Cleanup(s.Close)
	return s, h
}

// A beacon (id + role + port) lands in the peer map; a pairing request
// (cmd + id) must NOT — the two shapes share the `id` field, and
// misclassifying a pairing request as a beacon used to swallow
// "connect here" silently.
func TestDatagramClassification(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}

	beacon, err := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: true})
	if err != nil {
		t.Fatal(err)
	}
	s.handleDatagram(beacon, from)
	peers := s.List()
	if len(peers) != 1 || peers[0].ID != "bbbbbbbb11111111" {
		t.Fatalf("beacon not upserted as peer: %+v", peers)
	}
	if !peers[0].Active {
		t.Fatalf("beacon with running=true must record the peer as active: %+v", peers[0])
	}
	if peers[0].Source != SourceBroadcast {
		t.Fatalf("broadcast beacon must carry the broadcast source: %+v", peers[0])
	}

	// A beacon that advertises "GUI up but nothing running" records the
	// peer as inactive — a machine that stopped every service must not
	// linger as a live "nearby" machine.
	idle, err := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: false})
	if err != nil {
		t.Fatal(err)
	}
	s.handleDatagram(idle, from)
	if got := s.List(); len(got) != 1 || got[0].Active {
		t.Fatalf("idle beacon must mark the peer inactive: %+v", got)
	}

	req, err := json.Marshal(pairRequest{Cmd: "connect", ID: "cccccccc22222222", Name: "desk", Addr: "192.168.1.86:24800"})
	if err != nil {
		t.Fatal(err)
	}
	s.handleDatagram(req, from)
	if got := s.List(); len(got) != 1 {
		t.Fatalf("pairing request leaked into the peer map: %+v", got)
	}
	if len(s.host.(*fakeHost).pairs) != 0 {
		t.Fatal("pairing request must be queued for the worker, not delivered inline")
	}
	if pending := len(s.pairQueue); pending != 1 {
		t.Fatalf("pairing request not queued (queue len %d)", pending)
	}
}

// A subnet probe ("cmd":"probe") must NOT be recorded as a peer — it
// exists to trigger a beacon reply, and recording it would fabricate a
// machine at the prober's address.
func TestProbeNotRecorded(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}

	probe, err := json.Marshal(probeMsg{Cmd: "probe", ID: "aaaaaaaa11111111"})
	if err != nil {
		t.Fatal(err)
	}
	s.handleDatagram(probe, from)
	if got := s.List(); len(got) != 0 {
		t.Fatalf("probe must not be recorded as a peer: %+v", got)
	}
}

// The engine skips its own announcements on every channel: a beacon
// carrying our own id (a broadcast echo) must never fabricate a peer.
func TestOwnBeaconIgnored(t *testing.T) {
	s, h := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 86), Port: Port}

	beacon, _ := json.Marshal(beaconPayload{ID: h.id, Name: h.name, Role: "server", Port: 24800, Running: true})
	s.handleDatagram(beacon, from)
	if got := s.List(); len(got) != 0 {
		t.Fatalf("own echo recorded as a peer: %+v", got)
	}
}

// PeerByID matches full ids and the 8-char short form — the UI only
// ever holds the short form.
func TestPeerByIDShortForm(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}
	beacon, _ := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: true})
	s.handleDatagram(beacon, from)

	if _, ok := s.PeerByID("bbbbbbbb11111111"); !ok {
		t.Fatal("full id must match")
	}
	if _, ok := s.PeerByID("bbbbbbbb"); !ok {
		t.Fatal("short id must match")
	}
	if _, ok := s.PeerByID("bbbb"); ok {
		t.Fatal("partial id shorter than the short form must not match")
	}
}

// Peers age out after peerTTL of silence on every channel — the mDNS
// library never announces departures, so liveness expiry is the only
// thing that removes a machine that closed its GUI.
func TestPeersExpireAfterTTL(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}
	beacon, _ := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: true})
	s.handleDatagram(beacon, from)

	// Simulate the last contact aging past the TTL.
	s.mu.Lock()
	s.seen["bbbbbbbb11111111"] = time.Now().Add(-peerTTL - time.Second)
	s.mu.Unlock()

	if got := s.List(); len(got) != 0 {
		t.Fatalf("silent peer must expire: %+v", got)
	}
}

// Expiry must run on every read, not only inside Refresh: between
// sweeps the map is what every UI renders, and a row frozen at its
// last datagram showed a machine as connected long after it left
// (the observed "discovery shows a state that is not there").
func TestListExpiresStalePeersWithoutRefresh(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}
	beacon, _ := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: true})
	s.handleDatagram(beacon, from)
	if got := s.List(); len(got) != 1 {
		t.Fatalf("fresh beacon must list: %+v", got)
	}

	// The peer's machine went away: backdate its last contact. No
	// Refresh is called — the state loop's List (via DiscoverPeers,
	// once a second) is the only reader.
	s.mu.Lock()
	s.seen["bbbbbbbb11111111"] = time.Now().Add(-peerTTL - time.Second)
	s.mu.Unlock()

	if got := s.List(); len(got) != 0 {
		t.Fatalf("stale peer must vanish from a plain List, got: %+v", got)
	}
}

// An mDNS-only peer legitimately goes minutes between announcements
// (the responder announces on its own schedule, not per session tick),
// so it must outlive the broadcast TTL — expiring it at peerTTL froze
// live machines out of the list. It still expires at the wider mDNS
// window, so a machine that closed its GUI cannot linger forever.
func TestMDNSPeerUsesItsOwnTTL(t *testing.T) {
	s, _ := newTestService(t)

	entry := zeroconf.NewServiceEntry("kvmshare-bbbbbbbb11111111", serviceType, "local.")
	entry.Port = 24800
	entry.AddrIPv4 = []net.IP{net.IPv4(192, 168, 1, 72)}
	entry.Text = []string{"id=bbbbbbbb11111111", "name=laptop", "role=client", "port=24800", "running=true"}
	entry.TTL = 3600
	s.upsertMDNS(entry)
	if got := s.List(); len(got) != 1 || got[0].Source != SourceMDNS {
		t.Fatalf("mDNS announcement must list as an mDNS peer: %+v", got)
	}

	// Past the broadcast TTL but well inside the mDNS window: must stay.
	s.mu.Lock()
	s.seen["bbbbbbbb11111111"] = time.Now().Add(-peerTTL - time.Second)
	s.mu.Unlock()
	if got := s.List(); len(got) != 1 {
		t.Fatalf("mDNS peer must outlive the broadcast TTL: %+v", got)
	}

	// Past the mDNS window: gone.
	s.mu.Lock()
	s.seen["bbbbbbbb11111111"] = time.Now().Add(-mdnsPeerTTL - time.Second)
	s.mu.Unlock()
	if got := s.List(); len(got) != 0 {
		t.Fatalf("mDNS peer must expire at the mDNS window: %+v", got)
	}
}

// The seeking-session renewal check must read freshness, not map size:
// a stale entry must let the session end (grounding the engine), a
// fresh one must renew it. The pre-fix code took len(seen) > 0, so one
// aged entry renewed a dead session forever — the peer list froze with
// the stale machine the user was looking at.
func TestStalePeerCannotRenewSession(t *testing.T) {
	s, _ := newTestService(t)

	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}
	beacon, _ := json.Marshal(beaconPayload{ID: "bbbbbbbb11111111", Name: "laptop", Role: "client", Port: 24800, Running: true})
	s.handleDatagram(beacon, from)

	// A seeking session whose deadline has passed renews while the
	// peer is fresh — even though nothing new has arrived.
	s.sess.mu.Lock()
	s.sess.state = SessionSeeking
	s.sess.deadline = time.Now().Add(-time.Second)
	s.sess.mu.Unlock()
	go s.sessionLoop()
	time.Sleep(2500 * time.Millisecond) // > sessionCheckInterval
	if st := s.Status(); st.State != SessionSeeking.String() {
		t.Fatalf("fresh peer must renew the seeking session, state = %q", st.State)
	}

	// The peer goes stale: the next deadline pass must ground the
	// engine instead of renewing on the stale entry. (sess.mu and the
	// peer-map mutex are taken one at a time — the sessionLoop holds
	// sess.mu across its own s.mu read, so nesting them in the reverse
	// order here could deadlock.)
	s.mu.Lock()
	s.seen["bbbbbbbb11111111"] = time.Now().Add(-peerTTL - time.Second)
	s.mu.Unlock()
	s.sess.mu.Lock()
	s.sess.deadline = time.Now().Add(-time.Second)
	s.sess.mu.Unlock()
	deadline := time.Now().Add(5 * time.Second)
	for time.Now().Before(deadline) {
		if st := s.Status(); st.State == SessionIdle.String() {
			return
		}
		time.Sleep(100 * time.Millisecond)
	}
	t.Fatalf("stale peer must not renew the session, state = %q", s.Status().State)
}

// handlePairing keeps the hint's port (the only source of the
// sender's KVM port — the datagram's source port is the GUI's discovery
// socket, not the server) but re-derives the IP from the routed
// datagram, because the payload IP could be forged.
func TestPairingAddressNormalized(t *testing.T) {
	s, _ := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}

	req, _ := json.Marshal(pairRequest{Cmd: "connect", ID: "cccccccc22222222", Name: "desk", Addr: "10.0.0.1:24800"})
	s.handlePairing(req, from)

	h := s.host.(*fakeHost)
	if len(h.pairs) != 1 {
		t.Fatalf("pair request not delivered to host: %+v", h.pairs)
	}
	if got := h.pairs[0].Addr; got != "192.168.1.72:24800" {
		t.Fatalf("addr = %q, want routed IP with the hint port (192.168.1.72:24800)", got)
	}

	// A hint without a port falls back to the discovery port.
	s.handlePairing([]byte(`{"cmd":"connect","id":"cccccccc22222222","name":"desk","addr":"10.0.0.1"}`), from)
	if got := h.pairs[1].Addr; got != "192.168.1.72:24801" {
		t.Fatalf("addr = %q, want the discovery-port default (192.168.1.72:24801)", got)
	}
}

// A malformed or foreign pairing payload is dropped, not delivered.
func TestPairingGarbageDropped(t *testing.T) {
	s, h := newTestService(t)
	from := &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}

	s.handlePairing([]byte("not json"), from)
	s.handlePairing([]byte(`{"cmd":"connect"}`), from) // no id
	if len(h.pairs) != 0 {
		t.Fatalf("garbage delivered to host: %+v", h.pairs)
	}
}
