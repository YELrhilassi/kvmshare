package main

// The control-ownership signal: are this machine's physical input
// devices currently being driven from another machine?
//
// Both roles answer through one file, `<state_dir>/control.state`,
// written by the role process on every crossing (server: on
// SwitchTo/SwitchToLocal; client: on Enter/Leave):
//
//   server, control away:  away=1
//   client, controlled:    controlled=1
//   otherwise:             the file does not exist
//
// A missing file is the resting state, so the file is written once per
// transition — zero steady-state cost. Like every state file it is
// written atomically (tmp + rename) and removed on role exit, so a
// leftover can never outlive the process that wrote it (the GUI also
// reconciles against the role process below, the same way
// `client.state` is reconciled against the client process).

import (
	"os"
	"path/filepath"
	"strings"
)

// controlAway reads `<state_dir>/control.state` and reports whether it
// currently marks this machine's input as driven from afar. The file
// is meaningful only while the matching role process is alive, so the
// caller passes that liveness in.
func controlAway(stateDir string, roleRunning bool) bool {
	if !roleRunning {
		return false
	}
	raw, err := os.ReadFile(filepath.Join(stateDir, "control.state"))
	if err != nil {
		return false
	}
	for _, line := range strings.Split(string(raw), "\n") {
		kv := strings.SplitN(strings.TrimSpace(line), "=", 2)
		if len(kv) == 2 && kv[0] == "away" && kv[1] == "1" {
			return true
		}
		if len(kv) == 2 && kv[0] == "controlled" && kv[1] == "1" {
			return true
		}
	}
	return false
}

// controlAwayActive is the App-method shape the snapshot uses.
func (a *App) controlAwayActive() bool {
	a.mu.Lock()
	server := a.serverProc.running() || a.roleActive(roleServer)
	client := a.clientProc.running() || a.roleActive(roleClient)
	a.mu.Unlock()
	return controlAway(a.stateDir, server || client)
}
