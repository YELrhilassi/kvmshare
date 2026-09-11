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
	// Quoted path, no arguments: the GUI resumes whatever role it saved.
	return k.SetStringValue(autostartValueName, `"`+exe+`"`)
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
