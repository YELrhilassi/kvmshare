package main

// keycapture.go — the key-capture session machinery, platform-free so it
// is testable on every OS (the Windows-only hook that consults it lives
// in keycapture_windows.go; every other platform gets the no-op stub in
// keycapture_other.go and the recorder falls back to DOM events there).
//
// ## The model
//
// A recording session is one token: the frontend asks to record, the
// backend arms a session under that token, and every physical key event
// is reported to the page under `keycapture:<token>` while the session
// lives. While it lives, the platform hook suppresses the events — the
// recorder owns the keyboard, so the chord being recorded (Win+Tab)
// never reaches the shell.
//
// ## The two failure modes this design rules out
//
//   - **A page that dies mid-recording.** Every session carries a lease
//     renewed by the page's own key traffic; a lapsed session is torn
//     down by the next key event and the page is told once
//     (`keycapture:expired`). Keys pass through again immediately — a
//     crashed page can never leave the machine silent. The watchdog
//     covers the no-key-at-all case.
//   - **Session armed but no events flowing.** Arming and unarming are
//     decoupled from hook installation: the hook installs in the
//     background once, and the GUI bridge call returns as soon as the
//     session is armed — never blocked by, and never blocking, the hook
//     thread. A session armed while the hook is not (yet) installed is
//     inert by construction: suppression decisions happen only inside
//     the hook callback, which by definition only runs once installed.

import (
	"errors"
	"sync"
	"time"
)

// captureTTL bounds a capture session without a renewal. Long enough
// for any deliberate recording; short enough that a crashed page
// cannot leave the keyboard dead for minutes.
const captureTTL = 2 * time.Minute

// captureExpiredEvent is the TTL-lapse notification to the page.
const captureExpiredEvent = "keycapture:expired"

// keyEvent is one key transition delivered to the page.
type keyEvent struct {
	Token    string `json:"token"`
	Down     bool   `json:"down"`
	Repeat   bool   `json:"repeat"`
	VK       uint32 `json:"vk"`
	Scan     uint32 `json:"scan"`
	Extended bool   `json:"extended"`
	Control  bool   `json:"control"`
	Alt      bool   `json:"alt"`
	Shift    bool   `json:"shift"`
	Meta     bool   `json:"meta"`
}

// eventSink is what the session registry delivers events through. The
// Wails event manager satisfies this in production; tests supply their
// own. Emit must never block long enough to matter for a keystroke
// (Wails' emit hands off to an unbounded mailbox — see
// pkg/application/events.go in wails v3).
type eventSink interface {
	Emit(name string, data ...any) bool
}

// captureSession is one active recording session. expires is guarded by
// the registry's mutex.
type captureSession struct {
	token   string
	expires time.Time
}

// captureRegistry owns the at-most-one recording session. The platform
// hook consults it per key event (decideFor); the GUI bridge threads
// write it (arm/disarm/renew).
type captureRegistry struct {
	mu        sync.Mutex
	session   *captureSession
	sink      eventSink
	wakeHook  func() // nudges the hook thread; may be nil in tests
	installed bool   // hook present (diagnostics/testing)
}

var keyCapture = newCaptureRegistry()

var (
	errCaptureTokenRequired   = errors.New("capture token required")
	errCaptureSinkUnavailable = errors.New("app event sink not ready")
)

func newCaptureRegistry() *captureRegistry {
	return &captureRegistry{}
}

// arm starts (or replaces) the recording session for token. Replacing
// is safe by design: a new recorder mount takes over from a stale one
// without anyone stopping the old session first. The sink must be the
// live event manager — arming without one is refused, an armed session
// that can never report would only swallow keys silently.
func (r *captureRegistry) arm(token string, sink eventSink) error {
	if token == "" {
		return errCaptureTokenRequired
	}
	if sink == nil {
		return errCaptureSinkUnavailable
	}
	r.mu.Lock()
	r.session = &captureSession{token: token, expires: time.Now().Add(captureTTL)}
	r.sink = sink
	r.mu.Unlock()
	r.wakeHookNow()
	return nil
}

// disarm ends the session with this token (a stale token is a no-op).
func (r *captureRegistry) disarm(token string) {
	r.mu.Lock()
	if r.session != nil && r.session.token == token {
		r.session = nil
	}
	r.mu.Unlock()
	r.wakeHookNow()
}

// renew extends the session (the recorder renews on every key it
// receives, so a long thoughtful pause then a key still works).
func (r *captureRegistry) renew(token string) {
	r.mu.Lock()
	if r.session != nil && r.session.token == token {
		r.session.expires = time.Now().Add(captureTTL)
	}
	r.mu.Unlock()
}

// setSink wires the live event manager (called once at app start). A
// nil sink is not an error — the app may simply not be wired yet;
// arming refuses until it is.
func (r *captureRegistry) setSink(sink eventSink) {
	r.mu.Lock()
	r.sink = sink
	r.mu.Unlock()
}

// markHookInstalled records whether the platform hook is actually
// delivering events (diagnostics and tests).
func (r *captureRegistry) markHookInstalled(installed bool) {
	r.mu.Lock()
	r.installed = installed
	r.mu.Unlock()
}

// wakeHookNow nudges the hook thread that the session state changed
// (nil-safe: tests and non-Windows platforms run without a hook
// thread).
func (r *captureRegistry) wakeHookNow() {
	r.mu.Lock()
	wake := r.wakeHook
	r.mu.Unlock()
	if wake != nil {
		wake()
	}
}

// report delivers one key event to the page under the session's event
// name. Safe to call with a nil sink (no-op).
func (r *captureRegistry) report(token string, ev keyEvent) {
	r.mu.Lock()
	sink := r.sink
	r.mu.Unlock()
	if sink != nil {
		sink.Emit("keycapture:"+token, ev)
	}
}

// decideFor is the per-keystroke decision the platform hook consults.
// It applies the TTL first (a lapsed session ends here — the user
// regains their keyboard on this very event), then returns the token
// to report under and whether to suppress.
func (r *captureRegistry) decideFor(now time.Time) (token string, suppress bool, sink eventSink, expired bool) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.session == nil {
		return "", false, nil, false
	}
	if now.After(r.session.expires) {
		tok := r.session.token
		sink := r.sink
		r.session = nil
		return tok, false, sink, true
	}
	return r.session.token, true, r.sink, false
}

// lapsed returns the token of a session whose lease is over, ending
// it. The watchdog uses this to bound the armed-but-page-gone case
// when no keys arrive at all.
func (r *captureRegistry) lapsed(now time.Time) (token string, sink eventSink, ok bool) {
	r.mu.Lock()
	if r.session == nil || !now.After(r.session.expires) {
		r.mu.Unlock()
		return "", nil, false
	}
	token = r.session.token
	sink = r.sink
	r.session = nil
	r.mu.Unlock()
	return token, sink, true
}

// runCaptureWatchdog ends lapsed sessions even when no key arrives
// (a user arms a recording and touches nothing): without this the
// armed session would suppress keys for the full TTL with the page
// gone. The cadence is deliberately coarse — the per-key path expires
// immediately anyway; this only bounds the no-key case.
func runCaptureWatchdog(r *captureRegistry) {
	t := time.NewTicker(captureTTL / 4)
	for range t.C {
		if token, sink, ok := r.lapsed(time.Now()); ok && sink != nil {
			sink.Emit(captureExpiredEvent, keyEvent{Token: token})
		}
	}
}
