package main

// XDG autostart: the freedesktop.org standard every mainstream Linux
// desktop implements (GNOME, KDE, XFCE, Cinnamon, MATE…) and most
// window-manager setups honor through their session tools. A .desktop
// file in ~/.config/autostart is the whole contract — no init system,
// no systemd user units, nothing to migrate when the user switches
// desktops or init systems. A window-manager-only user without an XDG
// autostart implementation simply doesn't get the auto-launch (and can
// add the same .desktop line to their WM config; the entry is a plain
// file they can read).

import (
	"fmt"
	"os"
	"path/filepath"
)

// autostartPath is where the XDG autostart entry lives.
func autostartPath() (string, error) {
	cfg, err := os.UserConfigDir() // $XDG_CONFIG_HOME, defaulting to ~/.config
	if err != nil {
		return "", fmt.Errorf("resolve config dir: %w", err)
	}
	return filepath.Join(cfg, "autostart", "kvmshare.desktop"), nil
}

// The entry mirrors the desktop file the installer writes (same icon,
// same name), with Terminal=false and no StartupWMClass churn: a plain
// launch of the GUI. Exec is quoted because install paths may contain
// spaces (a Windows-style per-user dir or a mounted home).
func autostartEntry(execPath string) []byte {
	return fmt.Appendf(nil, `[Desktop Entry]
Type=Application
Version=1.0
Name=kvmshare
Comment=Share one keyboard and mouse across your machines
Exec="%s"
Icon=kvmshare
Terminal=false
Categories=Utility;
X-GNOME-Autostart-enabled=true
`, execPath)
}

func enableAutostart(exe string) error {
	p, err := autostartPath()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		return fmt.Errorf("create autostart dir: %w", err)
	}
	return os.WriteFile(p, autostartEntry(exe), 0o644)
}

func disableAutostart() error {
	p, err := autostartPath()
	if err != nil {
		return err
	}
	if err := os.Remove(p); err != nil && !os.IsNotExist(err) {
		return err
	}
	return nil
}

func autostartPresent() bool {
	p, err := autostartPath()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}
