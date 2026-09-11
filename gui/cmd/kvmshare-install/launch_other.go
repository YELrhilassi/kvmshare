//go:build !windows

package main

// The no-op counterpart of the Windows double-click handoff (see
// launch_windows.go): on other platforms the CLI installer IS the right
// face — a terminal launch keeps its terminal.

func doubleClicked() bool { return false }

func launchGUIInstaller() bool { return false }
