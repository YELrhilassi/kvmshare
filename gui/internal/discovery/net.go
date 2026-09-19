package discovery

import (
	"encoding/json"
	"errors"
	"net"
	"syscall"
)

// Timing constants live in sessions.go — one owner for the duty-cycle
// numbers (session budgets, beacon/probe rates, peer TTL). This file
// keeps only the wire format and socket classification.

// isFatalUDPError classifies receive/send errors that mean the socket
// itself is gone (closed under us, interface removed) and must be
// rebuilt — as opposed to transient errors (routes settling, buffers
// momentarily full) that a healthy socket rides out.
func isFatalUDPError(err error) bool {
	if err == nil {
		return false
	}
	if errors.Is(err, net.ErrClosed) {
		return true
	}
	var oe *net.OpError
	if errors.As(err, &oe) {
		// EBADF/EINVAL on a live socket means it was closed under us.
		var se syscall.Errno
		if errors.As(oe.Err, &se) {
			return se == syscall.EBADF || se == syscall.EINVAL
		}
	}
	return false
}

// beaconPayload is the beacon shape sent on the wire (JSON, one line).
type beaconPayload struct {
	ID   string `json:"id"`
	Name string `json:"name"`
	Role string `json:"role"`
	Port int    `json:"port"`
	// Running tells listeners whether the advertised role is actually
	// running here, so a GUI that is open but not sharing anything never
	// shows up as a live "nearby" machine.
	Running bool `json:"running"`
}

// pairRequest is one "connect here" request on the wire.
type pairRequest struct {
	Cmd  string `json:"cmd"`  // "connect"
	ID   string `json:"id"`   // server machine id
	Name string `json:"name"` // server machine name
	Addr string `json:"addr"` // server address hint (host:port)
}

// probeMsg is one "who is kvmshare here?" request on the wire.
type probeMsg struct {
	Cmd string `json:"cmd"` // "probe"
	ID  string `json:"id"`
}

// datagram is one classified inbound datagram. Beacons, probes and
// pairing requests all share the `id` field, so the payload shape alone
// cannot tell them apart — the explicit `cmd` field does (its absence
// means beacon). Misclassifying a pairing request as a beacon used to
// swallow "connect here" silently; this type is the contract that
// prevents that, and every inbound datagram is classified exactly once.
type datagram struct {
	kind datagramKind
	bp   beaconPayload
	pr   pairRequest
	pm   probeMsg
}

type datagramKind int

const (
	kindUnknown datagramKind = iota
	kindBeacon
	kindProbe
	kindPair
)

// classify parses one datagram. Anything malformed is kindUnknown —
// callers drop it silently: the LAN is a hostile-ish channel and the
// protocol is deliberately tolerant.
func classify(data []byte) datagram {
	var probe probeMsg
	if json.Unmarshal(data, &probe) == nil && probe.Cmd == "probe" {
		return datagram{kind: kindProbe, pm: probe}
	}
	var pr pairRequest
	if json.Unmarshal(data, &pr) == nil && pr.Cmd == "connect" && pr.ID != "" {
		return datagram{kind: kindPair, pr: pr}
	}
	var bp beaconPayload
	if json.Unmarshal(data, &bp) == nil && bp.ID != "" {
		return datagram{kind: kindBeacon, bp: bp}
	}
	return datagram{}
}
