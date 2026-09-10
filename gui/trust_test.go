package main

import (
	"kvmshare/gui/internal/discovery"
	"testing"
)

// Trust and revoke must round-trip on both roles, by short or full id,
// and revoke must not remove a *different* id that merely shares a
// prefix longer than 4 chars with the target.
func TestTrustAndRevoke(t *testing.T) {
	a, _ := newTestApp(t)
	full := "70b97d38631dda4b8f6ef627d753022d"
	short := full[:8]

	// Server side.
	if err := a.TrustClient(short); err != nil {
		t.Fatal(err)
	}
	cfg, _ := a.LoadConfig()
	if !idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatalf("TrustClient should record the id: %v", cfg.Network.TrustedIDs)
	}
	// Idempotent.
	if err := a.TrustClient(full); err != nil {
		t.Fatal(err)
	}
	if err := a.RevokeClient(full); err != nil {
		t.Fatal(err)
	}
	cfg, _ = a.LoadConfig()
	if idTrusted(cfg.Network.TrustedIDs, full) {
		t.Fatalf("RevokeClient should remove the id: %v", cfg.Network.TrustedIDs)
	}

	// Client side.
	if err := a.TrustServer(short); err != nil {
		t.Fatal(err)
	}
	if !idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatalf("TrustServer should record the id: %v", a.GetSettings().TrustedServers)
	}
	if err := a.RevokeServer(short); err != nil {
		t.Fatal(err)
	}
	if idTrusted(a.GetSettings().TrustedServers, full) {
		t.Fatalf("RevokeServer should remove the id: %v", a.GetSettings().TrustedServers)
	}

	// Revoking by a short prefix must only remove entries matching THAT
	// id — never a different trusted entry.
	if err := a.TrustClient(full); err != nil {
		t.Fatal(err)
	}
	if err := a.TrustClient("aaaaaaaa11111111"); err != nil {
		t.Fatal(err)
	}
	if err := a.RevokeClient(full[:6]); err != nil {
		t.Fatal(err)
	}
	cfg, _ = a.LoadConfig()
	if len(cfg.Network.TrustedIDs) != 1 || cfg.Network.TrustedIDs[0] != "aaaaaaaa11111111" {
		t.Fatalf("revoke must only remove the matching id: %v", cfg.Network.TrustedIDs)
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
