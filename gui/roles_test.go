package main

import (
	"testing"
	"time"
)

func TestProcessLifecycle(t *testing.T) {
	a, _ := newTestApp(t)

	started, err := a.ServerStart()
	if err != nil || !started {
		t.Fatalf("ServerStart: %v (started=%v)", err, started)
	}
	if !a.ServerRunning() {
		t.Fatal("server should be running")
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("server should be stopped")
	}

	// Second start of a stopped process works; starting twice is a no-op.
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
}

func TestClientLifecycle(t *testing.T) {
	a, _ := newTestApp(t)

	// No address configured → refused. (SetSettings itself rejects an
	// empty address in client mode, so poke the field directly.)
	a.mu.Lock()
	a.settings.ClientAddr = ""
	a.mu.Unlock()
	if _, err := a.ClientStart(); err == nil {
		t.Fatal("expected error when client address is empty")
	}

	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}

	// Configured → starts, runs, stops.
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if !a.ClientRunning() {
		t.Fatal("client should be running")
	}
	if err := a.ClientStop(); err != nil {
		t.Fatal(err)
	}
	if a.ClientRunning() {
		t.Fatal("client should be stopped")
	}
}

func TestStartActiveRespectsMode(t *testing.T) {
	a, _ := newTestApp(t)

	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	_ = a.SetSettings(s)

	if _, err := a.StartActive(); err != nil {
		t.Fatal(err)
	}
	if !a.ClientRunning() {
		t.Fatal("StartActive should have started the client in client mode")
	}
	if a.ServerRunning() {
		t.Fatal("server should not be running")
	}
	if err := a.StopActive(); err != nil {
		t.Fatal(err)
	}
	if a.ClientRunning() {
		t.Fatal("client should be stopped")
	}
}

func TestStopIsFast(t *testing.T) {
	a, _ := newTestApp(t)
	_, _ = a.ServerStart()
	start := time.Now()
	_ = a.ServerStop()
	if time.Since(start) > 2*time.Second {
		t.Fatalf("ServerStop took %v", time.Since(start))
	}
}

// The tray's Quit must leave nothing behind: every role process is
// stopped, whichever role is running.
func TestStopAllStopsRunningRoles(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should be running")
	}
	if err := a.StopAll(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("StopAll must stop the running server")
	}
	if a.ClientRunning() {
		t.Fatal("StopAll must leave no client running")
	}

	// Same for the client role.
	s := a.GetSettings()
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if err := a.StopAll(); err != nil {
		t.Fatal(err)
	}
	if a.ClientRunning() {
		t.Fatal("StopAll must stop the running client")
	}
}

// Changing the selected mode must NOT stop the running role: a toggle
// is a selection, and the running role keeps working until the user
// actually starts the other one (whose start stops it — see
// TestStartingOneRoleStopsTheOther). Stopping on a mere toggle would
// kill a working session the instant someone clicks the other chip.
func TestModeToggleKeepsRunningRole(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should be running")
	}

	// Toggling to client mode is a selection: the server keeps running.
	s := a.GetSettings()
	s.Mode = ModeClient
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("toggling mode must not stop the running server")
	}
	if a.settings.Mode != ModeClient {
		t.Fatal("the selected mode should now be client")
	}

	// Toggling back to server is also just a selection — the running
	// server keeps running, and a toggle spawns nothing (no duplicate).
	s2 := a.GetSettings()
	s2.Mode = ModeServer
	if err := a.SetSettings(s2); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should still be running after toggling back")
	}
}

func TestStartingOneRoleStopsTheOther(t *testing.T) {
	a, _ := newTestApp(t)
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}

	// Saving client settings must not disturb the running server.
	s := a.GetSettings()
	s.ClientAddr = "127.0.0.1:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}
	if !a.ServerRunning() {
		t.Fatal("server should still be running after saving client settings")
	}

	// Starting the client stops the server: never both at once.
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("starting the client must stop the server")
	}
	if !a.ClientRunning() {
		t.Fatal("client should be running")
	}
}

// Closing the GUI must NOT stop the role: it keeps running in the
// background, and a fresh GUI instance sees (and can stop) it through
// the role lock.
func TestRoleSurvivesGUICloseAndIsStoppable(t *testing.T) {
	a1, _ := newTestApp(t)
	if _, err := a1.ServerStart(); err != nil {
		t.Fatal(err)
	}

	// A second GUI (e.g. after closing and reopening the window) has no
	// child of its own but must report the background server as running.
	a2 := NewApp()
	if !a2.ServerRunning() {
		t.Fatal("fresh GUI should detect the background server via the role lock")
	}

	// Starting must adopt, never spawn a second instance.
	started, err := a2.ServerStart()
	if err != nil || !started {
		t.Fatalf("ServerStart should adopt the running instance: %v (started=%v)", err, started)
	}
	if a2.serverProc != nil {
		t.Fatal("adopting a background instance must not spawn a child")
	}

	// The fresh GUI can stop the background instance by pid.
	if err := a2.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a2.ServerRunning() {
		t.Fatal("server should be stopped")
	}
	if a1.ServerRunning() {
		t.Fatal("first GUI should also see it stopped")
	}
}

// A role started before the GUI existed (e.g. launched from dmenu) is
// discovered, reported as running, and stoppable.
func TestPreexistingBackgroundInstanceIsAdopted(t *testing.T) {
	a, _ := newTestApp(t)

	// Simulate "kvmshare-server started by hand": a fresh instance that
	// is not a child of any GUI.
	hand := NewApp()
	if _, err := hand.ServerStart(); err != nil {
		t.Fatal(err)
	}
	// Drop every reference so only the flock keeps it alive, like a
	// background process started outside the GUI.
	hand.serverProc = nil

	if !a.ServerRunning() {
		t.Fatal("GUI should report the hand-started server as running")
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a.ServerRunning() {
		t.Fatal("server should be stopped")
	}
}

// Auto-connect must never act over a running role: connecting as a
// client stops the local server (one role per machine), so "switch to
// server, click share" used to end with the fresh server killed and
// the machine reconnecting as a client a moment later. A running
// server (or client) blocks auto-connect; stopping it re-allows it.
func TestAutoConnectBlockedByRunningRole(t *testing.T) {
	a, _ := newTestApp(t)
	s := a.GetSettings()
	s.Mode = ModeClient
	s.AutoConnect = true
	s.ClientAddr = "192.168.1.86:24800"
	if err := a.SetSettings(s); err != nil {
		t.Fatal(err)
	}

	// Nothing running locally → auto-connect is allowed.
	if a.autoConnectBlocked() {
		t.Fatal("auto-connect should be allowed when nothing runs locally")
	}
	// A running client → blocked (already connected).
	if _, err := a.ClientStart(); err != nil {
		t.Fatal(err)
	}
	if !a.autoConnectBlocked() {
		t.Fatal("auto-connect must not run over a running client")
	}
	if err := a.ClientStop(); err != nil {
		t.Fatal(err)
	}
	if a.autoConnectBlocked() {
		t.Fatal("auto-connect should be allowed again after the client stops")
	}

	// A running server (explicitly shared) → blocked: auto-connecting
	// would stop the server and reconnect this machine as a client.
	if _, err := a.ServerStart(); err != nil {
		t.Fatal(err)
	}
	if !a.autoConnectBlocked() {
		t.Fatal("auto-connect must not run over a running server")
	}
	if err := a.ServerStop(); err != nil {
		t.Fatal(err)
	}
	if a.autoConnectBlocked() {
		t.Fatal("auto-connect should be allowed again after the server stops")
	}

	// Auto-connect only applies in client mode with the flag on.
	s2 := a.GetSettings()
	s2.Mode = ModeServer
	if err := a.SetSettings(s2); err != nil {
		t.Fatal(err)
	}
	if !a.autoConnectBlocked() {
		t.Fatal("auto-connect must never run in server mode")
	}
}
