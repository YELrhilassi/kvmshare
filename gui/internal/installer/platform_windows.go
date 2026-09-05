//go:build windows

package installer

// Windows desktop integration for the installer: the .ico, Start Menu and
// desktop shortcuts, the Add/Remove Programs entry, and launching the GUI
// when an install finishes. Shortcuts are created through the WScript.Shell
// COM object (the platform's own mechanism) via a base64-encoded PowerShell
// command — immune to every quoting layer between Go and cmd.

import (
	"encoding/base64"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"unicode/utf16"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"

	"kvmshare/gui/internal/selfupdate"
)

const icoName = "kvmshare.ico"

// integrateDesktop makes a finished install visible on the desktop: writes
// the icon next to the binaries, creates Start Menu + desktop shortcuts,
// and registers an uninstall entry. Best-effort per step — a shortcut
// failure must not roll back a completed binary install.
func integrateDesktop(dir string) error {
	var problems []string

	if err := os.WriteFile(filepath.Join(dir, icoName), selfupdate.IconBytes, 0o644); err != nil {
		problems = append(problems, fmt.Sprintf("icon: %v", err))
	}
	if err := createShortcuts(dir); err != nil {
		problems = append(problems, fmt.Sprintf("shortcuts: %v", err))
	}
	if err := registerUninstall(dir); err != nil {
		problems = append(problems, fmt.Sprintf("uninstall entry: %v", err))
	}
	// A sample layout config on first install only — the server refuses
	// to start without one (its --config must exist). The GUI also
	// creates it on demand, so both fresh installs and upgrades are
	// covered; existing layouts are never overwritten.
	if home, err := os.UserHomeDir(); err == nil {
		if err := selfupdate.EnsureServerConfig(
			filepath.Join(home, ".config", "kvmshare", "kvmshare-server.toml"),
		); err != nil {
			problems = append(problems, fmt.Sprintf("sample config: %v", err))
		}
	}
	if len(problems) > 0 {
		return fmt.Errorf("desktop integration had issues: %s", strings.Join(problems, "; "))
	}
	return nil
}

// removeDesktopIntegration undoes integrateDesktop (used by --uninstall).
func removeDesktopIntegration(dir string) error {
	removeShortcuts()
	_ = registry.DeleteKey(registry.CURRENT_USER, uninstallKey)
	_ = os.Remove(filepath.Join(dir, icoName))
	// Put the UAC prompt policy back the way it was before kvmshare
	// moved it to the normal desktop (best-effort: nothing to restore
	// when kvmshare never changed it).
	if err := restoreUacPolicy(); err != nil {
		return fmt.Errorf("restore UAC policy: %w", err)
	}
	return nil
}

// IsElevated reports whether this process runs with an elevated token
// (needed to write HKLM — the UAC policy lives there).
func IsElevated() bool {
	return windows.GetCurrentProcessToken().IsElevated()
}

// SelfElevate re-runs this binary elevated (one UAC consent prompt) and
// waits for it to finish. Used by --uninstall so the UAC policy restore
// (HKLM) always succeeds, mirroring how the Linux side re-executes
// itself through pkexec for privileged steps.
func SelfElevate(args []string) error {
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	quoted := make([]string, len(args))
	for i, a := range args {
		quoted[i] = "'" + strings.ReplaceAll(a, "'", "''") + "'"
	}
	script := fmt.Sprintf(
		"Start-Process -FilePath '%s' -ArgumentList %s -Verb RunAs -Wait",
		strings.ReplaceAll(exe, "'", "''"),
		strings.Join(quoted, ","),
	)
	return runPS(script)
}

// UAC consent prompts normally appear on the Winlogon secure desktop — a
// protected desktop that **no** process can inject input into, not even
// an elevated one. A KVM machine whose only mouse and keyboard is the
// shared stream therefore cannot answer them. The standard fix (used by
// Barrier, TeamViewer and other remote-control tools) is to make prompts
// appear on the normal desktop instead: there consent.exe runs at the
// same integrity level as the elevated client, so SendInput reaches it
// and the shared cursor can click Yes / type credentials.
//
// This writes `PromptOnSecureDesktop = 0` (HKLM, the GUI that calls it
// is elevated) and remembers the previous value under kvmshare's own
// registry key so `--uninstall` can restore it exactly.
const (
	// The policy that decides where UAC consent prompts appear.
	uacPoliciesKey = `SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System`
	// Where kvmshare remembers the value it replaced, for uninstall.
	uacPrevKey = `SOFTWARE\kvmshare`
	// The value names under uacPrevKey: the old PromptOnSecureDesktop
	// value, and whether it existed at all (absent means the default
	// "secure desktop on", restored by deleting the value).
	uacPrevValue    = "PromptOnSecureDesktopPrev"
	uacPrevExisted  = "PromptOnSecureDesktopExisted"
	uacPolicyValue  = "PromptOnSecureDesktop"
)

// EnsureUacAnswerable makes UAC prompts answerable by the shared input.
// Idempotent: once the policy is already 0, nothing happens. Best-effort
// contract for callers — failures are logged, never fatal; without it
// the client's desktop watchdog still releases local input as a safety
// net, the prompt just cannot be answered remotely.
func EnsureUacAnswerable() error {
	k, err := registry.OpenKey(registry.LOCAL_MACHINE, uacPoliciesKey, registry.QUERY_VALUE)
	if err != nil {
		return fmt.Errorf("open %s: %w", uacPoliciesKey, err)
	}
	cur, _, qerr := k.GetIntegerValue(uacPolicyValue)
	k.Close()
	if qerr == nil && cur == 0 {
		return nil // already on the normal desktop — nothing to do
	}
	// Remember what to restore: the previous value, or 1 (the default
	// when absent — deleting the value restores that default).
	prev := uint64(1)
	existed := false
	if qerr == nil {
		prev = cur
		existed = true
	}
	if err := rememberUacPrev(prev, existed); err != nil {
		return err
	}
	wk, err := registry.OpenKey(registry.LOCAL_MACHINE, uacPoliciesKey, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("open %s for write: %w", uacPoliciesKey, err)
	}
	defer wk.Close()
	return wk.SetDWordValue(uacPolicyValue, 0)
}

func rememberUacPrev(prev uint64, existed bool) error {
	k, _, err := registry.CreateKey(registry.LOCAL_MACHINE, uacPrevKey, registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer k.Close()
	// PromptOnSecureDesktop is a DWORD; its range is 0/1 so a DWORD
	// always holds the previous value.
	if err := k.SetDWordValue(uacPrevValue, uint32(prev)); err != nil {
		return err
	}
	ex := uint32(0)
	if existed {
		ex = 1
	}
	return k.SetDWordValue(uacPrevExisted, ex)
}

// restoreUacPolicy undoes EnsureUacAnswerable: the policy returns to the
// value kvmshare found (or is deleted when it never existed), and the
// memory key is dropped. No-op when kvmshare never changed the policy.
func restoreUacPolicy() error {
	k, err := registry.OpenKey(registry.LOCAL_MACHINE, uacPrevKey, registry.QUERY_VALUE)
	if err != nil {
		if err == registry.ErrNotExist {
			return nil // kvmshare never changed it (or already restored)
		}
		return err
	}
	prev, _, err := k.GetIntegerValue(uacPrevValue)
	existed, _, _ := k.GetIntegerValue(uacPrevExisted)
	k.Close()
	if err != nil {
		return err
	}

	pk, err := registry.OpenKey(registry.LOCAL_MACHINE, uacPoliciesKey, registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer pk.Close()
	if existed != 0 {
		if err := pk.SetDWordValue(uacPolicyValue, uint32(prev)); err != nil {
			return err
		}
	} else if err := pk.DeleteValue(uacPolicyValue); err != nil && err != registry.ErrNotExist {
		return err
	}
	_ = registry.DeleteKey(registry.LOCAL_MACHINE, uacPrevKey)
	return nil
}

// ensureInputAccess is unsupported on Windows.
func ensureInputAccess() error { return fmt.Errorf("--input-access is Linux-only") }

// launchGUI starts the installed GUI detached from this process. The GUI is
// a windowsgui (no console) binary, so nothing flashes on screen.
func launchGUI(dir string) error {
	gui := filepath.Join(dir, "kvmshare-gui.exe")
	cmd := exec.Command(gui)
	if err := cmd.Start(); err != nil {
		return fmt.Errorf("launch kvmshare-gui: %w", err)
	}
	return nil
}

const uninstallKey = `Software\Microsoft\Windows\CurrentVersion\Uninstall\kvmshare`

func registerUninstall(dir string) error {
	k, _, err := registry.CreateKey(registry.CURRENT_USER, uninstallKey, registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer k.Close()

	version := selfupdate.Version
	if version == "" {
		version = "0.0.0"
	}
	vals := map[string]string{
		"DisplayName":       "kvmshare",
		"DisplayVersion":    strings.TrimPrefix(version, "v"),
		"DisplayIcon":       filepath.Join(dir, "kvmshare-gui.exe"),
		"InstallLocation":   dir,
		"Publisher":         "kvmshare",
		"UninstallString":   filepath.Join(dir, "kvmshare-install.exe") + " --uninstall",
		"URLInfoAbout":      "https://github.com/YELrhilassi/kvmshare",
		"NoModify":          "1",
		"NoRepair":          "1",
		"QuietUninstallString": filepath.Join(dir, "kvmshare-install.exe") + " --uninstall",
	}
	for name, value := range vals {
		if err := k.SetStringValue(name, value); err != nil {
			return err
		}
	}
	return nil
}

// createShortcuts puts kvmshare in the per-user Start Menu and on the
// desktop, pointing at the GUI with the app icon.
func createShortcuts(dir string) error {
	gui := filepath.Join(dir, "kvmshare-gui.exe")
	ico := filepath.Join(dir, icoName)

	// Single-quoted PowerShell strings: escape embedded quotes by doubling.
	q := func(s string) string { return "'" + strings.ReplaceAll(s, "'", "''") + "'" }
	// IconLocation is a single "path,index" string; the comma must stay
	// inside the quotes or PowerShell builds an array and the path is lost.
	script := fmt.Sprintf(`
$ws = New-Object -ComObject WScript.Shell
$targets = @(
  (Join-Path ([Environment]::GetFolderPath('Programs')) 'kvmshare.lnk'),
  (Join-Path ([Environment]::GetFolderPath('Desktop')) 'kvmshare.lnk')
)
foreach ($p in $targets) {
  $s = $ws.CreateShortcut($p)
  $s.TargetPath = %s
  $s.WorkingDirectory = %s
  $s.IconLocation = %s
  $s.Description = 'kvmshare - share one keyboard and mouse across your machines'
  $s.Save()
}
`, q(gui), q(dir), q(ico+",0"))

	return runPS(script)
}

// removeShortcuts deletes the shortcuts created by createShortcuts. A
// missing file is not an error.
func removeShortcuts() {
	script := `
$ws = New-Object -ComObject WScript.Shell
$targets = @(
  (Join-Path ([Environment]::GetFolderPath('Programs')) 'kvmshare.lnk'),
  (Join-Path ([Environment]::GetFolderPath('Desktop')) 'kvmshare.lnk')
)
foreach ($p in $targets) {
  if (Test-Path $p) { Remove-Item $p -Force }
}
`
	_ = runPS(script)
}

// runPS executes a PowerShell script via -EncodedCommand (UTF-16LE
// base64), avoiding every quoting issue across Go -> cmd -> powershell.
func runPS(script string) error {
	u16 := utf16.Encode([]rune(script))
	buf := make([]byte, len(u16)*2)
	for i, r := range u16 {
		buf[i*2] = byte(r)
		buf[i*2+1] = byte(r >> 8)
	}
	enc := base64.StdEncoding.EncodeToString(buf)
	cmd := exec.Command("powershell.exe", "-NoProfile", "-NonInteractive", "-EncodedCommand", enc)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("powershell: %v: %s", err, strings.TrimSpace(string(out)))
	}
	return nil
}