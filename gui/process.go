package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"time"
)

// ServerRunning reports whether the managed server process is up.
func (a *App) ServerRunning() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return isRunning(a.serverCmd)
}

// ClientRunning reports whether the managed client process is up.
func (a *App) ClientRunning() bool {
	a.mu.Lock()
	defer a.mu.Unlock()
	return isRunning(a.clientCmd)
}

func isRunning(cmd *exec.Cmd) bool {
	return cmd != nil && cmd.Process != nil && cmd.ProcessState == nil
}

// ServerStart launches kvmshare-server with the current config.
func (a *App) ServerStart() (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if isRunning(a.serverCmd) {
		return true, nil
	}
	if _, err := os.Stat(a.serverPath); err != nil {
		return false, fmt.Errorf("server binary not found at %s (run make install)", a.serverPath)
	}
	cmd, err := spawn(a.serverPath, a.serverLogPath, "--config", a.configPath)
	if err != nil {
		return false, err
	}
	a.serverCmd = cmd
	return true, nil
}

// ServerStop stops the managed server process.
func (a *App) ServerStop() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.serverCmd = stopAndForget(a.serverCmd)
	return nil
}

// ClientStart launches kvmshare-client against the configured server.
func (a *App) ClientStart() (bool, error) {
	a.mu.Lock()
	defer a.mu.Unlock()
	if isRunning(a.clientCmd) {
		return true, nil
	}
	if _, err := os.Stat(a.clientPath); err != nil {
		return false, fmt.Errorf("client binary not found at %s (run make install)", a.clientPath)
	}
	addr := strings.TrimSpace(a.settings.ClientAddr)
	if addr == "" {
		return false, fmt.Errorf("set the server address on the client page first")
	}
	args := []string{addr}
	if name := strings.TrimSpace(a.settings.ClientName); name != "" {
		args = append(args, "--name", name)
	}
	cmd, err := spawn(a.clientPath, a.clientLogPath, args...)
	if err != nil {
		return false, err
	}
	a.clientCmd = cmd
	return true, nil
}

// ClientStop stops the managed client process.
func (a *App) ClientStop() error {
	a.mu.Lock()
	defer a.mu.Unlock()
	a.clientCmd = stopAndForget(a.clientCmd)
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

// restartServerLocked must be called with a.mu held.
func (a *App) restartServerLocked() error {
	if isRunning(a.serverCmd) {
		a.serverCmd = stopAndForget(a.serverCmd)
	}
	cmd, err := spawn(a.serverPath, a.serverLogPath, "--config", a.configPath)
	if err != nil {
		return err
	}
	a.serverCmd = cmd
	return nil
}

// spawn starts `bin` logging stdout+stderr to logPath, in its own process
// group so it survives GUI exit and can be killed as a unit.
func spawn(bin, logPath string, args ...string) (*exec.Cmd, error) {
	log, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, fmt.Errorf("open log: %w", err)
	}
	cmd := exec.Command(bin, args...)
	cmd.Stdout = log
	cmd.Stderr = log
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}
	if err := cmd.Start(); err != nil {
		log.Close()
		return nil, fmt.Errorf("start %s: %w", filepath.Base(bin), err)
	}
	return cmd, nil
}

// stopAndForget terminates the process group and reaps the process.
func stopAndForget(cmd *exec.Cmd) *exec.Cmd {
	if cmd == nil || cmd.Process == nil {
		return nil
	}
	if cmd.ProcessState == nil {
		// Negative pid signals the whole process group.
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGTERM)
		waited := make(chan struct{})
		go func() {
			_, _ = cmd.Process.Wait() // reap; avoids a zombie
			close(waited)
		}()
		select {
		case <-waited:
		case <-time.After(3 * time.Second):
			_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
			<-waited
		}
	}
	return nil
}
