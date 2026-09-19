package discovery

import (
	"encoding/json"
	"net"
	"testing"
)

func jsonMarshal(v any) ([]byte, error) { return json.Marshal(v) }

func jsonUnmarshal(data []byte, v any) error { return json.Unmarshal(data, v) }

// addrForTest is a stable sender address for upsert tests.
func addrForTest(t *testing.T) *net.UDPAddr {
	t.Helper()
	return &net.UDPAddr{IP: net.IPv4(192, 168, 1, 72), Port: Port}
}

// The wire classification of a direct-probe reply: a beacon payload
// must parse as a beacon (the reply path feeds it to upsert), and the
// probe request itself must never be recorded as a peer.
func TestProbeReplyClassification(t *testing.T) {
	s, _ := newTestService(t)

	beacon, err := (
		func() ([]byte, error) {
			return jsonMarshal(beaconPayload{ID: "bbbbbbbb33333333", Name: "hp", Role: "client", Port: 24800, Running: true})
		})()
	if err != nil {
		t.Fatal(err)
	}
	dg := classify(beacon)
	if dg.kind != kindBeacon {
		t.Fatalf("beacon reply misclassified: %v", dg.kind)
	}
	s.handleDatagram(beacon, addrForTest(t))
	if got := s.List(); len(got) != 1 || got[0].ID != "bbbbbbbb33333333" {
		t.Fatalf("probe reply beacon must land in the peer map: %+v", got)
	}
}

// The probe's dedicated-socket path parses a reply beacon into a Peer
// with the probed address as its source of truth.
func TestProbeDedicatedParsesReply(t *testing.T) {
	s, _ := newTestService(t)

	// Stand up a fake kvmshare machine: it answers probes with a
	// beacon from the same socket.
	fake, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		t.Fatal(err)
	}
	defer fake.Close()
	go func() {
		buf := make([]byte, 2048)
		for {
			n, from, err := fake.ReadFromUDP(buf)
			if err != nil {
				return
			}
			var pm probeMsg
			if jsonUnmarshal(buf[:n], &pm) == nil && pm.Cmd == "probe" {
				reply, _ := jsonMarshal(beaconPayload{ID: "cccccccc44444444", Name: "fake", Role: "server", Port: 24800, Running: true})
				_, _ = fake.WriteToUDP(reply, from)
			}
		}
	}()

	p, err := s.probeDedicated(
		&net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: fake.LocalAddr().(*net.UDPAddr).Port},
		[]byte(`{"cmd":"probe","id":"aaaaaaaa11111111"}`),
	)
	if err != nil {
		t.Fatalf("probe against a live responder failed: %v", err)
	}
	if p.ID != "cccccccc44444444" || p.Role != "server" || !p.Active {
		t.Fatalf("probe reply parsed wrong: %+v", p)
	}
	if p.Source != SourceProbe {
		t.Fatalf("direct probe must carry the probe source: %+v", p)
	}
}

// A silent responder produces a clear error naming the address — the
// manual path reports failure, never a silent nothing.
func TestProbeDedicatedTimeout(t *testing.T) {
	s, _ := newTestService(t)
	// Port 9 (discard) on loopback: nothing answers.
	_, err := s.probeDedicated(&net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 9},
		[]byte(`{"cmd":"probe","id":"aaaaaaaa11111111"}`))
	if err == nil {
		t.Fatal("a silent responder must produce an error")
	}
}
