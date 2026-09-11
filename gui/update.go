// In-place updates from the GUI. The bound methods let the frontend
// check GitHub for a newer release and apply it without leaving the app:
// the new binaries replace the current ones (rename-based, safe while
// running) and the GUI restarts into the new version. Roles are separate
// processes, so a running server/client keeps running the old code until
// its next start — nothing is interrupted.
//
// The restart is a deliberate hand-off: the old process spawns the new
// binary with KVMSHARE_RESTART=1, verifies the child actually started,
// and then quits — releasing the instance lock so the child can take it
// over (see App.restartHandoff). The alternative — keeping the old
// process alive — would strand the update behind the single-instance
// lock forever.
package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

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
// A release with no archive for this platform is reported as an error —
// an \"update available\" that cannot be installed is a lie. Dev builds
// (unreleased, e.g. the default v0.0.0-dev) are simply reported as
// having no update: they have no upstream to compare against, and a
// confusing failure would be worse than no claim.
func (a *App) CheckForUpdate() UpdateInfo {
	info := UpdateInfo{Current: selfupdate.Version}
	if !selfupdate.IsRelease(selfupdate.Version) {
		return info
	}
	rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
	if err != nil {
		info.Error = err.Error()
		return info
	}
	if _, err := rel.AssetFor(); err != nil {
		info.Error = err.Error()
		return info
	}
	info.Version = rel.Tag
	info.Available = selfupdate.Newer(rel.Tag, selfupdate.Version)
	return info
}

// ApplyUpdate downloads and installs the newest release in place, then
// hands off to the new GUI. Returns before the restart (the frontend
// shows a \"restarting\" state; this process exits shortly after).
func (a *App) ApplyUpdate() UpdateResult {
	// Dev builds never apply an update: there is no released archive for
	// them, and replacing a hand-built binary with a release is not what
	// self-update is for.
	if !selfupdate.IsRelease(selfupdate.Version) {
		return UpdateResult{Error: fmt.Sprintf("this is a development build (%s) — update it by reinstalling", selfupdate.Version)}
	}
	rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
	if err != nil {
		return UpdateResult{Error: err.Error()}
	}
	if !selfupdate.Newer(rel.Tag, selfupdate.Version) {
		return UpdateResult{Error: fmt.Sprintf("already on the latest version (%s)", selfupdate.Version)}
	}
	// The running binary's real path (may be a symlink on PATH). Read
	// BEFORE any replacement: on Windows the running image's file gets
	// renamed aside, and querying the path afterwards would return the
	// renamed (.old) location — i.e. the old binary.
	exe, err := os.Executable()
	if err != nil {
		return UpdateResult{Error: fmt.Sprintf("locate self: %v", err)}
	}
	exe, _ = filepath.EvalSymlinks(exe)

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
	if err := selfupdate.Download(asset.URL, archive, nil); err != nil {
		clean()
		return UpdateResult{Error: fmt.Sprintf("download: %v", err)}
	}
	// The checksum is mandatory, not best-effort: an archive that fails
	// verification must never be installed. A missing SHA256SUMS in the
	// release is a release defect and refuses the update outright.
	sums, err := selfupdate.FetchChecksums(rel)
	if err != nil {
		clean()
		return UpdateResult{Error: err.Error()}
	}
	expected, ok := sums[asset.Name]
	if !ok {
		clean()
		return UpdateResult{Error: fmt.Sprintf("release has no checksum for %s", asset.Name)}
	}
	if err := selfupdate.VerifyFile(archive, expected); err != nil {
		clean()
		return UpdateResult{Error: err.Error()}
	}

	extracted, err := selfupdate.Extract(archive, dir)
	if err != nil {
		clean()
		return UpdateResult{Error: err.Error()}
	}
	// Everything the release must contain, mapped to where it lives on
	// this machine. The binary names carry the platform extension
	// (kvmshare-gui.exe on Windows), so match against the extracted set
	// exactly — a mismatch here used to reject every Windows release as
	// "missing binaries". Replacing is one transaction (ReplaceSet): a
	// failure anywhere restores every already-replaced file, so the
	// machine is never left with a mix of versions.
	dest := map[string]string{
		"kvmshare-gui":     exe,
		"kvmshare-server":  a.serverPath,
		"kvmshare-client":  a.clientPath,
		"kvmshare-install": a.installPath,
	}
	var missingBins []string
	for _, bin := range selfupdate.Binaries() {
		if _, ok := extracted[bin]; !ok {
			missingBins = append(missingBins, bin)
		}
	}
	if len(missingBins) > 0 {
		sort.Strings(missingBins)
		clean()
		return UpdateResult{Error: fmt.Sprintf("the release archive is missing: %s", strings.Join(missingBins, ", "))}
	}
	installTargets := make(map[string]string, len(dest))
	for _, bin := range selfupdate.Binaries() {
		base := strings.TrimSuffix(bin, ".exe")
		installTargets[dest[base]] = extracted[bin]
	}
	if err := selfupdate.ReplaceSet(installTargets); err != nil {
		clean()
		return UpdateResult{Error: fmt.Sprintf("install: %v", err)}
	}
	clean()

	// Hand off: spawn the freshly-replaced binary detached and let it
	// take over, then quit. The child is started with KVMSHARE_RESTART=1
	// so it waits for our instance lock instead of raising us.
	child, err := startRestart(exe)
	if err != nil {
		return UpdateResult{Error: fmt.Sprintf("start updated app: %v", err)}
	}
	// Give the child a moment to prove it started. A broken update must
	// not strand the user with nothing running — if the child dies
	// immediately, keep the current (old) process and report.
	if !waitStarted(child, 2*time.Second) {
		return UpdateResult{Error: "the updated app failed to start — keeping the current version"}
	}
	// The child is up and waiting for the lock; exit so it can take it.
	// Deferred so the frontend's \"restarting\" response goes out first.
	go func() {
		time.Sleep(150 * time.Millisecond)
		os.Exit(0)
	}()
	return UpdateResult{Restarting: true}
}

// startRestart spawns `exe` (the freshly replaced binary) in its own
// session, marked as a restart hand-off, with a reaper attached.
func startRestart(exe string) (*exec.Cmd, error) {
	cmd := exec.Command(exe)
	cmd.Stdout = os.Stdout
	cmd.Stderr = os.Stderr
	cmd.Env = append(os.Environ(), "KVMSHARE_RESTART=1")
	cmd.SysProcAttr = restartAttrs()
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	return cmd, nil
}

// waitStarted reports whether the child is still running after `d`. It
// reaps the child; a later exit is observed by the goroutine and
// discarded. Returns false when the child exited (cleanly or not) within
// the window.
func waitStarted(cmd *exec.Cmd, d time.Duration) bool {
	done := make(chan struct{})
	go func() {
		_ = cmd.Wait()
		close(done)
	}()
	select {
	case <-done:
		return false
	case <-time.After(d):
		return true
	}
}
