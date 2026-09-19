//go:build windows

package installer

// The Windows elevation contract for role processes.
//
// ## Why the roles need an elevated token
//
// The role binaries move the cursor and type into every window of the
// session — including elevated ones (Task Manager, installers, UAC
// prompts moved to the normal desktop). Windows UIPI filters input from
// a lower-integrity process into higher-integrity windows, so a
// non-elevated client silently fails to control exactly the windows a
// user is most likely to fight with. The roles therefore run elevated.
//
// ## Why the GUI does NOT run elevated
//
// Windows skips elevated entries at logon for the Run key (the standard
// per-user autostart), so an elevated GUI could not autostart. The
// elevation belongs to the smallest surface that needs it: the role.
//
// ## The mechanism
//
// A per-user scheduled task with run level HighestAvailable per role
// launches the role binary. **Creation and running are different
// privileges**: creating a HIGHEST task requires Administrators
// membership (a standard user is refused), while *running* an existing
// task needs no elevation at all. The tasks are created once at install
// time (the installer runs elevated anyway) and self-heal from the GUI
// when missing — an admin user's filtered token is enough, no prompt.
//
// Measured, and worth recording: schtasks rejects the XML value
// "Highest" with "(6,27):RunLevel:Highest ... incorrectly formatted or
// out of range" — the CLI's /RL HIGHEST maps to the XML value
// **HighestAvailable**, and that error says nothing about the caller's
// token. The invalid value once masqueraded as an elevation failure and
// pushed the design into an unnecessary UAC relay.
//
// A task's command line is fixed at creation, so it cannot carry the
// values that change per run (the client's server address, log paths).
// The task therefore runs the role with `--args-file <path>` and the
// GUI rewrites that file before every start with the full argv — the
// contract is `kvmshare_app::args::merged_argv` on the Rust side: one
// argv token per line.
//
// ## Standard users
//
// Installing needs administrator rights on Windows anyway, so the task
// creation rides along at install time. A user who ends up without the
// tasks (a portable copy, a policy wipe) gets a graceful fallback: the
// GUI relays task creation through the elevated installer (one prompt),
// or — when declined — spawns the role directly (non-elevated). The
// session then works for every normal window and degrades exactly where
// Windows itself refuses.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf16"

	"golang.org/x/sys/windows"

	"kvmshare/gui/internal/selfupdate"
)

// elevationTaskXMLFile is where task definitions are staged before
// `/Create /XML` (schtasks requires UTF-16LE — see writeFileUTF16LE).
const elevationTaskXMLFile = "kvmshare-elevation-task.xml"

// ElevationRoles are the roles that get an elevation task, in the order
// they are created and deleted.
var ElevationRoles = []string{"server", "client"}

// ElevationTaskName is the scheduled-task name for one role, scoped to
// the user: the task namespace is machine-global, so two users on one
// machine must never fight over one task.
func ElevationTaskName(role string) string {
	user := os.Getenv("USERNAME")
	if user == "" {
		user = "user"
	}
	return "kvmshare-" + role + "-" + user
}

// ArgsFilePath is the per-run argument file for a task-started role.
// The task's command line is fixed; the GUI stages the real argv here
// (one token per line) before every start.
func ArgsFilePath(stateDir, role string) string {
	return filepath.Join(stateDir, role+"-args.txt")
}

// elevationTaskXML is the task definition: run level HighestAvailable
// (the whole point — see the measured note above for why the value is
// not "Highest"), logon type InteractiveToken (runs in the operator's
// session, where the input must land), no time limit, on-demand only.
const elevationTaskXML = `<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>HighestAvailable</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>%s</Command>
      <Arguments>%s</Arguments>
    </Exec>
  </Actions>
</Task>
`

// EnsureElevationTasks creates (or re-creates) the per-role elevation
// tasks for this user. Requires Administrators membership (a standard
// user cannot register a HighestAvailable task) — not an elevated
// token: an admin's filtered token is accepted, which is what lets the
// GUI self-heal without a UAC prompt. Idempotent.
//
// The task points at the role binary in the install dir (where the
// installer — and the GUI — live) and passes the state-dir args file;
// the GUI stages the actual argv there before every run.
func EnsureElevationTasks(stateDir string) error {
	exeDir := ""
	if exe, err := os.Executable(); err == nil {
		exeDir = filepath.Dir(exe)
	}
	for _, role := range ElevationRoles {
		exe := filepath.Join(exeDir, "kvmshare-"+role+".exe")
		if _, err := os.Stat(exe); err != nil {
			// Not beside this binary: the install dir is the only other
			// place the install flow puts role binaries.
			exe = filepath.Join(selfupdate.InstallDir(), "kvmshare-"+role+".exe")
		}
		args := fmt.Sprintf(`--args-file "%s"`, ArgsFilePath(stateDir, role))

		name := ElevationTaskName(role)
		if out, err := schtasks("/Delete", "/TN", name, "/F"); err != nil {
			_ = out // "does not exist" is the normal first-install case
		}
		xmlPath := filepath.Join(os.TempDir(), elevationTaskXMLFile)
		xml := fmt.Sprintf(elevationTaskXML, xmlEscape(exe), xmlEscape(args))
		if err := writeFileUTF16LE(xmlPath, xml); err != nil {
			return fmt.Errorf("stage %s task definition: %w", role, err)
		}
		if out, err := schtasks("/Create", "/TN", name, "/XML", xmlPath); err != nil {
			os.Remove(xmlPath)
			return fmt.Errorf("register %s task: %v: %s", role, err, oneLine(out))
		}
		os.Remove(xmlPath)
	}
	return nil
}

// DeleteElevationTasks removes the per-role tasks (uninstall path).
// Best-effort: a missing task is not an error.
func DeleteElevationTasks() {
	for _, role := range ElevationRoles {
		_, _ = schtasks("/Delete", "/TN", ElevationTaskName(role), "/F")
	}
}

// DebugTaskNames are the leftover scheduled tasks earlier builds created
// to launch the GUI itself elevated — an elevated GUI breaks the
// elevation model (non-elevated Run-key GUI vs elevated task GUI racing
// over one state dir). Swept at GUI start.
func DebugTaskNames() []string {
	return []string{"kvmshare-gui-dbg", "kvmshare-gui-start", "kvmshare-gui-v074"}
}

// SweepDebugTasks deletes the leftover debug tasks (see DebugTaskNames).
// Best-effort: a missing task is not an error, and a failure to delete
// one never blocks startup.
func SweepDebugTasks() {
	for _, name := range DebugTaskNames() {
		_, _ = schtasks("/Delete", "/TN", name, "/F")
	}
}

// AdminUser reports whether the current user holds Administrators
// membership on its (possibly filtered) token — the predictor for
// whether an elevation-task creation attempt can succeed at all.
func AdminUser() bool {
	if IsElevated() {
		return true
	}
	admin, err := windows.CreateWellKnownSid(windows.WinBuiltinAdministratorsSid)
	if err != nil {
		return false
	}
	member, err := windows.GetCurrentProcessToken().IsMember(admin)
	return err == nil && member
}

// schtasks runs one schtasks verb without flashing a console window
// (callers here are windowsgui binaries; a plain console child would
// pop a cmd window for its duration).
func schtasks(args ...string) ([]byte, error) {
	return hiddenCmd("schtasks", args...).CombinedOutput()
}

// RunElevationTask starts a role through its task. Unprivileged by
// design — running an existing HIGHEST task needs no elevation; only
// creating one does (the installer did that).
func RunElevationTask(role string) error {
	if out, err := schtasks("/Run", "/TN", ElevationTaskName(role)); err != nil {
		return fmt.Errorf("run task: %v: %s", err, oneLine(out))
	}
	return nil
}

// ElevationTaskPresent reports whether the role's task exists (a query
// is unprivileged; a task we can query is a task we can run).
func ElevationTaskPresent(role string) bool {
	_, err := schtasks("/Query", "/TN", ElevationTaskName(role))
	return err == nil
}

// writeFileUTF16LE writes s as UTF-16LE with a BOM — the encoding
// schtasks requires for /XML task definitions.
func writeFileUTF16LE(path, s string) error {
	u16 := utf16.Encode([]rune(s))
	buf := make([]byte, 2+len(u16)*2)
	buf[0], buf[1] = 0xFF, 0xFE // BOM: little-endian marker
	for i, r := range u16 {
		buf[2+i*2] = byte(r)
		buf[3+i*2] = byte(r >> 8)
	}
	return os.WriteFile(path, buf, 0o600)
}

// xmlEscape makes a string safe inside a task XML text node.
func xmlEscape(s string) string {
	r := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;", "'", "&apos;")
	return r.Replace(s)
}

func oneLine(b []byte) string {
	return strings.TrimSpace(strings.ReplaceAll(string(b), "\n", " "))
}
