package discovery

import (
	"encoding/json"
	"fmt"
	"net"
	"time"
)

// beaconLoop broadcasts this machine's presence every beaconInterval.
// The socket is owned by this loop: on fatal send failure it is closed
// and re-created, so a transient network error (interface flap, sleep/
// resume) cannot kill discovery for the process lifetime.
func (s *Service) beaconLoop() {
	for {
		if s.stopped() {
			return
		}
		conn, err := net.ListenUDP("udp4", nil)
		if err != nil {				s.warns.beacon.log("discovery: no UDP socket for beacons: ", err, warnCooldown)
			select {
			case <-s.stop:
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
		s.mu.Lock()
		s.beaconConn = conn
		s.mu.Unlock()

		s.beaconTick(conn)

		ticker := time.NewTicker(beaconInterval)
		for healthy := true; healthy; {
			select {
			case <-s.stop:
				ticker.Stop()
				s.clearSocket(conn)
				return
			case <-ticker.C:
				healthy = s.beaconTick(conn)
			}
		}
		ticker.Stop()
		// Socket went bad: close and rebuild on the next pass. Liveness
		// stamps are cleared too — a local network flap is exactly when
		// peers may have vanished, so everyone gets a fresh TTL window
		// once we can talk again.
		s.clearSocket(conn)
		s.mu.Lock()
		s.seen = map[string]time.Time{}
		s.mu.Unlock()
	}
}

// beaconTick sends one beacon round. Returns false when the socket is
// no longer usable and the loop should rebuild it.
func (s *Service) beaconTick(conn *net.UDPConn) bool {
	payload, _ := json.Marshal(s.beaconPayloadFor())
	dead := false
	for _, dst := range s.broadcastAddrs() {
		if _, err := conn.WriteToUDP(payload, dst); err != nil {
			if isFatalUDPError(err) {
				dead = true
			}
		}
	}
	return !dead
}

// beaconPayloadFor builds this machine's current announcement from the
// host's facts.
func (s *Service) beaconPayloadFor() beaconPayload {
	role, running := s.host.AdvertisedRole()
	return beaconPayload{
		ID:      s.host.MachineID(),
		Name:    s.host.MachineName(),
		Role:    role,
		Port:    s.host.ServerPort(),
		Running: running,
	}
}

// listenLoop receives beacons (→ peer list), subnet probes (→ reply)
// and pairing commands (→ queue) on the same UDP port. The socket is
// owned by this loop; on a fatal receive error the socket is closed and
// rebuilt, so a transient interface flap degrades one interval instead
// of silencing discovery forever (the old code returned on the first
// error and the GUI never listened again — observed live as a 213 KB
// unread backlog growing for minutes).
func (s *Service) listenLoop() {
	for {
		if s.stopped() {
			return
		}
		conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: Port})
		if err != nil {
			s.warns.listen.log(fmt.Sprintf("discovery: cannot bind :%d (another kvmshare GUI? another app on this port?): ", Port), err, listenRetry)
			select {
			case <-s.stop:
				return
			case <-time.After(listenRetry):
			}
			continue
		}
		s.mu.Lock()
		s.listenConn = conn
		s.mu.Unlock()

		s.listenServe(conn)

		// Serve returned: socket is dead or we are stopping.
		s.clearSocket(conn)
	}
}

// listenServe reads until a fatal receive error or shutdown. Non-fatal
// errors (transient) are skipped without rebuilding the socket.
func (s *Service) listenServe(conn *net.UDPConn) {
	buf := make([]byte, 2048)
	for {
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil {
			if s.stopped() || isFatalUDPError(err) {
				return
			}
			continue // transient — keep reading
		}
		s.handleDatagram(buf[:n], from)
	}
}

// handleDatagram classifies one datagram and routes it: probes are
// answered immediately (they are the discovery fallback, and replying
// is cheap), beacons are recorded, pairing requests are queued for the
// worker — the listener must never block on the connect flow.
func (s *Service) handleDatagram(data []byte, from *net.UDPAddr) {
	switch dg := classify(data); dg.kind {
	case kindProbe:
		s.replyProbe(from)
	case kindPair:
		job := pairJob{data: append([]byte(nil), data...), from: from, at: time.Now()}
		select {
		case s.pairQueue <- job:
		default:
			// Queue full: drop. The server's operator retries, and a
			// saturating queue means the worker is wedged — dropping
			// beats stalling the listener either way.
		}
	case kindBeacon:
		s.upsertBeacon(dg.bp, from)
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
func (s *Service) pairWorker() {
	for {
		select {
		case <-s.stop:
			return
		case job := <-s.pairQueue:
			if time.Since(job.at) > pairJobMaxAge {
				continue
			}
			s.handlePairing(job.data, job.from)
		}
	}
}

// probeLoop sends a "who is kvmshare here?" datagram to every host on
// the local subnets once per probeInterval. Machines reply with a
// regular beacon over unicast, which the listener records — so
// discovery works even on networks (AP client isolation, smart
// switches) that silently drop both broadcast and multicast. Costs a
// few tiny UDP packets per subnet per interval; hosts that never run
// kvmshare stay silent and cost nothing.
func (s *Service) probeLoop() {
	ticker := time.NewTicker(probeInterval)
	defer ticker.Stop()
	for {
		select {
		case <-s.stop:
			return
		case <-ticker.C:
			s.probeOnce()
		}
	}
}

// probeOnce pings every candidate host from the shared listener socket,
// so replies arrive on the same socket the listener already reads.
// Skips itself when the listener socket is down (replies must arrive on
// it — probing with replies undeliverable is pure waste).
func (s *Service) probeOnce() {
	s.mu.Lock()
	conn := s.listenConn
	s.mu.Unlock()
	if conn == nil {
		return // listener not up yet — try on the next interval
	}
	payload, _ := json.Marshal(probeMsg{Cmd: "probe", ID: s.host.MachineID()})
	for _, dst := range s.probeTargets() {
		_, _ = conn.WriteToUDP(payload, dst)
	}
}

// replyProbe answers a "who is kvmshare here?" probe with a normal
// beacon, unicast straight back to the prober.
func (s *Service) replyProbe(to *net.UDPAddr) {
	payload, _ := json.Marshal(s.beaconPayloadFor())
	conn, err := net.DialUDP("udp4", nil, to)
	if err != nil {
		return
	}
	defer conn.Close()
	_, _ = conn.Write(payload)
}

// clearSocket detaches and closes a loop-owned socket so the health
// watch can see the gap and the next rebuild pass starts clean. Both
// slots are compared: the caller knows which loop it is, the map does
// not care.
func (s *Service) clearSocket(conn *net.UDPConn) {
	s.mu.Lock()
	if s.listenConn == conn {
		s.listenConn = nil
	}
	if s.beaconConn == conn {
		s.beaconConn = nil
	}
	s.mu.Unlock()
	conn.Close()
}

// stopped reports whether shutdown has been requested.
func (s *Service) stopped() bool {
	select {
	case <-s.stop:
		return true
	default:
		return false
	}
}
