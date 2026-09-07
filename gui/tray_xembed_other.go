//go:build !linux

package main

// Non-Linux stub for the XEmbed fallback. XEmbed is an X11 tray protocol,
// so it only exists on Linux; Windows and macOS always have a native tray
// and take the SNI path (tray.go), making this file's surface a no-op.

import "github.com/wailsapp/wails/v3/pkg/application"

// xembedManagerPresent always reports false off-Linux (the backend is
// chosen before this could matter, and detectTrayHost short-circuits on
// runtime.GOOS != "linux").
func xembedManagerPresent() bool { return false }

// setupXEmbedTray is never called off-Linux; it panics loudly if that
// ever changes, instead of failing silently.
func setupXEmbedTray(app *application.App, core *App, win *application.WebviewWindow) {
	panic("tray: XEmbed fallback is Linux-only")
}
