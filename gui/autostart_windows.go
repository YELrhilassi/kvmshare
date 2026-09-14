package main

// Per-user launch-at-logon via the Run key — the Windows-standard,
// user-visible-in-Task-Manager mechanism (HKCU\...\Run). Chosen over a
// Startup-folder shortcut or a scheduled task because it needs no
// elevation, is the first place users look to control autostart, and
// Task Manager's Startup tab can both see and disable it — the operator
// stays in charge from the OS side too.

import (
	"fmt"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows/registry"
)

const autostartRunKey = `Software\Microsoft\Windows\CurrentVersion\Run`

// autostartValueName is the value this app owns under the Run key.
const autostartValueName = "kvmshare"

func enableAutostart(exe string) error {
	k, err := registry.OpenKey(registry.CURRENT_USER, autostartRunKey, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("open Run key: %w", err)
	}
	defer k.Close()
	// Quoted path plus --autostart: the flag is how the GUI knows this
	// launch came from the login session — it may start hidden to the
	// tray; a manual launch (no flag) always shows the window.
	return k.SetStringValue(autostartValueName, `"`+exe+`" --autostart`)
}

func disableAutostart() error {
	k, err := registry.OpenKey(registry.CURRENT_USER, autostartRunKey, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("open Run key: %w", err)
	}
	defer k.Close()
	if err := k.DeleteValue(autostartValueName); err != nil {
		if err == registry.ErrNotExist {
			return nil
		}
		return err
	}
	return nil
}

func autostartPresent() bool {
	k, err := registry.OpenKey(registry.CURRENT_USER, autostartRunKey, registry.QUERY_VALUE)
	if err != nil {
		return false
	}
	defer k.Close()
	// Any existing value counts — including one a user hand-edited to
	// point somewhere else; the OS truth wins over our settings flag.
	val, _, err := k.GetStringValue(autostartValueName)
	if err != nil || val == "" {
		return false
	}
	_ = filepath.Base(val) // shape check only; the OS launches whatever is there
	return true
}

// autostartHasFlag reports whether the existing Run-key value already
// launches the GUI with --autostart (values written by older builds did
// not; healAutostartEntry rewrites those).
func autostartHasFlag() bool {
	k, err := registry.OpenKey(registry.CURRENT_USER, autostartRunKey, registry.QUERY_VALUE)
	if err != nil {
		return false
	}
	defer k.Close()
	val, _, err := k.GetStringValue(autostartValueName)
	return err == nil && strings.Contains(val, "--autostart")
}
