//go:build !windows

package main

// keycapture_other.go — non-Windows platforms have no OS-boundary hook:
// the chord recorder falls back to DOM events (see BackendCapture.ts).
// The App methods exist so the frontend contract is identical — they
// report the fallback honestly instead of arming a session that could
// never report keys.

import "errors"

// errNoHookCapture names the honest refusal: without a hook there is
// no capture session to arm (the page records through DOM events).
var errNoHookCapture = errors.New("key capture hook unavailable on this platform")

func (a *App) StartKeyCapture(token string) (string, error) {
	return "", errNoHookCapture
}

func (a *App) StopKeyCapture(token string) error  { return nil }
func (a *App) RenewKeyCapture(token string) error { return nil }

// exitHookThread is a no-op without the Windows hook thread.
func exitHookThread() {}
