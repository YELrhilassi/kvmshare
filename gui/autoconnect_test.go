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
func TestRevokeServerIsSticky(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	short := full[:8]

	if err := a.TrustServer(short); err != nil {
		t.Fatal(err)
	}
	if !idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatal("trust should record the id")
	}

	if err := a.RevokeServer(short); err != nil {
		t.Fatal(err)
	}
	s := a.GetSettings()
	if idTrusted(s.TrustedServers, full) {
		t.Fatalf("revoke must drop the id from trusted: %v", s.TrustedServers)
	}
	if !idRevoked(s.RevokedServers, full) {
		t.Fatalf("revoke must record the id as revoked: %v", s.RevokedServers)
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

	// Trusting again is the explicit opposite and must clear it.
	if err := a.TrustServer(full); err != nil {
		t.Fatal(err)
	}
	if idRevoked(a.GetSettings().RevokedServers, full) {
		t.Fatal("trust must clear a previous revocation")
	}
	if !idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatal("trust must record the id")
	}
}

// A pairing request from a revoked server must be refused even though
// pairing is enabled by default — the client must not start.
func TestRevokedServerPairingIsRefused(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	if !a.GetSettings().AcceptPairing {
		t.Fatal("pairing should default to on for this test to be meaningful")
	}
	if err := a.RevokeServer(full); err != nil {
		t.Fatal(err)
	}
	a.OnPairRequest(discovery.PairRequest{ID: full, Name: "hp", Addr: "127.0.0.1:24800"})
	if a.ClientRunning() {
		t.Fatal("a revoked server must not be able to start the client by pairing")
	}

	// Trusting it again re-opens the door.
	if err := a.TrustServer(full); err != nil {
		t.Fatal(err)
	}
	a.OnPairRequest(discovery.PairRequest{ID: full, Name: "hp", Addr: "127.0.0.1:24800"})
	if !a.ClientRunning() {
		t.Fatal("a trusted server's pairing request should start the client")
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
