package main

import (
	"fmt"
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

// pidFromLock returns the pid a running instance recorded in its lock
// file (0 when unknown).
func (a *App) pidFromLock(role string) int {
	raw, err := os.ReadFile(a.roleLockPath(role))
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

	if _, err := os.Stat(a.serverPath); err != nil {
		return false, fmt.Errorf("server binary not found at %s (run make install)", a.serverPath)
	}
	// The log-control file sets the level/enabled the operator chose;
	// the process polls it, so later changes apply without a restart.
	a.writeLogCtlLocked(roleServer)
	p, err := a.spawn(a.serverPath, a.serverLogPath, "--config", a.configPath,
		"--logctl", filepath.Join(a.stateDir, roleServer+".logctl"))
	if err != nil {
		return false, err
	}
	if err := a.checkStarted(p, a.serverLogPath, "server"); err != nil {
		return false, err
	}
	a.serverProc = p
	return true, nil
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
	if err != nil {
		return false, err
	}
	if err := a.checkStarted(p, a.clientLogPath, "client"); err != nil {
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
	p := &proc{cmd: cmd, done: make(chan struct{})}
	go func() {
		_ = cmd.Wait() // reap; closes done when the process is truly gone
		close(p.done)
	}()
	return p, nil
}
