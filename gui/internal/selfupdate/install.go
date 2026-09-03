// Install layout: where the pieces go on each OS. The layout matches the
// Makefile's dev install (binaries on PATH, config and state under the
// user's XDG dirs) so a release install and a `make install` agree.

package selfupdate

import (
	_ "embed"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
)

//go:embed assets/tray.png
var trayIcon []byte

//go:embed assets/kvmshare.desktop
var desktopEntry []byte

//go:embed assets/kvmshare-server.toml
var sampleConfig []byte

// InstallDir is where release binaries land.
//
//	Linux:  ~/.local/bin        (already on PATH for most setups)
//	Other:  <prefix>/kvmshare   (override with KVMSHARE_PREFIX)
func InstallDir() string {
	if p := os.Getenv("KVMSHARE_PREFIX"); p != "" {
		return p
	}
	if runtime.GOOS == "windows" {
		if base := os.Getenv("LOCALAPPDATA"); base != "" {
			return filepath.Join(base, "kvmshare")
		}
		return filepath.Join(os.TempDir(), "kvmshare")
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return filepath.Join(os.TempDir(), "kvmshare")
	}
	return filepath.Join(home, ".local", "bin")
}

// Apply places the extracted binaries where they belong and writes the
// per-user extras (desktop entry, icon, sample config). Idempotent:
// re-running updates files in place. Returns the paths written.
func Apply(extracted map[string]string) ([]string, error) {
	dir := InstallDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	var written []string
	for _, bin := range Binaries() {
		src, ok := extracted[bin]
		if !ok {
			return nil, fmt.Errorf("release archive is missing %s", bin)
		}
		dst := filepath.Join(dir, bin)
		if err := replaceFile(src, dst); err != nil {
			return nil, fmt.Errorf("install %s: %w", bin, err)
		}
		written = append(written, dst)
	}
	if err := installExtras(); err != nil {
		return written, err
	}
	return written, nil
}

// replaceFile moves `src` over `dst`, surviving a running process: the
// target is first renamed aside (renaming a running binary is allowed on
// both Linux and Windows; deleting it is not). Falls back to a copy when
// a rename is impossible (e.g. src sits on a tmpfs while the install
// dir is on the real disk).
func replaceFile(src, dst string) error {
	old := dst + ".old"
	_ = os.Remove(old)
	if _, err := os.Stat(dst); err == nil {
		if err := os.Rename(dst, old); err != nil {
			return err
		}
	}
	if err := moveInto(src, dst); err != nil {
		// Put the old one back rather than leaving a gap.
		_ = os.Rename(old, dst)
		return err
	}
	_ = os.Remove(old)
	return nil
}

// moveInto puts `src` at `dst`, renaming when possible and copying
// across devices (os.Rename returns EXDEV for that).
func moveInto(src, dst string) error {
	if err := os.Rename(src, dst); err == nil {
		return nil
	}
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o755)
	if err != nil {
		return err
	}
	_, cerr := io.Copy(out, in)
	cerr2 := out.Close()
	if cerr != nil {
		return cerr
	}
	if cerr2 != nil {
		return cerr2
	}
	return os.Chmod(dst, 0o755)
}

// installExtras writes the non-binary pieces (Linux desktop integration;
// Windows gets nothing extra yet).
func installExtras() error {
	if runtime.GOOS != "linux" {
		return nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return nil // no home — binaries alone are still fine
	}
	appsDir := filepath.Join(home, ".local", "share", "applications")
	iconsDir := filepath.Join(home, ".local", "share", "icons", "hicolor", "256x256", "apps")
	if err := os.MkdirAll(appsDir, 0o755); err == nil {
		if err := os.WriteFile(filepath.Join(appsDir, "kvmshare.desktop"), desktopEntry, 0o644); err == nil {
			if err := os.MkdirAll(iconsDir, 0o755); err == nil {
				_ = os.WriteFile(filepath.Join(iconsDir, "kvmshare.png"), trayIcon, 0o644)
			}
		}
	}
	// Sample config on first install only — existing layouts must survive
	// updates untouched.
	cfg := filepath.Join(home, ".config", "kvmshare", "kvmshare-server.toml")
	if _, err := os.Stat(cfg); os.IsNotExist(err) {
		if err := os.MkdirAll(filepath.Dir(cfg), 0o755); err == nil {
			_ = os.WriteFile(cfg, sampleConfig, 0o644)
		}
	}
	return nil
}
