package main

import (
	"testing"

	"kvmshare/gui/internal/discovery"
)

// Trust and revoke are independent memberships: each setter is
// idempotent, works by short or full id, and touching one list must never
// disturb the other. An id can be trusted *and* revoked at the same time —
// revoke is the one that is enforced.
func TestTrustAndRevokeAreIndependent(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	short := full[:8]

	// Server side: trust, then revoke, without untrusting.
	if err := a.TrustClient(short, true); err != nil {
		t.Fatal(err)
	}
	cfg, _ := a.LoadConfig()
	if !idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatalf("TrustClient should record the id: %v", cfg.Network.TrustedIDs)
	}
	// Idempotent: trusting the full form of an already-trusted short id
	// does not add a duplicate entry.
	if err := a.TrustClient(full, true); err != nil {
		t.Fatal(err)
	}
	if err := a.RevokeClient(full, true); err != nil {
		t.Fatal(err)
	}
	cfg, _ = a.LoadConfig()
	if !idRevoked(cfg.Network.RevokedIDs, full) {
		t.Fatalf("RevokeClient should record the revocation: %v", cfg.Network.RevokedIDs)
	}
	if !idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatalf("revoking must NOT untrust — the lists are independent: %v", cfg.Network.TrustedIDs)
	}
	// Un-revoking leaves trust alone too.
	if err := a.RevokeClient(full, false); err != nil {
		t.Fatal(err)
	}
	cfg, _ = a.LoadConfig()
	if idRevoked(cfg.Network.RevokedIDs, full) {
		t.Fatalf("RevokeClient(false) should clear the revocation: %v", cfg.Network.RevokedIDs)
	}
	if !idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatal("un-revoking must not untrust")
	}
	// Untrusting leaves the (cleared) revocation list untouched.
	if err := a.TrustClient(full, false); err != nil {
		t.Fatal(err)
	}
	cfg, _ = a.LoadConfig()
	if idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatalf("TrustClient(false) should remove the trust: %v", cfg.Network.TrustedIDs)
	}

	// Client side: same independence.
	if err := a.TrustServer(short, true); err != nil {
		t.Fatal(err)
	}
	if !idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatalf("TrustServer should record the id: %v", a.GetSettings().TrustedServers)
	}
	if err := a.RevokeServer(short, true); err != nil {
		t.Fatal(err)
	}
	s := a.GetSettings()
	if !idRevoked(s.RevokedServers, full) {
		t.Fatalf("RevokeServer should record the revocation: %v", s.RevokedServers)
	}
	if !idTrusted(s.TrustedServers, full) {
		t.Fatalf("revoking a server must not untrust it: %v", s.TrustedServers)
	}
	if err := a.RevokeServer(full, false); err != nil {
		t.Fatal(err)
	}
	if idRevoked(a.GetSettings().RevokedServers, full) {
		t.Fatal("RevokeServer(false) should clear the revocation")
	}
}

// Revoking by a short prefix must match THAT id only — never a different
// entry that merely shares no overlap.
func TestRevokeMatchesOnlyItsID(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"

	if err := a.TrustClient(full, true); err != nil {
		t.Fatal(err)
	}
	if err := a.TrustClient("aaaaaaaa11111111", true); err != nil {
		t.Fatal(err)
	}
	if err := a.RevokeClient(full[:6], true); err != nil {
		t.Fatal(err)
	}
	cfg, _ := a.LoadConfig()
	if !idRevoked(cfg.Network.RevokedIDs, full) {
		t.Fatal("the revoked prefix must cover the full id")
	}
	if idRevoked(cfg.Network.RevokedIDs, "aaaaaaaa11111111") {
		t.Fatal("revoke must not touch a different id")
	}
	// Revocation is additive: both machine ids stay trusted.
	if len(cfg.Network.TrustedIDs) != 2 {
		t.Fatalf("revoking must not remove trust entries: %v", cfg.Network.TrustedIDs)
	}
}

// A too-short id is refused outright: a 3-char entry would match far too
// much to be safe.
func TestRevokeServerRejectsShortID(t *testing.T) {
	a, _ := newTestApp(t)
	if err := a.RevokeServer("abc", true); err == nil {
		t.Fatal("a 3-char id should be refused")
	}
	if err := a.RevokeClient("abc", true); err == nil {
		t.Fatal("a 3-char id should be refused")
	}
}

// The full pairing path through the App's Host adapter: a "connect"
// request from a local server is honored on first use (trust recorded)
// and this machine's client starts pointed at the sender. Wire-level
// parsing, classification and address normalization are covered in
// internal/discovery; the queue-then-process contract by the engine's
// own worker tests.
func TestPairingRequestConnects(t *testing.T) {
	a, _ := newTestApp(t)

	// Trust-on-first-use: the sender's id starts untrusted, pairing is
	// on by default, so the request is honored — and remembered.
	if idTrusted(a.GetSettings().TrustedServers, "cccccccc22222222") {
		t.Fatal("precondition: sender must start untrusted")
	}
	a.OnPairRequest(discovery.PairRequest{ID: "cccccccc22222222", Name: "desk", Addr: "192.168.1.86:24800"})

	if !idTrusted(a.GetSettings().TrustedServers, "cccccccc22222222") {
		t.Fatalf("pairing did not record the server as trusted: %v", a.GetSettings().TrustedServers)
	}
	if a.GetSettings().ClientAddr != "192.168.1.86:24800" {
		t.Fatalf("client addr = %q, want 192.168.1.86:24800", a.GetSettings().ClientAddr)
	}
	if !a.ClientRunning() {
		t.Fatal("client did not start after a pairing request")
	}

	// A request from an unknown server with pairing disabled is refused
	// (and must not start a client).
	a2, _ := newTestApp(t)
	s := a2.GetSettings()
	s.AcceptPairing = false
	if err := a2.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	a2.OnPairRequest(discovery.PairRequest{ID: "dddddddd33333333", Name: "stranger", Addr: "192.168.1.90:24800"})
	if idTrusted(a2.GetSettings().TrustedServers, "dddddddd33333333") {
		t.Fatal("untrusted request honored with pairing off")
	}
	if a2.ClientRunning() {
		t.Fatal("untrusted request started a client with pairing off")
	}
}
