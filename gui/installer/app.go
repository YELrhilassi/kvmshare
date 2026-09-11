package main

// The installer service: what the window can ask this process to do.
// All state lives in one mutex-guarded snapshot pushed to the front end
// as a Wails event on every change — installs run on a background
// goroutine (downloads can take a while) and each phase/progress/log
// update arrives immediately.
//
// Why push instead of the front end polling: WebView2 throttles page
// timers of an unfocused or occluded window, so a polling page freezes
// at its last rendered frame — the infamous "Installing 70%" that had
// actually finished on disk. Events ride the IPC channel on every
// change; the page keeps a slow poll only as a belt-and-braces fallback
// for state set before it subscribed.
//
// Locking discipline: every exported/bound method takes in.mu briefly,
// updates state, and pushes one event before releasing. No method calls
// another locking method while holding the lock (the old Launch held
// the mutex across logf and self-deadlocked — one click and the page
// froze forever).

import (
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sync"
	"time"

	"kvmshare/gui/internal/installer"
	"kvmshare/gui/internal/selfupdate"

	"github.com/wailsapp/wails/v3/pkg/application"
)

// stateEventName is the event the front end subscribes to for snapshots.
const stateEventName = "installer:state"

// maxLogLines keeps the status log from growing without bound.
const maxLogLines = 200

// progressEmitInterval throttles download-progress emissions: byte
// callbacks can fire thousands of times a second and each would push an
// event over IPC. Four updates a second is smoother than any human eye
// needs. Phase changes always emit immediately.
const progressEmitInterval = 250 * time.Millisecond

// Snapshot is the whole installer UI state, serialized for the front end.
type Snapshot struct {
	Latest    string   `json:"latest"`
	Installed bool     `json:"installed"`
	Busy      bool     `json:"busy"`
	Phase     string   `json:"phase"`
	Progress  float64  `json:"progress"`
	Log       []string `json:"log"`
	Error     string   `json:"error,omitempty"`
	Done      bool     `json:"done"`
}

// Installer is the Wails-bound service.
type Installer struct {
	mu       sync.Mutex
	latest   string
	busy     bool
	phase    string
	progress float64
	log      []string
	err      string
	done     bool

	events   *application.EventManager
	emitter  func(Snapshot) // set with events; lets tests observe emissions
	lastEmit time.Time      // last emission of any kind
	pending  bool           // a progress tick was throttled out and not yet sent
}

// emitterFunc returns the push function to use: the Wails event manager
// in production, or the injected sink in tests (nil events = no push).
func (in *Installer) emitterFunc() func(Snapshot) {
	if in.events != nil {
		return func(s Snapshot) { in.events.Emit(stateEventName, s) }
	}
	return in.emitter
}

// NewInstaller builds the service and primes the latest-version check.
func NewInstaller() *Installer {
	inst := &Installer{}
	go inst.checkLatest()
	return inst
}

// attachEvents hands the service the Wails event manager so state
// changes reach the frontend without the page polling. Called from
// main.go once the application is built (before then, emission is a
// no-op).
func (in *Installer) attachEvents(em *application.EventManager) {
	in.mu.Lock()
	defer in.mu.Unlock()
	in.events = em
	// Seed the page: it may subscribe after an earlier state change
	// (e.g. the latest-version check) already happened.
	in.emitLocked()
}

// snapshotLocked copies the current state. Callers must hold in.mu.
func (in *Installer) snapshotLocked() Snapshot {
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

// emitLocked pushes the current snapshot as an event. Callers must hold
// in.mu. Throttled callers (progress ticks) go through emitProgress.
func (in *Installer) emitLocked() {
	in.pending = false
	if push := in.emitterFunc(); push != nil {
		push(in.snapshotLocked())
		in.lastEmit = time.Now()
	}
}

// emitProgressLocked pushes a progress tick unless one went out very
// recently; a dropped tick is remembered (pending) so the next flush
// delivers the freshest value. Callers must hold in.mu. Phase changes
// should emitLocked directly — they are rare and must not be dropped.
func (in *Installer) emitProgressLocked() {
	push := in.emitterFunc()
	if push == nil {
		return
	}
	if time.Since(in.lastEmit) < progressEmitInterval {
		in.pending = true
		return
	}
	in.pending = false
	push(in.snapshotLocked())
	in.lastEmit = time.Now()
}

// flushProgressLocked delivers a throttled-out progress tick if one is
// pending. Callers must hold in.mu. Used at phase boundaries so the
// final fraction of a download is never lost to the throttle.
func (in *Installer) flushProgressLocked() {
	if in.pending {
		in.emitLocked()
	}
}

// Snapshot returns the current UI state (the front end polls it as a
// fallback; the primary channel is the pushed event).
func (in *Installer) Snapshot() Snapshot {
	in.mu.Lock()
	defer in.mu.Unlock()
	return in.snapshotLocked()
}

// InstallLatest fetches and applies the latest release.
func (in *Installer) InstallLatest() {
	in.mu.Lock()
	busy := in.busy
	in.mu.Unlock()
	if busy {
		return
	}
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
	in.mu.Lock()
	busy := in.busy
	in.mu.Unlock()
	if busy {
		return
	}
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
	in.mu.Lock()
	busy := in.busy
	in.mu.Unlock()
	if busy {
		return
	}
	go in.run("Uninstall", func() error {
		return installer.Uninstall(in.logf)
	})
}

// Launch starts the installed GUI, detached from this process. Does its
// own locking: no lock is held across the logf calls (the old version
// locked, then called logf which locked again — a guaranteed deadlock
// that froze the whole page on the first Launch click).
func (in *Installer) Launch() {
	if err := installer.Launch(selfupdate.InstallDir()); err != nil {
		in.mu.Lock()
		in.err = err.Error()
		in.mu.Unlock()
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
	} else {
		in.latest = rel.Tag
	}
	in.emitLocked()
}

// run executes fn as the current action, wrapping it in the busy/error/
// done state machine. Every state change emits one event.
func (in *Installer) run(label string, fn func() error) {
	in.mu.Lock()
	if in.busy {
		in.mu.Unlock()
		return
	}
	in.busy, in.phase, in.progress, in.err, in.done = true, label, 0.0, "", false
	in.emitLocked()
	in.mu.Unlock()

	err := fn()

	in.mu.Lock()
	defer in.mu.Unlock()
	in.flushProgressLocked() // last download tick before the phase flips
	in.busy = false
	if err != nil {
		in.err = err.Error()
		in.mu.Unlock()
		in.logf("failed: %v", err)
		return
	}
	in.done = true
	in.progress = 1.0
	// Desktop integration (shortcuts, input access) after a successful
	// install. Best-effort, reported through the log.
	dir := selfupdate.InstallDir()
	if integErr := installer.Integrate(dir, in.logf); integErr != nil {
		in.mu.Unlock()
		in.logf("desktop integration incomplete: %v", integErr)
		in.mu.Lock()
	}
	in.phase = "Done"
	in.emitLocked()
	in.logfLocked("kvmshare installed in %s", dir)
	in.emitLocked()
}

// logf appends a progress line and pushes it. Lock-safe from anywhere:
// it takes the lock itself, so callers must not hold it (the deadlock
// that used to freeze the page came from breaking this rule).
func (in *Installer) logf(format string, args ...any) {
	in.mu.Lock()
	defer in.mu.Unlock()
	in.logfLocked(format, args...)
}

// logfLocked is logf for callers that already hold in.mu.
func (in *Installer) logfLocked(format string, args ...any) {
	line := fmt.Sprintf(format, args...)
	in.log = append(in.log, line)
	if len(in.log) > maxLogLines {
		in.log = in.log[len(in.log)-maxLogLines:]
	}
	in.emitLocked()
}

// phasef receives installer-engine phase/progress updates. Phase labels
// (Download → Verify → Install → Done) always emit immediately; raw
// download-byte ticks are throttled by emitProgressLocked.
func (in *Installer) phasef(label string, progress float64) {
	in.mu.Lock()
	defer in.mu.Unlock()
	in.phasefLocked(label, progress)
}

// phasefLocked is phasef for callers that already hold in.mu.
func (in *Installer) phasefLocked(label string, progress float64) {
	if label != in.phase {
		// A phase boundary: push the final fraction of the previous
		// phase out first, then the new label unthrottled.
		in.flushProgressLocked()
		in.phase = label
		in.progress = progress
		in.emitLocked()
		return
	}
	in.progress = progress
	in.emitProgressLocked()
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
