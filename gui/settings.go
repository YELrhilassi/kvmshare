package main

// settings.go — the GUI's own persisted state (gui.json): the selected
// role, the client's connection target and name, the operator's logging
// preferences, and the discovery/pairing trust settings. Distinct from
// the server's layout config (config.go), which belongs to the server.

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// Mode is what this machine does in the KVM.
type Mode string

const (
	ModeServer Mode = "server"
	ModeClient Mode = "client"
)

// Settings is the GUI's own persisted state (role + client connection +
// the operator's logging preferences + discovery/trust settings).
type Settings struct {
	Mode       Mode   `json:"mode"`
	ClientAddr string `json:"clientAddr"` // host:port of the server to connect to
	ClientName string `json:"clientName"` // screen name used on the server
	LogLevel   string `json:"logLevel"`   // error|warn|info|debug|trace
	LogEnabled bool   `json:"logEnabled"` // false silences the role's log entirely

	// TrustedServers is the list of server machine ids this machine
	// accepts connection requests from (discovery pairing). Empty means
	// "no server may command this machine to connect".
	TrustedServers []string `json:"trustedServers"`
	// RevokedServers is the list of server machine ids the operator
	// explicitly refused. Revocation is sticky: it survives a later
	// pairing request and, crucially, it overrides the last-used-address
	// fallback auto-connect uses. Without it, "revoke" only removed the
	// id from TrustedServers — and auto-connect would reconnect to the
	// very server the operator had just refused, because its address
	// still matched the last connection.
	RevokedServers []string `json:"revokedServers"`
	// AcceptPairing lets any local kvmshare server request a connection
	// (a convenience with a trust trade-off; off by default).
	AcceptPairing bool `json:"acceptPairing"`
	// AutoConnect makes the client automatically connect to the last
	// used server when it appears on the network (discovery).
	AutoConnect bool `json:"autoConnect"`
	// AutoConnectPaused is set whenever a client session ends *by
	// request* — the operator pressed Stop, or the server sent the
	// disconnect command — and cleared by an explicit Start/Connect. It
	// is what stops auto-connect from immediately undoing an operator's
	// "stop": without it, pressing Stop with auto-connect on restarted
	// the client a second later. Persisted so a GUI restart does not
	// silently resume a session the operator ended.
	AutoConnectPaused bool `json:"autoConnectPaused"`
}

// LogSettings is what the Logs page shows and edits: the logging
// configuration for this machine's instance (one role runs at a time, so
// one level applies to whichever role is active).
type LogSettings struct {
	Role    string `json:"role"` // the active role: "server" or "client"
	Level   string `json:"level"`
	Enabled bool   `json:"enabled"`
}

// loadSettings reads gui.json, filling defaults for anything an older
// GUI (or a hand edit) left out.
func (a *App) loadSettings() {
	raw, err := os.ReadFile(a.settingsPath)
	if err != nil {
		return
	}
	// Presence check: a pre-update gui.json has no log fields, and the
	// zero value for a bool is *disabled* — so distinguish "absent"
	// (default logging ON) from an explicit `logEnabled: false`.
	var present map[string]json.RawMessage
	_ = json.Unmarshal(raw, &present)
	var s Settings
	if json.Unmarshal(raw, &s) != nil {
		return
	}
	if s.Mode != ModeServer && s.Mode != ModeClient {
		s.Mode = ModeServer
	}
	if s.ClientName == "" {
		s.ClientName = a.defaultClientName()
	}
	if !validLogLevel(s.LogLevel) {
		s.LogLevel = "info"
	}
	if _, ok := present["logEnabled"]; !ok {
		s.LogEnabled = true // logging defaults to ON
	}
	// Pairing defaults to ON: trust-on-first-use means a discovered
	// server's request is accepted once and remembered, so "connect
	// here" works out of the box. The toggle exists for stricter setups.
	if _, ok := present["acceptPairing"]; !ok {
		s.AcceptPairing = true
	}
	a.settings = s
}

// saveSettingsLocked persists the settings. Callers hold a.mu.
func (a *App) saveSettingsLocked() {
	raw, err := json.MarshalIndent(a.settings, "", "  ")
	if err != nil {
		return
	}
	_ = os.WriteFile(a.settingsPath, raw, 0o644)
}

// GetSettings returns the persisted GUI state.
func (a *App) GetSettings() Settings {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings
}

// ConnectToServer points the client at `addr` (host:port) and starts it.
// Used by discovery pairing: a trusted server asked this machine to
// connect. Idempotent when a client is already running — an active
// session is never disturbed, and its saved address is left alone.
func (a *App) ConnectToServer(addr string) error {
	a.mu.Lock()
	if a.clientProc.running() || a.roleActive(roleClient) {
		a.mu.Unlock()
		return nil // already connected: don't clobber the live session
	}
	a.settings.ClientAddr = addr
	a.saveSettingsLocked()
	a.mu.Unlock()
	_, err := a.ClientStart()
	return err
}

// SetSettings stores the GUI state. The client address may stay empty
// until the client is actually started (start validates it).
func (a *App) SetSettings(s Settings) error {
	a.mu.Lock()
	if s.Mode != ModeServer && s.Mode != ModeClient {
		a.mu.Unlock()
		return fmt.Errorf("mode must be 'server' or 'client'")
	}
	if s.LogLevel == "" {
		s.LogLevel = "info" // omitted (e.g. older callers) → default
	}
	if !validLogLevel(s.LogLevel) {
		a.mu.Unlock()
		return fmt.Errorf("unknown log level %q (use error, warn, info, debug or trace)", s.LogLevel)
	}
	// The revocation list and the auto-connect pause are managed by the
	// trust and lifecycle paths, not by the settings form — the frontend
	// only renders the fields it knows, so a later `{...settings}` write
	// would otherwise silently blank them. Enabling auto-connect is the
	// one settings change that re-arms it (a deliberate "turn it back
	// on" supersedes whatever ended the last session).
	s.RevokedServers = a.settings.RevokedServers
	s.AutoConnectPaused = a.settings.AutoConnectPaused
	if s.AutoConnect && !a.settings.AutoConnect {
		s.AutoConnectPaused = false
	}
	a.settings = s
	a.saveSettingsLocked()
	// The level/enabled the user picked must apply to the running
	// instance (hot reload) and to whichever role starts next.
	a.writeLogCtlLocked(roleServer)
	a.writeLogCtlLocked(roleClient)
	a.mu.Unlock()

	// Everything below is done WITHOUT a.mu held: re-publishing the
	// network advertisement reads the settings again (it takes the lock
	// itself), and the input-access grant runs in the background.
	// Holding the lock across these used to deadlock — SetSettings is
	// the one path that reaches the advertisement from inside a.mu.
	// A role switch changes what this machine advertises on the network
	// (server vs client) — re-publish so nearby machines see the truth.
	a.ReAdvertise()
	// Changing mode is a *selection*, not a command to stop anything:
	// the role currently running on this machine keeps running until the
	// user starts the other one. Starting a role stops the opposite role
	// first (ServerStart / ClientStart) — cleanup belongs at the moment
	// it matters, so toggling never silently kills a working session.
	if s.Mode == ModeServer {
		// Becoming the server means input isolation applies when it
		// starts: make sure the system grant exists (silent once
		// granted, and it never prompts when access already works).
		a.ensureInputAccess()
	}
	return nil
}

// GetLogSettings returns the operator's logging configuration plus the
// role it applies to (the active role — one instance per machine).
func (a *App) GetLogSettings() LogSettings {
	a.mu.Lock()
	defer a.mu.Unlock()
	return LogSettings{
		Role:    string(a.settings.Mode),
		Level:   a.settings.LogLevel,
		Enabled: a.settings.LogEnabled,
	}
}

// SetLogSettings stores the level/enabled choice and applies it live:
// the control files are re-written, and the running role process picks
// the change up within a poll interval — no restart. The inactive role's
// control file is written too, so the setting holds whichever role
// starts next (a machine is one role at a time).
func (a *App) SetLogSettings(s LogSettings) error {
	a.mu.Lock()
	defer a.mu.Unlock()
	if !validLogLevel(s.Level) {
		return fmt.Errorf("unknown log level %q (use error, warn, info, debug or trace)", s.Level)
	}
	a.settings.LogLevel = s.Level
	a.settings.LogEnabled = s.Enabled
	a.saveSettingsLocked()
	a.writeLogCtlLocked(roleServer)
	a.writeLogCtlLocked(roleClient)
	return nil
}

func validLogLevel(l string) bool {
	switch l {
	case "error", "warn", "info", "debug", "trace":
		return true
	}
	return false
}

// writeLogCtlLocked writes the role's log-control file, which the Rust
// process polls and hot-applies (level + enabled). Written atomically
// (tmp + rename) so a crash never leaves a torn file. Callers hold a.mu.
func (a *App) writeLogCtlLocked(role string) {
	content := fmt.Sprintf("level=%s\nenabled=%d\n", a.settings.LogLevel, boolInt(a.settings.LogEnabled))
	path := filepath.Join(a.stateDir, role+".logctl")
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, []byte(content), 0o644); err != nil {
		return
	}
	_ = os.Rename(tmp, path)
}

func boolInt(b bool) int {
	if b {
		return 1
	}
	return 0
}

// ClearLog empties the given role's log file ("server" or "client"). The
// role process keeps appending from the new offset — appends are atomic,
// so a running process cannot resurrect cleared lines.
func (a *App) ClearLog(role string) error {
	a.mu.Lock()
	var path string
	switch role {
	case roleServer:
		path = a.serverLogPath
	case roleClient:
		path = a.clientLogPath
	default:
		a.mu.Unlock()
		return fmt.Errorf("unknown role %q", role)
	}
	a.mu.Unlock()
	if err := os.Truncate(path, 0); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}
