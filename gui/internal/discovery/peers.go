package discovery

import (
	"encoding/json"
	"fmt"
	"net"
	"strings"
	"time"
)

// upsertBeacon records a peer from a broadcast beacon and stamps its
// liveness.
func (s *Service) upsertBeacon(bp beaconPayload, from *net.UDPAddr) {
	if bp.ID == s.host.MachineID() {
		return // our own echo
	}
	addr := from.IP.String()
	s.mu.Lock()
	s.peers[bp.ID] = &Peer{
		ID:     bp.ID,
		Name:   bp.Name,
		Role:   bp.Role,
		Addr:   addr,
		Port:   bp.Port,
		Source: SourceBroadcast,
		Active: bp.Running,
	}
	s.seen[bp.ID] = time.Now()
	s.mu.Unlock()
}

// expiryCutoff is the per-peer freshness deadline: the longest silence
// this peer's own channel can produce while healthy. Broadcast and
// probe peers re-stamp `seen` on every contact (beacons every 2 s), so
// peerTTL is generous for them. An mDNS-only peer is stamped only when
// its responder announces — minutes apart while healthy — so its
// silence window is the wider one (see mdnsPeerTTL). Everything else
// in the file uses wall-clock reads; this helper keeps the choice in
// one place.
func expiryCutoff(now time.Time, source string) time.Time {
	ttl := peerTTL
	if source == SourceMDNS {
		ttl = mdnsPeerTTL
	}
	return now.Add(-ttl)
}

// expire drops peers that went silent. Every channel stamps `seen` on
// every contact, so a live peer keeps refreshing it and a dead one ages
// out — regardless of which channel first discovered it. The mDNS
// library delivers announcements but never tells us when a service
// disappears, so mDNS peers age out too (their wider window; see
// [`mdnsPeerTTL`]).
//
// It runs on every read as well as on the sweep: expiry is what keeps
// the list *true*, and List is exactly where a stale row is about to
// be shown or acted on. The old design only expired inside Refresh —
// between sweeps the map (and every UI built from it) showed machines
// as they were when their last datagram landed, not as they are. That
// is the observed "discovery shows a machine as connected that is
// not": the row sits frozen while the map's only reaper waits for a
// button press.
func (s *Service) expire() {
	s.mu.Lock()
	defer s.mu.Unlock()
	cutoff := time.Now()
	for id := range s.peers {
		if last, ok := s.seen[id]; ok && last.Before(expiryCutoff(cutoff, s.peers[id].Source)) {
			delete(s.peers, id)
			delete(s.seen, id)
		}
	}
}

// pairAddrPort extracts the port from a pairing request's address hint
// (host:port), defaulting to Port (the sender of a pairing command is a
// GUI, which listens there — the KVM port is only for actual sessions).
func pairAddrPort(addr string) int {
	if i := strings.LastIndex(addr, ":"); i > 0 {
		if p, ok := atoi(addr[i+1:]); ok && p > 0 {
			return p
		}
	}
	return Port
}

// handlePairing normalizes one queued pairing request and hands it to
// the host. A datagram could have been forged; the sender's address is
// the one we trust for routing (the payload addr is a hint only).
func (s *Service) handlePairing(data []byte, from *net.UDPAddr) {
	var req pairRequest
	if json.Unmarshal(data, &req) != nil || req.Cmd != "connect" || req.ID == "" {
		return
	}
	s.host.OnPairRequest(PairRequest{
		ID:   req.ID,
		Name: req.Name,
		Addr: net.JoinHostPort(from.IP.String(), itoa(pairAddrPort(req.Addr))),
	})
}

// SendConnectRequest asks a discovered client (identified by its full
// or short id) to connect to this machine's server. Used from the
// server's Home page: pick a nearby machine, tell it to connect here.
func (s *Service) SendConnectRequest(peerID string) error {
	target, ok := s.PeerByID(peerID)
	if !ok {
		return fmt.Errorf("no discovered machine with id %q", peerID)
	}
	addr := net.JoinHostPort(target.Addr, itoa(Port))
	conn, err := net.Dial("udp", addr)
	if err != nil {
		return err
	}
	defer conn.Close()
	req := pairRequest{
		Cmd:  "connect",
		ID:   s.host.MachineID(),
		Name: s.host.MachineName(),
		// The address hint: where our own server listens. The receiver
		// re-derives the host from the datagram, but the port is ours to
		// state.
		Addr: net.JoinHostPort(s.host.LANAddr(), itoa(s.host.ServerPort())),
	}
	data, _ := json.Marshal(req)
	_, err = conn.Write(data)
	return err
}
