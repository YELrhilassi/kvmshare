// Package installer is the one implementation of "get kvmshare onto this
// machine". Both front ends use it — the CLI (`kvmshare-install`, for
// scripts and terminals) and the GUI installer (a Wails window with the
// same progress under the hood). Nothing else may duplicate this logic.
//
// The flow is the same everywhere:
//
//  1. resolve what to install (the latest GitHub release, a pinned tag,
//     or a local directory of built binaries),
//  2. download the platform archive and verify its checksum against the
//     release's SHA256SUMS,
//  3. extract and apply the binaries (selfupdate.Apply — atomic renames,
//     survives a running process),
//  4. integrate with the desktop (Linux: udev input access + desktop
//     entry; Windows: shortcuts + Add/Remove Programs entry).
//
// Progress is reported through the Options callbacks so each front end
// renders it in its own voice — the CLI prints lines, the GUI drives a
// progress bar.
package installer

import (
	"fmt"
	"io"
	"os"
	"path/filepath"

	"kvmshare/gui/internal/selfupdate"
)

// Options configures an install run.
type Options struct {
	// Tag pins a specific release ("" = the latest published).
	Tag string
	// Upstream is the "owner/repo" to fetch from ("" = the default).
	Upstream string
	// Log receives human-readable progress lines (download size,
	// checksum result, written paths).
	Log func(format string, args ...any)
	// Phase receives structured progress: a short label plus how far
	// the overall run has come (0..1) — the GUI drives its bar from it.
	Phase func(label string, progress float64)
}

func logf(fn func(string, ...any), format string, args ...any) {
	if fn != nil {
		fn(format, args...)
	}
}

// EnsureInputAccess grants the desktop user access to the physical input
// devices (Linux: a udev rule applied via pkexec; grant-only-if-missing so
// it is safe to call unconditionally). A no-op on other platforms.
func EnsureInputAccess() error {
	return ensureInputAccess()
}

func phasef(fn func(string, float64), label string, p float64) {
	if fn != nil {
		fn(label, p)
	}
}

// Install fetches and applies a release (latest by default, or the
// pinned Tag). Idempotent: running it again updates in place.
func Install(opts Options) error {
	up := opts.Upstream
	if up == "" {
		up = selfupdate.DefaultUpstream
	}

	rel, err := selfupdate.FetchRelease(up)
	if err != nil {
		return fmt.Errorf("fetch release: %w", err)
	}
	tag := rel.Tag
	if opts.Tag != "" {
		tag = opts.Tag
		if tag != rel.Tag {
			// Pinned tag's metadata (asset URLs) may differ from the
			// latest release: refetch by tag.
			r2, err := selfupdate.FetchReleaseTag(up, tag)
			if err != nil {
				return fmt.Errorf("fetch release %s: %w", tag, err)
			}
			rel = r2
		}
	}
	phasef(opts.Phase, "Downloading "+tag, 0.05)
	return installRelease(tag, rel, opts)
}

// Check reports the latest published version ("vX.Y.Z"), used by
// --check and by the GUI installer's header.
func Check(upstream string) (*selfupdate.Release, error) {
	if upstream == "" {
		upstream = selfupdate.DefaultUpstream
	}
	return selfupdate.FetchRelease(upstream)
}

// InstallLocal installs from a directory that already contains the
// built binaries (a dist/ folder or a manually extracted archive).
// Fully offline — used for testing and air-gapped machines.
func InstallLocal(dir string, log func(string, ...any)) error {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return err
	}
	if !st.IsDir() {
		return fmt.Errorf("%s is not a directory", abs)
	}
	tmp, err := os.MkdirTemp("", "kvmshare-local-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	// Stage as copies: Apply renames binaries into place (fast, atomic
	// on Windows) and renaming would otherwise consume the source dir.
	extracted := make(map[string]string, len(selfupdate.Binaries()))
	for _, bin := range selfupdate.Binaries() {
		src := filepath.Join(abs, bin)
		if _, err := os.Stat(src); err != nil {
			return fmt.Errorf("source dir is missing %s", bin)
		}
		staged := filepath.Join(tmp, bin)
		if err := copyFile(src, staged); err != nil {
			return fmt.Errorf("stage %s: %w", bin, err)
		}
		extracted[bin] = staged
	}
	logf(log, "installing from %s", abs)
	return applyExtracted(extracted, log, nil)
}

// Uninstall removes the installed binaries and undoes the desktop
// integration (udev rule on Linux, shortcuts and the Add/Remove entry
// on Windows).
func Uninstall(log func(string, ...any)) error {
	dir := selfupdate.InstallDir()
	removed := 0
	for _, bin := range selfupdate.Binaries() {
		p := filepath.Join(dir, bin)
		if _, err := os.Stat(p); err == nil {
			if err := os.Remove(p); err != nil {
				return fmt.Errorf("remove %s: %w", p, err)
			}
			logf(log, "removed %s", p)
			removed++
		}
	}
	if err := removeDesktopIntegration(dir); err != nil {
		logf(log, "warning cleaning desktop integration: %v", err)
	}
	if removed == 0 {
		logf(log, "nothing installed")
	}
	return nil
}

// Integrate finishes a successful install: desktop entry + input access
// on Linux, shortcuts + uninstall entry on Windows. Best-effort — the
// binaries are already in place; a failed integration step must never
// look like a failed install.
func Integrate(dir string, log func(string, ...any)) error {
	if err := integrateDesktop(dir); err != nil {
		logf(log, "desktop integration: %v", err)
		return err
	}
	return nil
}

// Launch starts the installed GUI. On Windows this detaches the GUI from
// the installer process; on Linux it is a no-op (the desktop entry is
// how users start it).
func Launch(dir string) error {
	return launchGUI(dir)
}

// installRelease downloads `tag`'s archive, verifies it, and applies it.
func installRelease(tag string, rel *selfupdate.Release, opts Options) error {
	asset, err := rel.AssetFor()
	if err != nil {
		return err
	}
	logf(opts.Log, "downloading %s (%d bytes)", asset.Name, asset.Size)
	tmp, err := os.MkdirTemp("", "kvmshare-install-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	archive := filepath.Join(tmp, asset.Name)
	if err := selfupdate.Download(asset.URL, archive); err != nil {
		return err
	}
	phasef(opts.Phase, "Verifying checksum", 0.5)

	sums, err := selfupdate.FetchChecksums(rel)
	if err != nil {
		return err
	}
	expected, ok := sums[asset.Name]
	if !ok {
		return fmt.Errorf("SHA256SUMS has no entry for %s", asset.Name)
	}
	if err := selfupdate.VerifyFile(archive, expected); err != nil {
		return err
	}
	logf(opts.Log, "checksum ok")
	phasef(opts.Phase, "Installing", 0.7)

	extracted, err := selfupdate.Extract(archive, tmp)
	if err != nil {
		return err
	}
	return applyExtracted(extracted, opts.Log, opts.Phase)
}

func applyExtracted(extracted map[string]string, log func(string, ...any), phase func(string, float64)) error {
	written, err := selfupdate.Apply(extracted)
	if err != nil {
		return err
	}
	for _, p := range written {
		logf(log, "%s", p)
	}
	phasef(phase, "Done", 1.0)
	return nil
}

// copyFile copies src to dst (used to stage --local sources so the
// install's renames never touch the user's files).
func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	return os.Chmod(dst, 0o755)
}
