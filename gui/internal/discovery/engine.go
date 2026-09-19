package discovery

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"time"
)

// engine.go — the traffic loops and the socket plumbing they share.
//
// Loop lifetime is owned by sessions (coordinator.go): the beacon and
// probe senders are born with a session context and die with it —
// `stopLoops` waits for them, so sessions never overlap. The listener is
// process-scoped instead (started once in Start): receiving costs
// nothing while idle (one goroutine parked in recvfrom) and it is what
// keeps an idle machine discoverable-by-request (probe replies, pairing
// invites) — the "manual discovery can never fail" property.

// loopContext mints the context for one session's sender loops.
func (s *Service) loopContext() context.Context {
	s.loopsCtxMu.Lock()
	defer s.loopsCtxMu.Unlock()
	if s.loopsCancel != nil {
		s.loopsCancel() // defensive: never two live session contexts
	}
	ctx, cancel := context.WithCancel(s.rootCtx)
	s.loopsCancel = cancel
	return ctx
}

// cancelLoops tears down the current session context.
func (s *Service) cancelLoops() {
	s.loopsCtxMu.Lock()
	defer s.loopsCtxMu.Unlock()
	if s.loopsCancel != nil {
		s.loopsCancel()
		s.loopsCancel = nil
	}
}

// beaconLoop broadcasts this machine's presence for the duration of its
// session, at the session's rate: full rate while seeking, the slow
// keep-visible rate while connected. On a fatal send failure the socket
// is closed and rebuilt, so a transient network error (interface flap,
// sleep/resume) costs one interval, not the whole session.
func (s *Service) beaconLoop(ctx context.Context, state Session) {
	defer s.loopsWG.Done()
	interval := beaconInterval
	if state == SessionConnected {
		interval = connectedInterval
	}
	conn := s.getBeaconSocket()
	if conn == nil {
		return // no socket possible (see getBeaconSocket): nothing to send with
	}
	s.beaconTick(conn)

	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			if cur := s.beaconSocketRef(); cur != conn {
				// stopLoops or a rebuild replaced the socket under us.
				conn = cur
				if conn == nil {
					return
				}
			}
			if !s.beaconTick(conn) {
				s.clearBeaconSocket(conn)
				if conn = s.getBeaconSocket(); conn == nil {
					return
				}
			}
		}
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
// and pairing commands (→ queue) on the same UDP port for the whole
// process lifetime. The socket is owned by this loop; on a fatal
// receive error it is rebuilt after a short retry, so a transient
// interface flap degrades one interval instead of silencing discovery
// forever (the old code returned on the first error and the GUI never
// listened again — observed live as a 213 KB unread backlog growing for
// minutes). Exits only when the process context ends.
func (s *Service) listenLoop(ctx context.Context) {
	for {
		if ctx.Err() != nil {
			return
		}
		conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: Port})
		if err != nil {
			s.warns.listen.log(fmt.Sprintf("discovery: cannot bind :%d (another kvmshare GUI? another app on this port?): ", Port), err, listenRetry)
			select {
			case <-ctx.Done():
				return
			case <-time.After(listenRetry):
			}
			continue
		}
		s.mu.Lock()
		s.listenConn = conn
		s.mu.Unlock()

		s.listenServe(ctx, conn)

		// Serve returned: socket is dead. Drop it; the next pass (or a
		// synchronous Refresh) rebuilds it.
		s.mu.Lock()
		if s.listenConn == conn {
			s.listenConn = nil
		}
		s.mu.Unlock()
		conn.Close()
	}
}

// listenServe reads until a fatal receive error or process end.
// Non-fatal errors (transient) are skipped without rebuilding.
func (s *Service) listenServe(ctx context.Context, conn *net.UDPConn) {
	buf := make([]byte, 2048)
	for {
		n, from, err := conn.ReadFromUDP(buf)
		if err != nil {
			if ctx.Err() != nil || isFatalUDPError(err) {
				return
			}
			continue // transient — keep reading
		}
		s.handleDatagram(buf[:n], from)
	}
}

// rebuildListener creates the receive socket synchronously — the path a
// manual Refresh takes when the background listener is down, so the
// sweep always runs on a socket that can actually hear the replies.
func (s *Service) rebuildListener() error {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{Port: Port})
	if err != nil {
		return fmt.Errorf("discovery: cannot bind :%d — %w", Port, err)
	}
	s.mu.Lock()
	if s.listenConn != nil {
		s.listenConn.Close()
	}
	s.listenConn = conn
	s.mu.Unlock()
	return nil
}

// getBeaconSocket returns the current send socket, creating one if
// absent. nil means the OS refused every socket — logged once per
// cooldown; the loop exits and the session watchdog reports the outcome.
func (s *Service) getBeaconSocket() *net.UDPConn {
	s.mu.Lock()
	conn := s.beaconConn
	s.mu.Unlock()
	if conn != nil {
		return conn
	}
	c, err := net.ListenUDP("udp4", nil)
	if err != nil {
		s.warns.beacon.log("discovery: no UDP socket for beacons: ", err, warnCooldown)
		return nil
	}
	if raw, err := c.SyscallConn(); err == nil {
		_ = raw.Control(func(fd uintptr) {
			_ = setBroadcast(fd)
		})
	}
	s.mu.Lock()
	s.beaconConn = c
	s.mu.Unlock()
	return c
}

// beaconSocketRef reads the current send socket without creating one.
func (s *Service) beaconSocketRef() *net.UDPConn {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.beaconConn
}

// clearBeaconSocket detaches and closes a dead send socket.
func (s *Service) clearBeaconSocket(conn *net.UDPConn) {
	s.mu.Lock()
	if s.beaconConn == conn {
		s.beaconConn = nil
	}
	s.mu.Unlock()
	conn.Close()
}

// clearSockets closes whatever send socket a session left behind.
// Callers hold the session transition lock.
func (s *Service) clearSockets() {
	s.mu.Lock()
	conn := s.beaconConn
	s.beaconConn = nil
	s.mu.Unlock()
	if conn != nil {
		conn.Close()
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
		case <-s.rootCtx.Done():
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
// the local subnets once per probeInterval, for the duration of its
// session. Machines reply with a regular beacon over unicast, which the
// listener records — so discovery works even on networks (AP client
// isolation, smart switches) that silently drop both broadcast and
// multicast. Costs a few tiny UDP packets per subnet per interval;
// hosts that never run kvmshare stay silent and cost nothing.
func (s *Service) probeLoop(ctx context.Context) {
	defer s.loopsWG.Done()
	ticker := time.NewTicker(probeInterval)
	defer ticker.Stop()
	s.probeOnce()
	for {
		select {
		case <-ctx.Done():
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
		return // listener not up yet — the next tick or Refresh retries
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
