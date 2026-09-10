//go:build !linux

package sessionbus

// Ensure is a no-op off Linux: session-bus ownership is a Linux concern: on Linux a missing bus makes
// godbus and WebKitGTK autolaunch a fresh private bus per launch, each
// growing an immortal dbus-activated stack. Windows and macOS provide
// their own notification/tray plumbing and have no such autolaunch trap,
// so there is nothing to ensure here.
func Ensure(stateDir string) func() {
	return func() {}
}
