package discovery

import (
	"net"
	"sync"
	"time"
)

// targetsCacheTTL bounds how long a cached set of broadcast/probe
// targets stays fresh. The interface enumeration used to run on every
// 2 s beacon tick; caching makes steady-state discovery cost almost no
// syscalls, and a Refresh forces a re-enumeration so a network change
// is picked up within one interval instead of at process restart.
const targetsCacheTTL = 60 * time.Second

// networkCache holds one enumeration of this machine's interfaces, in
// the two forms the channels need. Guarded by its own mutex — target
// lookups happen on every beacon tick and must not serialize behind
// the peer map.
type networkCache struct {
	mu        sync.Mutex
	at        time.Time
	broadcast []*net.UDPAddr
	probe     []*net.UDPAddr
}

// broadcastAddrs returns the destinations for beacons: the limited
// broadcast plus each interface's subnet-directed broadcast (some
// routers only forward one of the two forms). Cached.
func (s *Service) broadcastAddrs() []*net.UDPAddr {
	c := &s.netCache
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.broadcast == nil || time.Since(c.at) > targetsCacheTTL {
		c.broadcast = computeBroadcastAddrs()
		c.at = time.Now()
	}
	return c.broadcast
}

// probeTargets is the cached per-subnet candidate list (see
// computeProbeTargets).
func (s *Service) probeTargets() []*net.UDPAddr {
	c := &s.netCache
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.probe == nil || time.Since(c.at) > targetsCacheTTL {
		c.probe = computeProbeTargets()
		c.at = time.Now()
	}
	return c.probe
}

// invalidateTargets drops the cached interface enumeration — called on
// an explicit refresh, and cheap enough to call after any network
// change suspicion (a wrong cache costs one stale interval, not data).
func (s *Service) invalidateTargets() {
	c := &s.netCache
	c.mu.Lock()
	c.broadcast = nil
	c.probe = nil
	c.at = time.Time{}
	c.mu.Unlock()
}

// computeBroadcastAddrs enumerates the actual destinations: the limited
// broadcast plus each interface's subnet-directed broadcast, derived
// from the address with a /24 mask (the overwhelmingly common
// home/office case).
func computeBroadcastAddrs() []*net.UDPAddr {
	out := []*net.UDPAddr{{IP: net.IPv4bcast, Port: Port}}
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
			out = append(out, &net.UDPAddr{IP: bcast, Port: Port})
		}
	}
	return out
}

// computeProbeTargets lists every candidate host on this machine's /24
// subnets (all hosts minus ourselves and the broadcast addresses). Only
// /24 subnets are probed — the sweep stays at 254 packets per interval,
// and anything larger is left to the broadcast/mDNS channels (and
// manual addresses).
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
				out = append(out, &net.UDPAddr{IP: cand, Port: Port})
			}
		}
	}
	return out
}
