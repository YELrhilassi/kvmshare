// Package zeroconf is a **vendored fork** of github.com/libp2p/zeroconf
// (itself a maintained fork of the archived grandcat/zeroconf; MIT
// license, see LICENSE) — vendored because the upstream forks still
// race on Shutdown: the receiver goroutines (recv4/recv6) call
// shutdownEnd.Add(1) *inside* themselves, so Shutdown's Wait() can slip
// past an Add that has not run yet — a textbook WaitGroup misuse that
// the race detector flags in kvmshare's own tests. The fix here is the
// standard one: every Add happens before its goroutine is spawned (see
// server.go, marked VENDORED-FIX), so Wait always has exact knowledge
// of outstanding receivers.
//
// Functionally it is Multicast DNS-SD (RFC 6762/6763) service
// registration, browsing and resolving — compatible with Avahi and
// Apple's Bonjour.
package zeroconf
