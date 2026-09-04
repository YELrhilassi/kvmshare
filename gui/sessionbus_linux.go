//go:build linux

package main

// Session-bus ownership.
//
// The tray icon, desktop notifications and the WebKitGTK webview all talk
// to the D-Bus session bus. When the environment provides none — no
// DBUS_SESSION_BUS_ADDRESS, no /run/user/<uid>/bus (a bare WM started
// without dbus-launch, as Void's i3 commonly is) — godbus and GDBus
// autolaunch a *fresh* private bus for every process that asks. Each
// private bus then grows its own dbus-activated stack (at-spi,
// xdg-desktop-portal, gvfsd, and a notification daemon) that never dies
// with the launching app, because the forked bus daemon is reparented to
// init. Repeated GUI launches therefore left dozens of immortal
// portal/gvfs/dbus stacks behind.
//
// ensureSessionBus runs before anything touches D-Bus and guarantees the
// env var is set: it adopts an existing session bus when one is
// reachable, and otherwise creates exactly ONE private bus under the
// state dir — reused across launches, killed when the GUI exits — so a
// fresh bus (and its stack) can never be spawned per launch again.

import (
	"fmt"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// ensureSessionBus makes sure DBUS_SESSION_BUS_ADDRESS is set and
// reachable. It returns a cleanup function that stops any private bus it
// created (no-op when an existing bus was adopted).
//
// Resolution order:
//  1. An address already in the environment (a desktop session that set
//     it) — adopt if reachable.
//  2. The elogind/systemd user bus socket /run/user/<uid>/bus.
//  3. A legacy dbus-launch session file under ~/.dbus/session-bus.
//  4. A private bus at <stateDir>/dbus.sock, created once and reused.
func ensureSessionBus(stateDir string) func() {
	noop := func() {}

	// 1. Environment already names a bus (normal desktops).
	if addr := os.Getenv("DBUS_SESSION_BUS_ADDRESS"); addr != "" && busReachable(addr) {
		return noop
	}

	// 2. elogind/systemd user bus.
	if addr := runtimeUserBus(); addr != "" {
		os.Setenv("DBUS_SESSION_BUS_ADDRESS", addr)
		return noop
	}

	// 3. A legacy dbus-launch session file that still points at a live
	// socket (an autolaunched bus from another app in this session).
	if addr := sessionFileBus(); addr != "" {
		os.Setenv("DBUS_SESSION_BUS_ADDRESS", addr)
		return noop
	}

	// 4. Own one private bus under the state dir. Reuse it when it is
	// still alive (a previous launch crashed without cleanup); create it
	// otherwise. The GUI is single-instance, so ownership is ours.
	sock := filepath.Join(stateDir, "dbus.sock")
	pidFile := filepath.Join(stateDir, "dbus.pid")

	if pid := readPidFile(pidFile); pid != 0 && processAlive(pid) && socketExists(sock) {
		// A live bus we (or a crashed launch) created: adopt it and keep
		// ownership so cleanup still happens on a clean exit.
		os.Setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path="+sock)
		return func() { stopPrivateBus(pid, sock, pidFile) }
	}

	pid, err := startPrivateBus(sock)
	if err != nil {
		// Could not create a bus (dbus-daemon missing?): leave the env
		// unset. godbus and GDBus will autolaunch, but only for this
		// process's lifetime is that a concern — log and move on.
		fmt.Fprintf(os.Stderr, "kvmshare: no session bus and could not start one: %v\n", err)
		return noop
	}
	_ = os.WriteFile(pidFile, []byte(strconv.Itoa(pid)), 0o644)
	os.Setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path="+sock)
	return func() { stopPrivateBus(pid, sock, pidFile) }
}

// runtimeUserBus returns the elogind/systemd user bus address if its
// socket exists, else "".
func runtimeUserBus() string {
	dir := os.Getenv("XDG_RUNTIME_DIR")
	if dir == "" {
		dir = fmt.Sprintf("/run/user/%d", os.Getuid())
	}
	sock := filepath.Join(dir, "bus")
	if !socketExists(sock) {
		return ""
	}
	return "unix:path=" + sock
}

// sessionFileBus scans the legacy dbus-launch session files for an
// address whose socket is still live. Returns "" when none is.
func sessionFileBus() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	matches, _ := filepath.Glob(filepath.Join(home, ".dbus", "session-bus", "*"))
	for _, m := range matches {
		data, err := os.ReadFile(m)
		if err != nil {
			continue
		}
		for _, line := range strings.Split(string(data), "\n") {
			line = strings.TrimSpace(line)
			if !strings.HasPrefix(line, "DBUS_SESSION_BUS_ADDRESS=") {
				continue
			}
			addr := strings.TrimPrefix(line, "DBUS_SESSION_BUS_ADDRESS=")
			addr = strings.Trim(addr, "'\"")
			if busReachable(addr) {
				return addr
			}
		}
	}
	return ""
}

// startPrivateBus launches `dbus-daemon --session` bound to a unix
// socket, forked with its pid printed to stdout (--fork --print-pid=1).
// Returns the daemon pid.
func startPrivateBus(sock string) (int, error) {
	_ = os.Remove(sock) // stale socket from a dead daemon
	addr := "unix:path=" + sock
	cmd := exec.Command("dbus-daemon", "--session", "--fork", "--print-pid=1", "--address="+addr)
	out, err := cmd.Output()
	if err != nil {
		return 0, fmt.Errorf("dbus-daemon: %w", err)
	}
	pid, err := strconv.Atoi(strings.TrimSpace(string(out)))
	if err != nil {
		return 0, fmt.Errorf("dbus-daemon printed no pid: %q", strings.TrimSpace(string(out)))
	}
	// Give the daemon a moment to bind the socket before callers dial it.
	for i := 0; i < 50; i++ {
		if socketExists(sock) {
			return pid, nil
		}
		time.Sleep(10 * time.Millisecond)
	}
	return 0, fmt.Errorf("dbus-daemon (pid %d) did not bind %s", pid, sock)
}

// stopPrivateBus terminates the private bus we started and removes its
// socket and pidfile. Killing the bus daemon takes its whole activated
// stack (portals, at-spi, gvfs, notification daemon) down with it.
func stopPrivateBus(pid int, sock, pidFile string) {
	if pid != 0 {
		// Refuse to kill a recycled pid that is no longer our daemon.
		if processAlive(pid) && processIsDBus(pid) {
			_ = syscall.Kill(pid, syscall.SIGTERM)
			// SIGTERM is graceful; escalate only if it lingers.
			for i := 0; i < 20 && processAlive(pid); i++ {
				time.Sleep(25 * time.Millisecond)
			}
			if processAlive(pid) {
				_ = syscall.Kill(pid, syscall.SIGKILL)
			}
		}
	}
	_ = os.Remove(sock)
	_ = os.Remove(pidFile)
}

// busReachable reports whether a DBUS_SESSION_BUS_ADDRESS value can be
// dialed. Only unix addresses are understood; anything else is treated
// as reachable (e.g. a systemd-launchd-style address we cannot probe),
// because refusing an env-provided bus would be worse than trusting it.
func busReachable(addr string) bool {
	if !strings.HasPrefix(addr, "unix:") {
		return true
	}
	path := ""
	switch {
	case strings.HasPrefix(addr, "unix:path="):
		path = strings.TrimPrefix(addr, "unix:path=")
	case strings.HasPrefix(addr, "unix:abstract="):
		path = "@" + strings.TrimPrefix(addr, "unix:abstract=")
	default:
		return false
	}
	if i := strings.IndexByte(path, ','); i >= 0 { // strip guid= etc.
		path = path[:i]
	}
	conn, err := net.DialTimeout("unix", path, 500*time.Millisecond)
	if err != nil {
		return false
	}
	conn.Close()
	return true
}

// socketExists reports whether a unix socket file exists.
func socketExists(path string) bool {
	fi, err := os.Stat(path)
	return err == nil && fi.Mode()&os.ModeSocket != 0
}

// readPidFile returns the pid stored in `path`, or 0.
func readPidFile(path string) int {
	data, err := os.ReadFile(path)
	if err != nil {
		return 0
	}
	pid, _ := strconv.Atoi(strings.TrimSpace(string(data)))
	return pid
}

// processAlive reports whether a pid exists (kill 0).
func processAlive(pid int) bool {
	return syscall.Kill(pid, 0) == nil
}

// processIsDBus confirms the pid is a dbus-daemon (never kill a recycled
// pid that now belongs to something else).
func processIsDBus(pid int) bool {
	exe, err := os.Readlink(fmt.Sprintf("/proc/%d/exe", pid))
	return err == nil && strings.Contains(exe, "dbus-daemon")
}
