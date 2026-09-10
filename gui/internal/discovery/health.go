package discovery

import (
	"log"
	"sync/atomic"
	"time"
)

// Health-watch cadence and thresholds. quietAfter must comfortably
// exceed beaconInterval: two live machines exchange beacons every 2 s,
// so a healthy session is never quiet this long.
const (
	watchInterval = 30 * time.Second
	quietAfter    = 90 * time.Second
	warnCooldown  = 10 * time.Minute
)

// warnOnce logs a warning at most once per cooldown window (keyed by
// an atomic timestamp), so a persistent condition costs one line per
// cooldown instead of one per tick.
type warnOnce struct {
	last atomic.Int64 // unix nanos of the last emission
}

func (w *warnOnce) log(msg string, err error, cooldown time.Duration) {
	now := time.Now().UnixNano()
	prev := w.last.Load()
	if prev != 0 && now-prev < cooldown.Nanoseconds() {
		return
	}
	if !w.last.CompareAndSwap(prev, now) {
		return // another goroutine won the race
	}
	if err != nil {
		log.Printf("kvmshare-gui: %s%v", msg, err)
		return
	}
	log.Printf("kvmshare-gui: %s", msg)
}

// warnCounters is the bounded-diagnostics state: one warning per
// condition per cooldown, so a broken network costs a handful of log
// lines per hour instead of one every tick.
type warnCounters struct {
	beacon warnOnce // socket problems on the send path
	listen warnOnce // socket problems on the receive path
	quiet  warnOnce // healthy sockets but hearing nothing
}

// watchLoop is the discovery layer's own pulse check. Every
// watchInterval it verifies the invariants that make discovery work —
// sockets alive, something heard recently — and, when they break, says
// so (once per cooldown per condition). This exists because discovery
// used to fail silently: a dead listener looked exactly like an empty
// network, and nobody could tell the difference from the outside.
func (s *Service) watchLoop() {
	ticker := time.NewTicker(watchInterval)
	defer ticker.Stop()
	for {
		select {
		case <-s.stop:
			return
		case <-ticker.C:
			s.checkHealth()
		}
	}
}

// checkHealth runs one watch pass. Deliberately cheap: two
// mutex-guarded reads, no syscalls.
func (s *Service) checkHealth() {
	s.mu.Lock()
	listening := s.listenConn != nil
	beaconing := s.beaconConn != nil
	var last time.Time
	for _, t := range s.seen {
		if t.After(last) {
			last = t
		}
	}
	nPeers := len(s.peers)
	s.mu.Unlock()
	now := time.Now()

	if !listening {
		s.warns.listen.log("discovery: receive socket down — rebuilding", nil, warnCooldown)
	}
	if !beaconing {
		s.warns.beacon.log("discovery: beacon socket down — rebuilding", nil, warnCooldown)
	}
	// Hearing nothing at all is itself a condition worth one line: with
	// two kvmshare machines on a LAN, beacons arrive every couple of
	// seconds. A long silent stretch with healthy sockets means the
	// network filters the discovery traffic (AP isolation, VLAN) —
	// exactly the case the user needs to know about, because the fix is
	// on the network side (or manual addresses). Only meaningful once
	// this engine has been up long enough to expect traffic
	// (quietAfter).
	if listening && beaconing && nPeers == 0 && now.Sub(last) > quietAfter {
		s.warns.quiet.log("discovery: healthy but hearing no beacons — the network may filter broadcast/multicast (AP isolation?); manual addresses still work", nil, warnCooldown)
	}
}
