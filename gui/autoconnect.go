package main

// autoconnect.go — the client-side auto-connect policy.
//
// Auto-connect is a convenience with teeth: when it fires it *starts a
// role*, and starting the client stops a local server (one role per
// machine). Two consequences shape the whole design:
//
//   - The decision must be made atomically with the start. The old
//     watcher checked "is auto-connect allowed?", scanned the peer list,
//     then connected — and the operator could begin a server in that
//     gap, which the connect then killed. Every condition here is
//     re-checked under a.mu inside autoConnectLocked, the same lock the
//     operator's Start takes, so the two can never interleave.
//
//   - Every explicit operator decision outranks the convenience. A
//     running role blocks it; a revoked machine id is never a candidate
//     (not even when its address matches the last connection); and a
//     session the operator ended — Stop, or the server's disconnect
//     command — stays ended until they start again (see
//     AutoConnectPaused).
//
// The old watcher also only acted on a *transition* (a server newly
// seen) and never retried: the first connect usually races the server's
// own startup, loses, and then the peer stayed "seen" forever, so
// auto-connect looked like it "didn't always connect". This one keeps
// trying while a matching server is visible, spaced by an exponential
// backoff so a powered-off machine costs a few packets a minute rather
// than a connect storm.

import (
	"errors"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"kvmshare/gui/internal/discovery"
	"kvmshare/gui/internal/ids"
)

// Watcher cadence and retry backoff. The floor keeps a just-appeared
// server snappy; the ceiling keeps a stale one cheap.
const (
	autoConnectTick       = time.Second
	autoConnectBackoffMin = 2 * time.Second
	autoConnectBackoffMax = 30 * time.Second
)

// errAutoConnectOff reports that the connect was not attempted because
// auto-connect no longer applies (feature off, role running, paused).
// It is deliberately not a failure: it must not consume retry budget.
var errAutoConnectOff = errors.New("auto-connect not applicable")

// autoConnectAttempt is the watcher's per-server bookkeeping. It is only
// ever touched by the single watcher goroutine, so it carries no lock.
type autoConnectAttempt struct {
	lastTry  time.Time
	failures int
}

// autoConnectTarget is the candidate the watcher decided on, plus why —
// so selection can prefer a trusted id over a bare address match.
type autoConnectTarget struct {
	id          string
	addr        string
	trusted     bool
	matchesAddr bool
}

// AutoConnectLoop runs the auto-connect watcher for the life of the GUI.
func (a *App) AutoConnectLoop() {
	go func() {
		ticker := time.NewTicker(autoConnectTick)
		defer ticker.Stop()
		attempts := map[string]*autoConnectAttempt{}
		wasRunning := false
		for range ticker.C {
			wasRunning = a.autoConnectStep(attempts, wasRunning)
		}
	}()
}

// autoConnectStep is one watcher tick; it returns whether the client is
// running now (the next tick's transition baseline). It is safe to run
// at any moment: every condition it reads is re-validated under a.mu at
// connect time, so a mode switch, an operator Start/Stop, a pairing
// request or a revocation landing between ticks can never be overwritten.
func (a *App) autoConnectStep(attempts map[string]*autoConnectAttempt, wasRunning bool) bool {
	if a.ClientRunning() {
		return true // connected or connecting: nothing to do
	}
	// A client the *server* asked to disconnect exits on its own — the
	// GUI never called Stop — so the watcher has to notice and hold
	// auto-connect off, or the server's disconnect button would be undone
	// a tick later. Only a *transition* counts: the marker is also what a
	// just-started client leaves behind until it writes "connecting", and
	// acting on that stale read would re-pause a session the operator
	// just resumed.
	if wasRunning && a.clientStoppedByServer() {
		a.pauseAutoConnect()
	}
	if a.autoConnectPaused() {
		// Re-arming must not inherit a stale backoff from before the stop.
		a.resetAttempts(attempts)
		return false
	}
	target, ok := a.autoConnectTarget()
	if !ok {
		return false
	}
	st := attempts[target.id]
	if st == nil {
		st = &autoConnectAttempt{}
		attempts[target.id] = st
	}
	if time.Since(st.lastTry) < backoffDelay(st.failures) {
		return false
	}
	st.lastTry = time.Now()
	if err := a.autoConnectLocked(target); err != nil {
		if errors.Is(err, errAutoConnectOff) {
			// Not a failure — conditions changed. Start fresh next tick.
			delete(attempts, target.id)
			return false
		}
		st.failures++
		return false
	}
	delete(attempts, target.id) // connected (or already connected)
	return a.ClientRunning()    // this start may be async (checkStarted)
}

// resetAttempts empties the retry bookkeeping without replacing the map
// the loop closure holds.
func (a *App) resetAttempts(attempts map[string]*autoConnectAttempt) {
	for id := range attempts {
		delete(attempts, id)
	}
}

// backoffDelay is the wait before retry number `failures` (0 = try now).
// Exponential, doubling from autoConnectBackoffMin and capped at
// autoConnectBackoffMax.
func backoffDelay(failures int) time.Duration {
	if failures <= 0 {
		return 0
	}
	d := autoConnectBackoffMin
	for i := 1; i < failures; i++ {
		if d >= autoConnectBackoffMax {
			return autoConnectBackoffMax
		}
		d *= 2
	}
	if d > autoConnectBackoffMax {
		d = autoConnectBackoffMax
	}
	return d
}

// revokedPeerAtAddr resolves a target address (host:port) to a discovered
// machine and reports its id when that machine is revoked. Used to refuse
// a connect *before* starting anything: the client would refuse the
// session after the handshake anyway (see the revoked-ids env it is
// spawned with), but a moment-long session that appears and vanishes
// reads as a bug rather than as a policy.
func (a *App) revokedPeerAtAddr(addr string) (string, bool) {
	if a.disc == nil {
		return "", false
	}
	revoked := a.GetSettings().RevokedServers
	for _, p := range a.disc.List() {
		if !idRevoked(revoked, p.ID) {
			continue
		}
		target := net.JoinHostPort(p.Addr, strconv.Itoa(portOrDefault(p.Port)))
		if sameHost(target, addr) {
			return p.ID, true
		}
	}
	return "", false
}

// sameHost reports whether two host:port addresses name the same host
// (port ignored: a server may be rediscovered on a different port).
func sameHost(a, b string) bool {
	host := func(s string) string {
		s = strings.TrimSpace(s)
		if i := strings.LastIndex(s, ":"); i > 0 {
			return s[:i]
		}
		return s
	}
	ha, hb := host(a), host(b)
	return ha != "" && ha == hb
}

// autoConnectTarget picks the server this machine should auto-connect
// to, or reports none. Rules, in order:
//
//   - the feature must be on, the role client, and no role running here;
//   - only servers that are actually *running* — an open GUI with the
//     server stopped cannot accept a connection, so trying is pointless;
//   - only servers on this machine's own network (a peer reached across
//     a router is not a neighbour, and connecting to it unattended would
//     be surprising);
//   - never a revoked id, whatever its address;
//   - a trusted id wins over a bare last-used-address match.
func (a *App) autoConnectTarget() (autoConnectTarget, bool) {
	s := a.GetSettings()
	if s.Mode != ModeClient || !s.AutoConnect || s.AutoConnectPaused {
		return autoConnectTarget{}, false
	}
	if a.ClientRunning() || a.ServerRunning() {
		return autoConnectTarget{}, false
	}
	var best autoConnectTarget
	found := false
	for _, p := range a.DiscoverPeers() {
		if p.Role != "server" || !p.Active {
			continue
		}
		if idRevoked(s.RevokedServers, p.ID) {
			continue
		}
		if !peerOnLocalNetwork(p.Addr) {
			continue
		}
		trusted := ids.Trusted(s.TrustedServers, p.ID)
		matchesAddr := a.peerMatchesClientAddr(p)
		if !trusted && !matchesAddr {
			continue
		}
		cand := autoConnectTarget{
			id:          p.ID,
			addr:        net.JoinHostPort(p.Addr, strconv.Itoa(portOrDefault(p.Port))),
			trusted:     trusted,
			matchesAddr: matchesAddr,
		}
		if betterAutoConnectTarget(cand, best, found) {
			best = cand
			found = true
		}
	}
	return best, found
}

// betterAutoConnectTarget orders candidates: a trusted id beats an
// address match, and among equals the last-used address wins (that is
// the server the operator most likely means). Ties keep the earlier
// candidate, and the peer list is already sorted by name, so the choice
// is deterministic.
func betterAutoConnectTarget(cand, best autoConnectTarget, have bool) bool {
	if !have {
		return true
	}
	if cand.trusted != best.trusted {
		return cand.trusted
	}
	if cand.matchesAddr != best.matchesAddr {
		return cand.matchesAddr
	}
	return false
}

// autoConnectLocked starts the client against `target`, but only if
// every auto-connect condition still holds *now*, under a.mu. Returns
// errAutoConnectOff (not an error worth backing off on) when the world
// changed — a mode switch, a running role, a fresh operator stop, or a
// revocation that landed while we scanned.
func (a *App) autoConnectLocked(target autoConnectTarget) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if a.autoConnectBlockedLocked() {
		return errAutoConnectOff
	}
	if idRevoked(a.settings.RevokedServers, target.id) {
		return errAutoConnectOff
	}
	a.settings.ClientAddr = target.addr
	a.saveSettingsLocked()
	_, err := a.clientStartLocked()
	return err
}

// autoConnectBlocked reports whether auto-connect must stay quiet right
// now. Checked before the peer scan AND again under a.mu at connect
// time, because the decision goes stale fast.
func (a *App) autoConnectBlocked() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.autoConnectBlockedLocked()
}

// autoConnectBlockedLocked is autoConnectBlocked without taking a.mu.
// Callers hold it.
func (a *App) autoConnectBlockedLocked() bool {
	if a.settings.Mode != ModeClient || !a.settings.AutoConnect || a.settings.AutoConnectPaused {
		return true
	}
	// A role is running locally — a client (already connected) or a
	// server (explicitly shared). Auto-connecting over either would
	// fight the operator.
	return a.clientProc.running() || a.roleActive(roleClient) ||
		a.serverProc.running() || a.roleActive(roleServer)
}

// clientRevokedEnvLocked is the environment for the client process: this
// machine's revoked server ids, comma-separated. The client checks the
// server id it receives in `Welcome` against it, so a revoked server is
// refused even when the connect did not come through auto-connect or
// pairing (a typed address, a reconnect). Callers hold a.mu.
func (a *App) clientRevokedEnvLocked() []string {
	return []string{"KVMSHARE_REVOKED_IDS=" + strings.Join(a.settings.RevokedServers, ",")}
}

// pauseAutoConnectLocked holds auto-connect off until the operator
// starts the client again. Callers hold a.mu.
func (a *App) pauseAutoConnectLocked() {
	if a.settings.AutoConnectPaused {
		return
	}
	a.settings.AutoConnectPaused = true
	a.saveSettingsLocked()
}

// clearAutoConnectPauseLocked re-arms auto-connect. Callers hold a.mu.
func (a *App) clearAutoConnectPauseLocked() {
	if !a.settings.AutoConnectPaused {
		return
	}
	a.settings.AutoConnectPaused = false
	a.saveSettingsLocked()
}

// pauseAutoConnect is the locking form for the watcher goroutine.
func (a *App) pauseAutoConnect() {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.pauseAutoConnectLocked()
}

// autoConnectPaused reports whether an operator-ended session is holding
// auto-connect off.
func (a *App) autoConnectPaused() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings.AutoConnectPaused
}

// clientStoppedByServer reports whether the client's state file records
// a disconnect the *server* requested. The Rust client rewrites the file
// on every transition and marks only that exit with `stopped=1`, so a
// transient "disconnected" (the client still retrying) never matches.
func (a *App) clientStoppedByServer() bool {
	raw, err := os.ReadFile(filepath.Join(a.stateDir, "client.state"))
	if err != nil {
		return false
	}
	for _, line := range strings.Split(string(raw), "\n") {
		kv := strings.SplitN(strings.TrimSpace(line), "=", 2)
		if len(kv) == 2 && kv[0] == "stopped" && kv[1] == "1" {
			return true
		}
	}
	return false
}

// peerMatchesClientAddr reports whether a discovered server matches the
// address the operator last connected to (same host, any port).
func (a *App) peerMatchesClientAddr(p discovery.Peer) bool {
	a.mu.Lock()
	addr := strings.TrimSpace(a.settings.ClientAddr)
	a.mu.Unlock()
	host := addr
	if i := strings.LastIndex(host, ":"); i > 0 {
		host = host[:i]
	}
	return host != "" && host == p.Addr
}

// peerOnLocalNetwork reports whether `addr` is an address on one of this
// machine's own interfaces — the network the discovery channels operate
// on. Loopback counts (a single-machine setup pairs with itself), but a
// peer reached through a router does not.
func peerOnLocalNetwork(addr string) bool {
	ip := net.ParseIP(addr)
	if ip == nil {
		return false
	}
	if ip.IsLoopback() {
		return true
	}
	ifaces, err := net.Interfaces()
	if err != nil {
		return false
	}
	for _, ifc := range ifaces {
		if ifc.Flags&net.FlagUp == 0 {
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
			if ipn.Contains(ip) {
				return true
			}
		}
	}
	return false
}
