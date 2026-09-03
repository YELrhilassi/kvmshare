// In-place updates from the GUI. The bound methods let the frontend
// check GitHub for a newer release and apply it without leaving the app:
// the new binaries replace the current ones (rename-based, safe while
// running) and the GUI restarts into the new version. Roles are separate
// processes, so a running server/client keeps running the old code until
// its next start — nothing is interrupted.
package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"kvmshare/gui/internal/selfupdate"
)

// UpdateInfo describes the outcome of an update check.
type UpdateInfo struct {
	Current   string `json:"current"`
	Available bool   `json:"available"`
	Version   string `json:"version"` // newest published, if any
	Error     string `json:"error,omitempty"`
}

// UpdateResult describes the outcome of applying an update.
type UpdateResult struct {
	Restarting bool   `json:"restarting"`
	Error      string `json:"error,omitempty"`
}

// GetVersion reports this build's version (injected at link time).
func (a *App) GetVersion() string {
	return selfupdate.Version
}

// CheckForUpdate compares this build against the latest GitHub release.
func (a *App) CheckForUpdate() UpdateInfo {
	info := UpdateInfo{Current: selfupdate.Version}
	rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
	if err != nil {
		info.Error = err.Error()
		return info
	}
	info.Version = rel.Tag
	if selfupdate.Newer(rel.Tag, selfupdate.Version) {
		info.Available = true
	}
	return info
}

// ApplyUpdate downloads and installs the newest release in place, then
// restarts this GUI into it. Returns before the restart (the frontend
// shows a "restarting" state; the process is replaced shortly after).
func (a *App) ApplyUpdate() UpdateResult {
	rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
	if err != nil {
		return UpdateResult{Error: err.Error()}
	}
	if !selfupdate.Newer(rel.Tag, selfupdate.Version) {
		return UpdateResult{Error: fmt.Sprintf("already on the latest version (%s)", selfupdate.Version)}
	}

	dir := filepath.Join(a.stateDir, "update")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return UpdateResult{Error: err.Error()}
	}
	clean := func() { _ = os.RemoveAll(dir) }

	asset, err := rel.AssetFor()
	if err != nil {
		clean()
		return UpdateResult{Error: err.Error()}
	}
	archive := filepath.Join(dir, asset.Name)
	if err := selfupdate.Download(asset.URL, archive); err != nil {
		clean()
		return UpdateResult{Error: fmt.Sprintf("download: %v", err)}
	}
	sums, err := selfupdate.FetchChecksums(rel)
	if err == nil {
		if expected, ok := sums[asset.Name]; ok {
			if err := selfupdate.VerifyFile(archive, expected); err != nil {
				clean()
				return UpdateResult{Error: err.Error()}
			}
		}
	}

	extracted, err := selfupdate.Extract(archive, dir)
	if err != nil {
		clean()
		return UpdateResult{Error: err.Error()}
	}
	// The running binary's real path (may be a symlink on PATH).
	exe, err := os.Executable()
	if err != nil {
		clean()
		return UpdateResult{Error: fmt.Sprintf("locate self: %v", err)}
	}
	exe, _ = filepath.EvalSymlinks(exe)

	targets := map[string]string{
		"kvmshare-gui":    exe,
		"kvmshare-server": a.serverPath,
		"kvmshare-client": a.clientPath,
	}
	for _, bin := range []string{"kvmshare-gui", "kvmshare-server", "kvmshare-client"} {
		src, ok := extracted[bin]
		if !ok {
			clean()
			return UpdateResult{Error: fmt.Sprintf("release is missing %s", bin)}
		}
		if err := selfupdate.ReplaceAt(src, targets[bin]); err != nil {
			clean()
			return UpdateResult{Error: fmt.Sprintf("replace %s: %v", targets[bin], err)}
		}
	}
	clean()

	// Restart into the new binary, detached, then quit this instance.
	restart(exe)
	return UpdateResult{Restarting: true}
}

// restart spawns `exe` (the freshly replaced binary) in its own session,
// so it survives this process exiting.
func restart(exe string) {
	cmd := exec.Command(exe, os.Args[1:]...)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	if runtime.GOOS != "windows" {
		cmd.SysProcAttr = processGroupAttrs()
		cmd.SysProcAttr.Setsid = true
	}
	_ = cmd.Start()
	// Let the child prove it started before we quit (a broken update
	// should not silently leave nothing running).
	go func() {
		if err := cmd.Wait(); err != nil {
			fmt.Fprintf(os.Stderr, "kvmshare: restart into new version failed: %s\n", strings.TrimSpace(err.Error()))
		}
	}()
}
