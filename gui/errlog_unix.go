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
// Best-effort: when the redirect fails the GUI keeps its inherited
// stderr; logging is diagnostics, never a launch blocker.
func redirectStderr(path string) {
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
