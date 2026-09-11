package main

// roles.go — the role lifecycle: start (never two instances, one role
// per machine), stop (whoever holds the lock), adopt (a background
// instance from an earlier GUI), and the bounded auto-restart that
// honors the server supervisor's restart request.

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

func (a *App) ServerRunning() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.serverProc.running() || a.roleActive(roleServer)
}

func (a *App) ClientRunning() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.clientProc.running() || a.roleActive(roleClient)
}

// ---------------------------------------------------------------------------
// Stop
// ---------------------------------------------------------------------------

// stopRoleLocked stops the role's instance whether we spawned it or not:
// our child first, then whoever still holds the lock (signalled via the
// pid recorded in the lock file). Callers hold a.mu.
//
// Returns an error when the role is still holding its lock after the
// grace period — a stop that silently did nothing would leave the other
// role refusing to start ("X is already running") with no explanation.
func (a *App) stopRoleLocked(role string) error {
	if role == roleServer {
		a.serverProc = stopProc(a.serverProc)
	} else {
		a.clientProc = stopProc(a.clientProc)
	}
	roleBin := func() string {
		if role == roleServer {
			return filepath.Base(a.serverPath)
		}
		return filepath.Base(a.clientPath)
	}()
	deadline := time.Now().Add(4 * time.Second)
	for a.roleActive(role) && time.Now().Before(deadline) {
		if pid := a.pidFromLock(role); pid > 0 {
			// Graceful first; when that is refused (typically an
			// elevated process and a non-elevated controller), fall
			// back to the platform's hard kill (SIGKILL on Unix,
			// taskkill on Windows).
			if err := signalPid(pid); err != nil {
				_ = forceKillPid(pid)
			}
		}
		// A lock file with no pid (a crash between locking and
		// writing it) leaves nothing to signal; kill by name instead.
		_ = killRoleByName(roleBin)
		time.Sleep(120 * time.Millisecond)
	}
	if a.roleActive(role) {
		return fmt.Errorf("could not stop the running %s (pid %d): it is still holding its lock", role, a.pidFromLock(role))
	}
	// The role's live-state files must not outlive the process:
	//
	//   - client: a stale "connected" file would make the Home page
	//     claim a connection that does not exist. The next start writes
	//     it fresh again.
	//   - server: `clients.json` is written by the server's event sink,
	//     which dies with the server process — a stale list would show
	//     the GUI's "connected to you" badge for machines that are no
	//     longer connected (ghost clients). A fresh server re-persists
	//     the real list on its first lifecycle event, so removing here
	//     is safe and makes the stopped state truthful immediately.
	if role == roleClient {
		_ = os.Remove(filepath.Join(a.stateDir, "client.state"))
	} else {
		// A stopped server must leave no trace of its session behind:
		// the connected-client list (written by the server's event sink,
		// which dies with the process — a stale list would show ghost
		// "connected to you" badges) and any queued client commands (the
		// server reads `server.cmd` after truncating it, so commands a
		// stopped server never consumed would replay against a fresh
		// client on the next start — disconnecting a machine nobody asked
		// to disconnect). A fresh server re-persists the real list and
		// sees an empty command file.
		_ = os.Remove(filepath.Join(a.stateDir, "clients.json"))
		_ = os.Remove(filepath.Join(a.stateDir, "server.cmd"))
	}
	return nil
}

func (a *App) ServerStop() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.stopRoleLocked(roleServer)
}

func (a *App) ClientStop() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	// An operator stop is a decision, not a transient failure: hold
	// auto-connect off until they explicitly start again, or the watcher
	// would restart the client a tick later and the Stop would look
	// broken.
	a.pauseAutoConnectLocked()
	return a.stopRoleLocked(roleClient)
}

// StopActive stops the process for the currently selected role.
func (a *App) StopActive() error {
	if a.currentMode() == ModeClient {
		return a.ClientStop()
	}
	return a.ServerStop()
}

// StopAll stops every role process on this machine (server and client —
// a machine runs at most one, so at most one is actually running). Used
// by the tray's Quit, which must leave nothing sharing input behind:
// quitting the GUI with a role still running strands the other machine's
// cursor (and, on Windows, the elevated client's input gate).
func (a *App) StopAll() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	var first error
	if err := a.stopRoleLocked(roleServer); err != nil && first == nil {
		first = err
	}
	if err := a.stopRoleLocked(roleClient); err != nil && first == nil {
		first = err
	}
	return first
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

// The exit code the server binary uses to ask for a restart after the
// supervisor detects a wedged input path (mirrors EXIT_RESTART in
// crates/core/src/server.rs). Any other exit is a stop, a crash, or a
// role conflict — none of which warrant an automatic restart here.
const restartExitCode = 66

// How long to wait before respawning after a supervisor restart, and how
// many consecutive restarts to attempt before giving up (a machine that
// keeps wedging has a real problem the user should see, not an endless
// respawn loop).
const restartDelay = 1500 * time.Millisecond
const maxRestarts = 3

// A start that survives longer than this counts as healthy and resets
// the consecutive-restart budget.
const restartBudgetResetAfter = 2 * time.Minute

// conflictError reports whether a failed start died because the *other*
// role still held its lock (the Rust binaries refuse to run alongside
// the opposite role). The message carries the binary's own log tail, so
// the match covers both the reason line and the log path form.
func conflictError(err error) bool {
	if err == nil {
		return false
	}
	s := strings.ToLower(err.Error())
	return strings.Contains(s, "already running") || strings.Contains(s, "is locked")
}

func (a *App) ServerStart() (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

	if a.serverProc.running() || a.roleActive(roleServer) {
		return true, nil // already running (adopt the background instance)
	}
	// One role per machine: stop any client first. A client that cannot
	// be stopped (e.g. an elevated process outranking the GUI) must
	// surface as a clear error here — starting would fail anyway when
	// the server binary refuses its role lock.
	if err := a.stopRoleLocked(roleClient); err != nil {
		return false, err
	}
	p, err := a.spawnServerLocked()
	if err == nil {
		err = a.checkStarted(p, a.serverLogPath, "server")
	}
	// The stop reported the client gone, yet the fresh server still died
	// on its lock — the opposite role reappeared in the gap (a stop that
	// landed just after our last probe, a lingering second instance).
	// Clean up harder and try once more before surfacing the error.
	if conflictError(err) {
		if stopErr := a.stopRoleLocked(roleClient); stopErr == nil {
			p, err = a.spawnServerLocked()
			if err == nil {
				err = a.checkStarted(p, a.serverLogPath, "server")
			}
		}
	}
	if err != nil {
		return false, err
	}
	a.serverProc = p
	a.watchAutoRestart(roleServer, p)
	return true, nil
}

// spawnServerLocked starts the server binary with the configured args.
// Callers hold a.mu.
func (a *App) spawnServerLocked() (*proc, error) {
	if _, err := os.Stat(a.serverPath); err != nil {
		return nil, fmt.Errorf("server binary not found at %s (run make install)", a.serverPath)
	}
	if err := a.verifyBinary(a.serverPath); err != nil {
		return nil, err
	}
	// The log-control file sets the level/enabled the operator chose;
	// the process polls it, so later changes apply without a restart.
	a.writeLogCtlLocked(roleServer)
	return a.spawn(a.serverPath, a.serverLogPath, nil, "--config", a.configPath,
		"--logctl", filepath.Join(a.stateDir, roleServer+".logctl"))
}

// ClientStart starts the client against the configured server address.
// Same adopt/stop-the-other-role/conflict-retry contract as
// ServerStart. The body lives in clientStartLocked so the auto-connect
// path can decide-and-start under a single hold of a.mu — otherwise a
// check that passes can go stale before the start, and the start can
// kill a server the operator began in the gap.
func (a *App) ClientStart() (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	// An explicit start is the operator re-arming auto-connect: whatever
	// ended the last session by request (Stop, or the server's
	// disconnect), this supersedes it.
	a.clearAutoConnectPauseLocked()
	return a.clientStartLocked()
}

func (a *App) clientStartLocked() (bool, error) {
	if a.clientProc.running() || a.roleActive(roleClient) {
		return true, nil // already running (adopt the background instance)
	}
	// One role per machine: stop any server first (see ServerStart for
	// why a failed stop aborts the start).
	if err := a.stopRoleLocked(roleServer); err != nil {
		return false, err
	}

	if _, err := os.Stat(a.clientPath); err != nil {
		return false, fmt.Errorf("client binary not found at %s (run make install)", a.clientPath)
	}
	if err := a.verifyBinary(a.clientPath); err != nil {
		return false, err
	}
	addr := strings.TrimSpace(a.settings.ClientAddr)
	if addr == "" {
		return false, fmt.Errorf("set the server address first (client page)")
	}
	args := []string{addr}
	if name := strings.TrimSpace(a.settings.ClientName); name != "" {
		args = append(args, "--name", name)
	}
	a.writeLogCtlLocked(roleClient)
	args = append(args, "--logctl", filepath.Join(a.stateDir, roleClient+".logctl"))
	// Hand the client this machine's revoked-servers list: the client
	// refuses a session whose server id is on it, which covers connects
	// the GUI never screened (a hand-typed address, a reconnect).
	env := a.clientRevokedEnvLocked()
	p, err := a.spawn(a.clientPath, a.clientLogPath, env, args...)
	if err == nil {
		err = a.checkStarted(p, a.clientLogPath, "client")
	}
	// Same conflict-retry as ServerStart: a server that reappeared in
	// the gap between our cleanup and the client's start must not turn
	// into a confusing "exited immediately" error.
	if conflictError(err) {
		if stopErr := a.stopRoleLocked(roleServer); stopErr == nil {
			p, err = a.spawn(a.clientPath, a.clientLogPath, env, args...)
			if err == nil {
				err = a.checkStarted(p, a.clientLogPath, "client")
			}
		}
	}
	if err != nil {
		return false, err
	}
	a.clientProc = p
	return true, nil
}

// StartActive starts the process for the currently selected role.
func (a *App) StartActive() (bool, error) {
	if a.currentMode() == ModeClient {
		return a.ClientStart()
	}
	return a.ServerStart()
}

// checkStarted gives a freshly spawned child a moment to prove it is
// alive. A child that dies immediately (role lock refused, port taken,
// missing display) surfaces as a clear error instead of a silent no-op.
func (a *App) checkStarted(p *proc, logPath, label string) error {
	select {
	case <-p.done:
		return fmt.Errorf("%s exited immediately: %s", label, logTail(logPath))
	case <-time.After(350 * time.Millisecond):
	}
	return nil
}

// ---------------------------------------------------------------------------
// Auto-restart (server supervisor's restart request)
// ---------------------------------------------------------------------------

// watchAutoRestart respawns a role process when it exits with the
// supervisor's restart code: the server detected a wedged input path and
// asked for a clean restart (its own exit released every kernel/X grab).
// A stop, a crash or a role switch never triggers it (different exit
// codes, and the mode/proc checks below). The watcher shares nothing with
// the process beyond `p.done`, so it can never be blocked by whatever
// wedged the role. Bounded: at most [`maxRestarts`] consecutive
// respawns, then it logs and gives up.
func (a *App) watchAutoRestart(role string, p *proc) {
	go func() {
		<-p.done
		code := -1
		if p.cmd.ProcessState != nil {
			code = p.cmd.ProcessState.ExitCode()
		}
		if code != restartExitCode {
			return
		}
		restarts := 0
		for restarts < maxRestarts {
			time.Sleep(restartDelay)
			a.mu.Lock()
			// The user may have stopped the role or switched modes
			// (serverProc replaced or nil, or the mode no longer matches)
			// while we waited — then this process is no longer the one
			// being managed, and respawning would fight the user.
			relevant := a.serverProc == p && a.currentModeLocked() == ModeServer && !a.roleActive(roleServer)
			var err error
			if relevant {
				var np *proc
				np, err = a.spawnServerLocked()
				if err == nil {
					if cErr := a.checkStarted(np, a.serverLogPath, "server"); cErr != nil {
						err = cErr
					} else {
						a.serverProc = np
					}
				}
			}
			a.mu.Unlock()
			if err != nil {
				log.Printf("kvmshare: auto-restart failed for %s: %v", role, err)
				return
			}
			if !relevant {
				return
			}
			restarts++
			// The fresh process inherits the same supervision: watch it
			// the same way. A start that survives a while is healthy — it
			// resets the consecutive-restart budget, so a wedge that only
			// happens under specific conditions can never exhaust it over
			// time.
			p = a.serverProc
			select {
			case <-p.done:
				if time.Since(p.started) > restartBudgetResetAfter {
					restarts = 0
				}
				code = -1
				if p.cmd.ProcessState != nil {
					code = p.cmd.ProcessState.ExitCode()
				}
				if code != restartExitCode {
					return
				}
			}
		}
		log.Printf("kvmshare: %s asked for a restart %d times in a row — giving up; check the log for the wedge cause", role, maxRestarts)
	}()
}

func (a *App) currentModeLocked() Mode {
	return a.settings.Mode
}

func (a *App) currentMode() Mode {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings.Mode
}
