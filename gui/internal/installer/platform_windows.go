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