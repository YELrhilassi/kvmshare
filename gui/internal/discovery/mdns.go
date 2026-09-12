package discovery

import (
	"context"
	"strconv"
	"strings"
	"time"

	"kvmshare/gui/internal/discovery/zeroconf"
)

// Republish (re)advertises this machine over mDNS under the *current*
// role. Called at startup and on role changes. Same contract as the
// UDP beacon: advertise what is actually running (see
// Host.AdvertisedRole), never what a mode dropdown says.
func (s *Service) Republish() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reg != nil {
		s.reg.Shutdown()
		s.reg = nil
	}
	role, running := s.host.AdvertisedRole()
	id := s.host.MachineID()
	port := s.host.ServerPort()
	reg, err := zeroconf.Register(
		"kvmshare-"+id,
		serviceType,
		"local.",
		port,
		[]string{
			"id=" + id,
			"name=" + s.host.MachineName(),
			"role=" + role,
			"port=" + strconv.Itoa(port),
			"running=" + strconv.FormatBool(running),
		},
		nil,
	)
	if err != nil {
		s.reg = nil
		return
	}
	s.reg = reg
}

// browse runs until the process ends, maintaining the peer map from
// mDNS. mDNS is the best-effort second channel: every failure here is
// silent by design (the broadcast channel is the primary one), and the
// context keeps the resolver's goroutines from outliving the engine in
// embedders that stop it.
func (s *Service) browse() {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	entries := make(chan *zeroconf.ServiceEntry, 8)
	resolver, err := zeroconf.NewResolver()
	if err != nil {
		return
	}
	go func() {
		_ = resolver.Browse(ctx, serviceType, "local.", entries)
	}()

	for {
		select {
		case <-s.stop:
			return
		case e, ok := <-entries:
			if !ok {
				continue
			}
			s.upsertMDNS(e)
		}
	}
}

// upsertMDNS records one mDNS announcement. mDNS wins on fields it
// knows, but must not resurrect a peer the broadcast/probe channels
// have declared dead — the peer map is shared, and the mDNS library
// never tells us when a service disappears (only liveness ageing
// removes peers).
func (s *Service) upsertMDNS(e *zeroconf.ServiceEntry) {
	var id, name, role string
	var running bool
	port := e.Port
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
	if id == "" || id == s.host.MachineID() {
		return
	}
	addr := ""
	if len(e.AddrIPv4) > 0 {
		addr = e.AddrIPv4[0].String()
	} else if len(e.AddrIPv6) > 0 {
		addr = e.AddrIPv6[0].String()
	}
	s.mu.Lock()
	if existing, ok := s.peers[id]; ok && existing.Source != SourceMDNS && time.Since(s.seen[id]) > peerTTL {
		s.mu.Unlock()
		return // broadcast already aged this peer out — do not resurrect
	}
	s.peers[id] = &Peer{ID: id, Name: name, Role: role, Addr: addr, Port: port, Source: SourceMDNS, Active: running}
	s.seen[id] = time.Now()
	s.mu.Unlock()
}

// atoi is strconv.Atoi with the error folded into the ok-bool the TXT
// parsing wants.
func atoi(s string) (int, bool) {
	n, err := strconv.Atoi(s)
	return n, err == nil
}
