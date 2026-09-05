package main

// The installer service: what the window can ask this process to do.
// All state lives in one mutex-guarded snapshot that the front end
// polls — installs run on a background goroutine (downloads can take a
// while) and the poll is how the progress bar and log lines move.

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"

	"kvmshare/gui/internal/installer"
	"kvmshare/gui/internal/selfupdate"
)

// maxLogLines keeps the status log from growing without bound.
const maxLogLines = 200

// Snapshot is the whole installer UI state, serialized for the front end.
type Snapshot struct {
	Latest    string   // latest published version ("vX.Y.Z"), "" if unknown
	Installed bool     // binaries are present in the install dir
	Busy      bool     // an install/uninstall is running
	Phase     string   // label of the current step
	Progress  float64  // 0..1
	Log       []string // progress lines, newest last
	Error     string   // last failure (cleared when the next action starts)
	Done      bool     // the last action finished successfully
}

// Installer is the Wails-bound service.
type Installer struct {
	mu      sync.Mutex
	latest  string
	busy    bool
	phase   string
	progress float64
	log     []string
	err     string
	done    bool
}

// NewInstaller builds the service and primes the latest-version check.
func NewInstaller() *Installer {
	inst := &Installer{}
	go inst.checkLatest()
	return inst
}

// Snapshot returns the current UI state (the front end polls it).
func (in *Installer) Snapshot() Snapshot {
	in.mu.Lock()
	defer in.mu.Unlock()
	return Snapshot{
		Latest:    in.latest,
		Installed: installPresent(),
		Busy:      in.busy,
		Phase:     in.phase,
		Progress:  in.progress,
		Log:       append([]string(nil), in.log...),
		Error:     in.err,
		Done:      in.done,
	}
}

// InstallLatest fetches and applies the latest release.
func (in *Installer) InstallLatest() {
	go in.run("Install", func() error {
		return installer.Install(installer.Options{
			Upstream: os.Getenv("KVMSHARE_UPSTREAM"),
			Log:      in.logf,
			Phase:    in.phasef,
		})
	})
}

// InstallVersion fetches and applies a pinned release tag.
func (in *Installer) InstallVersion(tag string) {
	go in.run("Install "+tag, func() error {
		return installer.Install(installer.Options{
			Tag:      tag,
			Upstream: os.Getenv("KVMSHARE_UPSTREAM"),
			Log:      in.logf,
			Phase:    in.phasef,
		})
	})
}

// Uninstall removes the installed binaries and desktop integration.
func (in *Installer) Uninstall() {
	go in.run("Uninstall", func() error {
		return installer.Uninstall(in.logf)
	})
}

// Launch starts the installed GUI, detached from this process.
func (in *Installer) Launch() {
	in.mu.Lock()
	defer in.mu.Unlock()
	if err := installer.Launch(selfupdate.InstallDir()); err != nil {
		in.err = err.Error()
		in.logf("launch failed: %v", err)
		return
	}
	in.logf("launched kvmshare-gui")
}

// AppVersion is the version of this installer build.
func (in *Installer) AppVersion() string {
	if selfupdate.Version == "" {
		return "dev"
	}
	return selfupdate.Version
}

// InstallDir reports where binaries will be (or were) installed.
func (in *Installer) InstallDir() string {
	return selfupdate.InstallDir()
}

// Platform is a short human label for the OS (shown in the footer).
func (in *Installer) Platform() string {
	switch runtime.GOOS {
	case "windows":
		return "Windows"
	case "linux":
		return "Linux"
	default:
		return runtime.GOOS
	}
}

// checkLatest resolves the latest published release in the background.
func (in *Installer) checkLatest() {
	rel, err := installer.Check(os.Getenv("KVMSHARE_UPSTREAM"))
	in.mu.Lock()
	defer in.mu.Unlock()
	if err != nil {
		in.err = "check for updates failed: " + err.Error()
		return
	}
	in.latest = rel.Tag
}

// run executes fn as the current action, wrapping it in the busy/error/
// done state machine.
func (in *Installer) run(label string, fn func() error) {
	in.mu.Lock()
	if in.busy {
		in.mu.Unlock()
		return
	}
	in.busy, in.phase, in.progress, in.err, in.done = true, label, 0.0, "", false
	in.mu.Unlock()

	err := fn()

	in.mu.Lock()
	defer in.mu.Unlock()
	in.busy = false
	if err != nil {
		in.err = err.Error()
		in.logf("failed: %v", err)
		return
	}
	in.done = true
	in.progress = 1.0
	// Desktop integration (shortcuts, input access) after a successful
	// install. Best-effort, reported through the log.
	dir := selfupdate.InstallDir()
	if err := installer.Integrate(dir, in.logf); err != nil {
		in.logf("desktop integration incomplete: %v", err)
	}
	in.phase = "Done"
	in.logf("kvmshare installed in %s", dir)
}

func (in *Installer) logf(format string, args ...any) {
	in.mu.Lock()
	defer in.mu.Unlock()
	line := fmt.Sprintf(format, args...)
	in.log = append(in.log, line)
	if len(in.log) > maxLogLines {
		in.log = in.log[len(in.log)-maxLogLines:]
	}
}

func (in *Installer) phasef(label string, progress float64) {
	in.mu.Lock()
	defer in.mu.Unlock()
	in.phase = label
	in.progress = progress
}

// installPresent reports whether the platform binaries are in place.
func installPresent() bool {
	dir := selfupdate.InstallDir()
	for _, bin := range selfupdate.Binaries() {
		if _, err := os.Stat(filepath.Join(dir, bin)); err != nil {
			return false
		}
	}
	return true
}