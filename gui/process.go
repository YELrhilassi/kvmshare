package main

import (
	"fmt"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// A kvmshare role runs in the background, independent of the GUI: closing
// the window leaves it running, and only one instance per role may exist
// (enforced by flock locks taken by the Rust binaries). This file is the
// GUI's side of that contract:
//
//   - Running  = our own child is up, OR someone holds the role lock
//     (started by an earlier GUI, or by hand). Detected via the lock.
//   - Start    = never spawn a second instance: if the role already runs
//     in the background, adopt it.
//   - Stop     = kill our child if any, else signal the pid recorded in
//     the lock file, and wait for the lock to clear.
//   - Starting one role stops the other (a machine is one or the other).

// proc wraps a managed child process with a reaper goroutine.
//
// The reaper calls Wait and closes `done`, so running() is accurate the
// moment the child dies (crash, role-lock refusal, kill) instead of only
// after an explicit stop — no ghost "running" states. Stopping is a
// single, non-leaking operation: stop() may be called any number of times.
type proc struct {
	cmd  *exec.Cmd
	done chan struct{}
	// When the child was spawned; the auto-restart watcher uses it to
	// reset its consecutive-restart budget after a long-lived run.
	started time.Time
}

func (p *proc) running() bool {
	if p == nil || p.cmd == nil || p.cmd.Process == nil {
		return false
	}
	select {
	case <-p.done:
		return false
	default:
		return true
	}
}

// stop terminates the process group (SIGTERM, then SIGKILL after 3s) and
// waits for the reaper. Safe to call more than once or on a dead proc.
func (p *proc) stop() {
	if p == nil || p.cmd == nil || p.cmd.Process == nil {
		return
	}
	select {
	case <-p.done:
		return // already gone
	default:
	}
	// Signal the whole process group (on Windows: terminate the process).
	_ = signalGroup(p.cmd.Process.Pid, signalTerm)
	select {
	case <-p.done:
	case <-time.After(3 * time.Second):
		_ = signalGroup(p.cmd.Process.Pid, signalKill)
		<-p.done
	}
}

func stopProc(p *proc) *proc {
	p.stop()
	return nil
}

// ---------------------------------------------------------------------------
// Role discovery through the lock files (which only exist while a process
// holds them — flock releases on process death, so "lock held" == running).
// ---------------------------------------------------------------------------

func (a *App) roleLockPath(role string) string {
	return filepath.Join(a.stateDir, role+".lock")
}

// rolePidPath returns where the role records its pid. Kept out of the
// lock file on purpose: on Windows the lock is a byte-range LockFileEx,
// which blocks reads of the locked range by other handles — a pid inside
// the lock file would read back as empty (pid 0) and break stop-by-pid.
func (a *App) rolePidPath(role string) string {
	return filepath.Join(a.stateDir, role+".pid")
}

// roleActive reports whether a kvmshare process of `role` is running on
// this machine — ours or not (detected by probing the role's flock).
func (a *App) roleActive(role string) bool {
	f, err := os.OpenFile(a.roleLockPath(role), os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return false
	}
	defer f.Close()
	if tryLockFile(f) == nil {
		unlockFile(f) // not held: we own it
		return false
	}
	return true // another process holds it
}

// pidFromLock returns the pid a running instance recorded for its role
// (0 when unknown). Read from the dedicated `.pid` file — the lock file
// itself cannot be read reliably on Windows (byte-range lock), and the
// pid is only consulted while the role lock is actually held, so a
// stale pid file (left by a crash) is never acted on.
func (a *App) pidFromLock(role string) int {
	raw, err := os.ReadFile(a.rolePidPath(role))
	if err != nil {
		return 0
	}
	var pid int
	if _, err := fmt.Sscanf(string(raw), "%d", &pid); err != nil || pid <= 1 {
		return 0
	}
	return pid
}

// logTail returns the last lines of a log file (for start-failure
// messages), or a generic note when the log is missing.
func logTail(path string) string {
	raw, err := os.ReadFile(path)
	if err != nil {
		return "see the log for details"
	}
	lines := strings.Split(strings.TrimRight(string(raw), "\n"), "\n")
	if len(lines) > 3 {
		lines = lines[len(lines)-3:]
	}
	out := strings.Join(lines, " | ")
	if out == "" {
		return "see the log for details"
	}
	return out
}

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

const roleServer = "server"
const roleClient = "client"

// ---------------------------------------------------------------------------
// Start (never two instances) / Stop (whoever holds the lock)
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
	return a.stopRoleLocked(roleClient)
}

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
	// The log-control file sets the level/enabled the operator chose;
	// the process polls it, so later changes apply without a restart.
	a.writeLogCtlLocked(roleServer)
	return a.spawn(a.serverPath, a.serverLogPath, "--config", a.configPath,
		"--logctl", filepath.Join(a.stateDir, roleServer+".logctl"))
}

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

// currentModeLocked returns the selected role. Callers hold a.mu.
func (a *App) currentModeLocked() Mode {
	return a.settings.Mode
}

func (a *App) ClientStart() (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()

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
	p, err := a.spawn(a.clientPath, a.clientLogPath, args...)
	if err == nil {
		err = a.checkStarted(p, a.clientLogPath, "client")
	}
	// Same conflict-retry as ServerStart: a server that reappeared in
	// the gap between our cleanup and the client's start must not turn
	// into a confusing "exited immediately" error.
	if conflictError(err) {
		if stopErr := a.stopRoleLocked(roleServer); stopErr == nil {
			p, err = a.spawn(a.clientPath, a.clientLogPath, args...)
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

// StartActive starts the process for the currently selected role.
func (a *App) StartActive() (bool, error) {
	if a.currentMode() == ModeClient {
		return a.ClientStart()
	}
	return a.ServerStart()
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

func (a *App) currentMode() Mode {
	a.mu.Lock()
	defer a.mu.Unlock()
	return a.settings.Mode
}

// spawn starts `bin` logging stdout+stderr to logPath, in its own process
// group, with a reaper attached. The child is NOT tied to the GUI's life:
// closing the GUI leaves it running in the background (flock keeps it
// unique).
func (a *App) spawn(bin, logPath string, args ...string) (*proc, error) {
	log, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, fmt.Errorf("open log: %w", err)
	}
	cmd := exec.Command(bin, args...)
	cmd.Stdout = log
	cmd.Stderr = log
	// Pin the child to our state dir. On Windows, GUI-launched processes
	// inherit no HOME, and the Rust role guard would otherwise fall back
	// to a relative dir in whatever cwd we gave it (often not writable —
	// "access is denied"). This guarantees GUI and binary always
	// coordinate on the same lock/log files regardless of the child env.
	cmd.Env = append(os.Environ(), "KVMSHARE_STATE="+a.stateDir)
	cmd.SysProcAttr = processGroupAttrs()
	if err := cmd.Start(); err != nil {
		log.Close()
		return nil, fmt.Errorf("start %s: %w", filepath.Base(bin), err)
	}
	p := &proc{cmd: cmd, done: make(chan struct{}), started: time.Now()}
	go func() {
		_ = cmd.Wait() // reap; closes done when the process is truly gone
		close(p.done)
	}()
	return p, nil
}
