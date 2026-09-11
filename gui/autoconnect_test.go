package main

import (
	"net"
	"os"
	"path/filepath"
	"testing"
	"time"

	"kvmshare/gui/internal/discovery"
)

// Revocation must be sticky and must survive the paths that would
// otherwise reconnect: it is checked before the pairing toggle and
// before the last-used-address fallback, and a plain settings write
// (which the frontend does for unrelated fields) must not clear it.
// Trusting the same id does not clear it either — the lists are
// independent, and revoke wins.
func TestRevokeServerIsSticky(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	short := full[:8]

	if err := a.TrustServer(short, true); err != nil {
		t.Fatal(err)
	}
	if !idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatal("trust should record the id")
	}

	if err := a.RevokeServer(short, true); err != nil {
		t.Fatal(err)
	}
	s := a.GetSettings()
	if !idRevoked(s.RevokedServers, full) {
		t.Fatalf("revoke must record the id as revoked: %v", s.RevokedServers)
	}
	if !idTrusted(s.TrustedServers, full) {
		t.Fatalf("revoking must not untrust — the lists are independent: %v", s.TrustedServers)
	}

	// A settings write that knows nothing about revocation (as the
	// frontend's `{...settings}` edits are) must not wipe it.
	next := a.GetSettings()
	next.ClientName = "renamed"
	next.RevokedServers = nil
	if err := a.SetSettings(next); err != nil {
		t.Fatal(err)
	}
	if !idRevoked(a.GetSettings().RevokedServers, full) {
		t.Fatal("revocation must survive an unrelated settings write")
	}

	// Trusting the revoked id again must NOT re-open it; only an explicit
	// un-revoke does.
	if err := a.TrustServer(full, true); err != nil {
		t.Fatal(err)
	}
	if !idRevoked(a.GetSettings().RevokedServers, full) {
		t.Fatal("trusting must not clear a revocation")
	}
	if err := a.RevokeServer(full, false); err != nil {
		t.Fatal(err)
	}
	if idRevoked(a.GetSettings().RevokedServers, full) {
		t.Fatal("un-revoking must clear it")
	}
}

// The client process must receive the revoked-server list so it can
// refuse a session the GUI never screened (a typed address, a
// reconnect): the server's id only becomes known from its `Welcome`, so
// the check has to live in the client too.
func TestClientGetsRevokedListEnv(t *testing.T) {
	a, _ := newTestApp(t)
	if err := a.RevokeServer("70b97d38631dda4b8f6ef627d753022d", true); err != nil {
		t.Fatal(err)
	}
	if err := a.RevokeServer("aabbccdd11223344", true); err != nil {
		t.Fatal(err)
	}
	a.mu.Lock()
	env := a.clientRevokedEnvLocked()
	a.mu.Unlock()
	if len(env) != 1 || env[0] != "KVMSHARE_REVOKED_IDS=70b97d38631dda4b8f6ef627d753022d,aabbccdd11223344" {
		t.Fatalf("revoked env = %v", env)
	}
}

// A pairing request from a revoked server must be refused even though
// pairing is enabled by default — the client must not start. Trusting it
// does not re-open it; only un-revoking does.
func TestRevokedServerPairingIsRefused(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	if !a.GetSettings().AcceptPairing {
		t.Fatal("pairing should default to on for this test to be meaningful")
	}
	if err := a.RevokeServer(full, true); err != nil {
		t.Fatal(err)
	}
	a.OnPairRequest(discovery.PairRequest{ID: full, Name: "hp", Addr: "127.0.0.1:24800"})
	if a.ClientRunning() {
		t.Fatal("a revoked server must not be able to start the client by pairing")
	}

	// Trusting the revoked id does not help — revoke still wins.
	if err := a.TrustServer(full, true); err != nil {
		t.Fatal(err)
	}
	a.OnPairRequest(discovery.PairRequest{ID: full, Name: "hp", Addr: "127.0.0.1:24800"})
	if a.ClientRunning() {
		t.Fatal("trusting a revoked server must not re-open pairing")
	}

	// An explicit un-revoke re-opens the door.
	if err := a.RevokeServer(full, false); err != nil {
		t.Fatal(err)
	}
	a.OnPairRequest(discovery.PairRequest{ID: full, Name: "hp", Addr: "127.0.0.1:24800"})
	if !a.ClientRunning() {
		t.Fatal("an un-revoked server's pairing request should start the client")
	}
}

// The backoff doubles from the floor and never exceeds the ceiling.
func TestAutoConnectBackoff(t *testing.T) {
	cases := []struct {
		failures int
		want     time.Duration
	}{
		{0, 0},
		{1, autoConnectBackoffMin},
		{2, 2 * autoConnectBackoffMin},
		{3, 4 * autoConnectBackoffMin},
		{10, autoConnectBackoffMax},
		{100, autoConnectBackoffMax},
	}
	for _, c := range cases {
		if got := backoffDelay(c.failures); got != c.want {
			t.Errorf("backoffDelay(%d) = %v, want %v", c.failures, got, c.want)
		}
	}
}

// Only addresses on this machine's own interfaces count as "same
// network": loopback for single-machine setups, the machine's LAN
// subnet, but nothing reachable only through a router.
func TestPeerOnLocalNetwork(t *testing.T) {
	if !peerOnLocalNetwork("127.0.0.1") {
		t.Fatal("loopback should count as local")
	}
	if peerOnLocalNetwork("8.8.8.8") {
		t.Fatal("a public address must not count as local")
	}
	if peerOnLocalNetwork("") || peerOnLocalNetwork("not-an-ip") {
		t.Fatal("unparsable addresses must not count as local")
	}
	// This machine's own LAN address (when it has one) is local.
	if own := ownLANAddrForTest(); own != "" && !peerOnLocalNetwork(own) {
		t.Fatalf("this machine's own address %s should count as local", own)
	}
}

// The stopped marker distinguishes a server-requested disconnect (the
// client exited on purpose — auto-connect must hold off) from a
// transient "disconnected" while the client keeps retrying.
func TestClientStoppedByServerMarker(t *testing.T) {
	a, _ := newTestApp(t)
	path := filepath.Join(a.stateDir, "client.state")

	if a.clientStoppedByServer() {
		t.Fatal("a missing state file must not read as a requested stop")
	}
	if err := os.WriteFile(path, []byte("status=disconnected\nserver=192.168.1.72:24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if a.clientStoppedByServer() {
		t.Fatal("a transient disconnect must not read as a requested stop")
	}
	if err := os.WriteFile(path, []byte("status=disconnected\nserver=192.168.1.72:24800\nstopped=1\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if !a.clientStoppedByServer() {
		t.Fatal("the stopped marker must read as a requested stop")
	}
}

// A revoked target is refused before anything is started, so a revoked
// server can never even briefly hold a session.
func TestConnectToRevokedPeerIsRefused(t *testing.T) {
	a, _ := newTestApp(t)
	// With nothing discovered there is nothing to resolve, so the guard
	// is a no-op (the client-side check still covers the handshake).
	if _, ok := a.revokedPeerAtAddr("192.168.1.72:24800"); ok {
		t.Fatal("no peer is discovered, so nothing can match")
	}

	if !sameHost("192.168.1.72:24800", "192.168.1.72:9999") {
		t.Fatal("same host, different port must match")
	}
	if sameHost("", "192.168.1.72:24800") || sameHost("192.168.1.72:24800", "") {
		t.Fatal("an empty address must never match")
	}
	if sameHost("192.168.1.72:24800", "192.168.1.73:24800") {
		t.Fatal("different hosts must not match")
	}
}

// ownLANAddrForTest returns this machine's first private IPv4 address,
// or "" when it has none (CI containers and single-machine setups).
func ownLANAddrForTest() string {
	ifaces, err := net.Interfaces()
	if err != nil {
		return ""
	}
	for _, ifc := range ifaces {
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
			if ip != nil && ip.IsPrivate() {
				return ip.String()
			}
		}
	}
	return ""
}
