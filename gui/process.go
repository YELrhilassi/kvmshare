package main

// process.go — child-process plumbing. A kvmshare role runs in the
// background, independent of the GUI: closing the window leaves it
// running, and only one instance per role may exist (enforced by flock
// locks taken by the Rust binaries — see rolelock.go). This file holds
// the generic machinery shared by both roles: manifest verification,
// the reaped child handle, and the spawn. The lifecycle decisions
// (when to start/stop/adopt, auto-restart policy) live in roles.go.

import (
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"kvmshare/gui/internal/selfupdate"
)

// verifyBinary refuses to spawn a binary that is not part of a
// consistent install (see selfupdate/manifest.go): the dir the binary
// lives in must carry a manifest and the binary must match it. A
// manifest-less dir (old install) fails closed with a reinstall
// message — the mixed-version cursor bugs this prevents are far more
// expensive than one forced reinstall. KVMSHARE_SKIP_MANIFEST=1 is the
// dev escape hatch for hand-built binaries (make install still writes
// a manifest; a bare cargo build + copy does not).
func (a *App) verifyBinary(path string) error {
	if os.Getenv("KVMSHARE_SKIP_MANIFEST") == "1" {
		return nil
	}
	dir := filepath.Dir(path)
	if err := selfupdate.VerifyBinaries(dir); err != nil {
		if errors.Is(err, selfupdate.ErrNoManifest) {
			return fmt.Errorf("%s has no %s — reinstall kvmshare (kvmshare-install --local or make install) so all binaries are from one build", dir, selfupdate.ManifestName)
		}
		return err
	}
	return nil
}

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

// spawn starts `bin` logging stdout+stderr to logPath, in its own process
// group, with a reaper attached. `extraEnv` adds KEY=VALUE entries on top
// of the inherited environment (role-specific policy the child cannot
// read for itself — see the client's revoked-servers list). The child is
// NOT tied to the GUI's life: closing the GUI leaves it running in the
// background (flock keeps it unique).
func (a *App) spawn(bin, logPath string, extraEnv []string, args ...string) (*proc, error) {
	// O_TRUNC: each role start begins a fresh log, so an "exited
	// immediately" message can never echo a stale line from a previous
	// run (which made real failures look like old ones).
	log, err := os.OpenFile(logPath, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0o644)
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
	cmd.Env = append(cmd.Env, extraEnv...)
	cmd.SysProcAttr = processGroupAttrs()
	if err := cmd.Start(); err != nil {
		log.Close()
		return nil, fmt.Errorf("start %s: %w", filepath.Base(bin), err)
	}
	// The child inherited its own handle to the file as stdout/stderr;
	// our copy must be closed or the file stays locked by the GUI for
	// the whole session (each spawn would leak a handle and block other
	// readers — e.g. tools reading server.log).
	log.Close()
	p := &proc{cmd: cmd, done: make(chan struct{}), started: time.Now()}
	go func() {
		_ = cmd.Wait() // reap; closes done when the process is truly gone
		close(p.done)
	}()
	return p, nil
}
