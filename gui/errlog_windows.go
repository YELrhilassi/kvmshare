//go:build !unix

package main

// Non-unix stub: stderr capture is currently a Linux/macOS diagnostic.
// Windows builds keep the inherited stderr (the GUI is a windowed
// binary there; its stderr goes nowhere anyway).

func redirectStderr(path string) {
	_ = path // nothing to do on this platform yet
}
