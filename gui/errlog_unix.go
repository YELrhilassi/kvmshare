//go:build unix

package main

import (
	"os"
	"syscall"
)

// keepLogOpen holds the handle for the redirected stderr so the runtime
// never garbage-collects it while fd 2 still points at it.
var keepLogOpen *os.File

// redirectStderr points file descriptor 2 at `path` (append). Everything
// the process writes to stderr afterwards — Go runtime panics, WebKit
// diagnostics, framework logs — lands in the file instead of vanishing
// with the launcher that started us (autostart, dmenu, the tray).
//
// The file is size-capped: past stderrLogCap the previous content is
// dropped at startup (never mid-run), so a chatty WebKit cannot grow it
// without limit over weeks of daily use. Best-effort: when the redirect
// fails the GUI keeps its inherited stderr; logging is diagnostics,
// never a launch blocker.
const stderrLogCap = 1 << 20 // 1 MiB of history is plenty to diagnose a crash

func redirectStderr(path string) {
	if st, err := os.Stat(path); err == nil && st.Size() > stderrLogCap {
		_ = os.Truncate(path, 0)
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return
	}
	if err := syscall.Dup3(int(f.Fd()), 2, 0); err != nil {
		f.Close()
		return
	}
	keepLogOpen = f
}
