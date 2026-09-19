package discovery

import (
	"encoding/json"
	"fmt"
	"net"
	"time"
)

// probe.go — the direct path: ask one address "are you a kvmshare
// machine?" and wait for the answer. This is manual discovery in its
// purest form: no broadcast, no multicast, no cached state — it works
// whenever IP reachability works, which is what makes it the fallback
// that "can never fail" as long as the network itself is up.

// probeTimeout bounds one direct probe. Two full seconds: a Wi-Fi
// client in power-save can take most of a second to wake and answer,
// and the probe is a user action where a beat of patience beats a
// false "no one there".
const probeTimeout = 2 * time.Second

// Probe asks `host:port` (a GUI discovery socket) directly, using the
// listener's socket when one exists so the reply lands where the
// listener is already reading — the peer then enters the map through
// the normal beacon path — and falling back to a dedicated socket when
// it does not. Returns the announced peer either way; the peer map is
// updated only by the listener (one writer, no torn state).
func (s *Service) Probe(addr string) (Peer, error) {
	raddr, err := net.ResolveUDPAddr("udp4", addr)
	if err != nil {
		return Peer{}, fmt.Errorf("bad address %q: %w", addr, err)
	}
	payload, _ := json.Marshal(probeMsg{Cmd: "probe", ID: s.host.MachineID()})

	s.mu.Lock()
	conn := s.listenConn
	s.mu.Unlock()
	if conn == nil {
		// Listener down: use a dedicated socket and read the reply
		// ourselves. Still a real answer — just not one that lands in
		// the map (the session's listener will learn it soon after).
		return s.probeDedicated(raddr, payload)
	}
	if _, err := conn.WriteToUDP(payload, raddr); err != nil {
		return Peer{}, fmt.Errorf("probe to %s: %w", raddr, err)
	}
	// The reply is a normal beacon; give the listener a moment to
	// process it, then read the map. Waiting here is the point: the
	// caller wants the answer, not an ACK that a question was asked.
	deadline := time.Now().Add(probeTimeout)
	for {
		if p, ok := s.peerNear(raddr, time.Until(deadline)); ok {
			return p, nil
		}
		if time.Now().After(deadline) {
			return Peer{}, fmt.Errorf("%s did not answer — is kvmshare running there?", raddr)
		}
		time.Sleep(100 * time.Millisecond)
	}
}

// probeDedicated runs the probe on its own socket when the shared
// listener is down. Same contract, private reply path.
func (s *Service) probeDedicated(raddr *net.UDPAddr, payload []byte) (Peer, error) {
	conn, err := net.DialUDP("udp4", nil, raddr)
	if err != nil {
		return Peer{}, fmt.Errorf("probe to %s: %w", raddr, err)
	}
	defer conn.Close()
	if _, err := conn.Write(payload); err != nil {
		return Peer{}, fmt.Errorf("probe to %s: %w", raddr, err)
	}
	conn.SetReadDeadline(time.Now().Add(probeTimeout))
	buf := make([]byte, 2048)
	for {
		n, err := conn.Read(buf)
		if err != nil {
			return Peer{}, fmt.Errorf("%s did not answer — is kvmshare running there?", raddr)
		}
		var bp beaconPayload
		if json.Unmarshal(buf[:n], &bp) == nil && bp.ID != "" && bp.ID != s.host.MachineID() {
			return Peer{
				ID:     bp.ID,
				Name:   bp.Name,
				Role:   bp.Role,
				Addr:   raddr.IP.String(),
				Port:   bp.Port,
				Source: SourceProbe,
				Active: bp.Running,
			}, nil
		}
		// Not a beacon (echo of our own probe, foreign datagram): keep
		// reading until the deadline.
	}
}

// peerNear finds a peer at `raddr`'s IP, waiting up to `wait` for the
// listener to record the reply beacon.
func (s *Service) peerNear(raddr *net.UDPAddr, wait time.Duration) (Peer, bool) {
	deadline := time.Now().Add(wait)
	for {
		for _, p := range s.List() {
			if net.ParseIP(p.Addr) != nil && p.Addr == raddr.IP.String() {
				return p, true
			}
		}
		if time.Now().After(deadline) {
			return Peer{}, false
		}
		time.Sleep(50 * time.Millisecond)
	}
}
