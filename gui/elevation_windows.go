//go:build windows

package main

// Elevation for the role processes on Windows.
//
// ## Why the roles need an elevated token
//
// The role binaries move the cursor and type into every window of the
// session, including elevated ones (Task Manager, installers, UAC
// prompts moved to the normal desktop). Windows UIPI filters input from
// a lower-integrity process into higher-integrity windows: a
// non-elevated client silently fails to control exactly the windows a
// user is most likely to fight with. So the roles must run elevated.
//
// ## Why the GUI does NOT run elevated
//
// Windows skips elevated entries at logon for the Run key (the
// standard per-user autostart), and an elevated GUI breaks the
// drag-and-drop and document flows users expect from a normal app. The
// elevation therefore belongs to the smallest surface that needs it:
// the role process, spawned per start.
//
// ## The mechanism
//
// A per-user *scheduled task* with run level HIGHEST launches the role
// binary. `schtasks /Run` starts it elevated without a UAC prompt
// (granting highest privileges was accepted once when the task was
// created — by the elevated installer, or by one UAC prompt on first
// use); the task self-deletes when the role process exits, and any
// leftover from a crash is swept at GUI startup. This is the same
// mechanism teams use for "run elevated at logon" and what the task
// exists for here: elevation without permanently elevated apps.
//
// ## Standard users
//
// Creating the task needs the "Log on as the current user, run with
// highest privileges" right, which administrators grant silently and a
// standard user is refused. The fallback keeps the product working for
// standard accounts: spawn the role directly (non-elevated). The
// session then works for every normal window and degrades exactly
// where Windows itself refuses — elevated windows ignore non-elevated
// input by OS design — and the GUI says so plainly (RoleElevationInfo).

import (
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"

	"golang.org/x/sys/windows"
)

// The scheduled task that lends the role binaries an elevated token.
// One task per install per user, recreated on demand and self-deleting:
// there is no standing privileged object on the machine. The name is
// user-scoped because the task namespace is machine-global — two users
// on one machine must never fight over one task.
func elevationTaskName() string {
	user := os.Getenv("USERNAME")
	if user == "" {
		user = "user"
	}
	return "kvmshare-role-" + user
}

// Task creation arguments: run level HIGHEST (the whole point), logon
// type Interactive (runs in the operator's session, where the input
// must land), and no start/end boundary (on demand only).
const elevationTaskXML = `<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>Highest</RunLevel>
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

// elevationTaskXMLFile is where the task definition is staged before
// registration (schtasks takes XML from a file; a heredoc through cmd
// would fight every quoting layer).
const elevationTaskXMLFile = "kvmshare-role-task.xml"

// elevated reports whether THIS process holds an elevated token. The
// GUI usually does not (by design); a hand-elevated one can, and then
// children simply inherit — no task needed.
func elevated() bool {
	return windows.GetCurrentProcessToken().IsElevated()
}

// ensureElevationTask recreates the role-elevation task so its command
// matches `exe` and `args`. schtasks refuses to overwrite an existing
// task, so the previous definition is deleted first; the task is
// creation-fail-fast — the caller treats an error as "not available".
// The XML is staged in the state dir (writable by this user by
// definition) and never carries secrets.
func ensureElevationTask(stateDir, exe, args string) error {
	if out, err := schtasks("/Delete", "/TN", elevationTaskName(), "/F"); err != nil {
		// "does not exist" is the expected first-run case; anything
		// else still deserves the retry attempt below.
		_ = out
	}
	xmlPath := stateDir + "\\" + elevationTaskXMLFile
	xml := fmt.Sprintf(elevationTaskXML, xmlEscape(exe), xmlEscape(args))
	if err := writeFileUTF16LE(xmlPath, xml); err != nil {
		return fmt.Errorf("stage task definition: %w", err)
	}
	if out, err := schtasks("/Create", "/TN", elevationTaskName(), "/XML", xmlPath); err != nil {
		return fmt.Errorf("register task: %v: %s", err, oneLine(out))
	}
	return nil
}

// startElevated launches `exe args` through the task and returns once
// the process exists. The caller polls its role lock for readiness (the
// child needs a moment to appear; the task start itself only queues).
func startElevated() error {
	if out, err := schtasks("/Run", "/TN", elevationTaskName()); err != nil {
		return fmt.Errorf("run task: %v: %s", err, oneLine(out))
	}
	return nil
}

// deleteElevationTask removes the task. Best-effort: a leftover task is
// swept again at the next GUI start.
func deleteElevationTask() {
	_, _ = schtasks("/Delete", "/TN", elevationTaskName(), "/F")
}

// cleanupElevationTask deletes leftover scheduled tasks. Two kinds:
// a role-elevation task abandoned by a hard kill (it normally
// self-deletes on role exit), and the debug tasks earlier sessions
// created to launch the GUI itself elevated — an elevated GUI breaks
// the elevation model (non-elevated Run-key GUI vs elevated task GUI
// racing over one state dir). Safe at every GUI start: missing tasks
// are simply not there.
func (a *App) cleanupElevationTask() {
	deleteElevationTask()
	for _, debug := range []string{"kvmshare-gui-dbg", "kvmshare-gui-start", "kvmshare-gui-v074"} {
		_, _ = schtasks("/Delete", "/TN", debug, "/F")
	}
}

// schtasks runs one schtasks verb hidden (the GUI is a windowsgui
// binary; a plain console child would flash a window).
func schtasks(args ...string) ([]byte, error) {
	cmd := exec.Command("schtasks", args...)
	cmd.SysProcAttr = hiddenSysProcAttr()
	return cmd.CombinedOutput()
}

// spawnRoleElevated starts `exe args...` elevated via the task and
// waits until the process is actually alive (polling `alive`, which
// checks the role lock / process visibility). Returns when the role is
// up, or an error when the task path is unavailable (standard user,
// policy) — the caller then falls back to a direct spawn.
func spawnRoleElevated(stateDir, exe string, args []string, alive func() bool, timeout time.Duration) error {
	if err := ensureElevationTask(stateDir, exe, strings.Join(quoteArgs(args), " ")); err != nil {
		return err
	}
	if err := startElevated(); err != nil {
		deleteElevationTask()
		return err
	}
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		if alive() {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}
	deleteElevationTask()
	return fmt.Errorf("elevated role did not appear within %s", timeout)
}

// quoteArgs wraps each argument in double quotes (schtasks passes the
// string to CreateProcess verbatim). Paths and args here are our own
// (install dir, fixed flags), so simple quoting is sufficient; embedded
// quotes cannot occur in these values.
func quoteArgs(args []string) []string {
	out := make([]string, len(args))
	for i, a := range args {
		out[i] = `"` + strings.ReplaceAll(a, `"`, ``) + `"`
	}
	return out
}

// xmlEscape makes a string safe inside a task XML text node.
func xmlEscape(s string) string {
	r := strings.NewReplacer("&", "&amp;", "<", "&lt;", ">", "&gt;", `"`, "&quot;", "'", "&apos;")
	return r.Replace(s)
}

func oneLine(b []byte) string {
	return strings.TrimSpace(strings.ReplaceAll(string(b), "\n", " "))
}
