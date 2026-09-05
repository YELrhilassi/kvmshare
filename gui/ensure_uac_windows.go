//go:build windows

package main

// UAC consent prompts on Windows normally appear on the Winlogon secure
// desktop — a protected desktop that no process can inject input into,
// not even an elevated one. A KVM machine whose only mouse and keyboard
// is the shared stream therefore cannot answer them. The fix is to make
// prompts appear on the normal desktop instead (PromptOnSecureDesktop=0,
// restored on uninstall) — then the elevated client's SendInput reaches
// the prompt and the shared cursor can click it. This GUI is elevated
// (requireAdministrator), so the write always succeeds; the client's
// desktop watchdog remains as the safety net for locked workstations and
// systems where the policy is overridden.

import (
	"log/slog"

	"kvmshare/gui/internal/installer"
)

// ensureUacAnswerable makes UAC prompts answerable with the shared
// mouse/keyboard. Idempotent and best-effort: failures are logged, never
// fatal.
func (a *App) ensureUacAnswerable() {
	if err := installer.EnsureUacAnswerable(); err != nil {
		slog.Warn("uac: could not move UAC prompts to the normal desktop (secure-desktop fallback remains active)", "err", err)
		return
	}
	slog.Info("uac: UAC prompts open on the normal desktop, so the shared mouse and keyboard can answer them")
}