// kvmshare-install — the one file you download to get kvmshare on a
// machine from a terminal or script (the GUI installer is a Wails window
// with the same engine behind it — see gui/installer).
//
// It fetches the latest release archive for this platform from GitHub,
// verifies its checksum against the release's SHA256SUMS, extracts it,
// and installs the binaries plus per-platform desktop integration. This
// CLI keeps every verb for scripting; the GUI installer covers the same
// ground interactively.
//
//	kvmshare-install                 install the latest release
//	kvmshare-install --version v0.1.0   install a specific version
//	kvmshare-install --local ./out     install from a local directory
//	kvmshare-install --check         print the latest version and exit
//	kvmshare-install --uninstall     remove installed binaries + shortcuts
//
// No shell scripts, no curl pipelines: this binary is the installer.
package main

import (
	"flag"
	"fmt"
	"os"
	"runtime"
	"strings"

	"kvmshare/gui/internal/installer"
	"kvmshare/gui/internal/selfupdate"
)

func main() {
	var (
		version     = flag.String("version", "", "install this exact version (default: latest)")
		local       = flag.String("local", "", "install from a local directory of built binaries (no network)")
		check       = flag.Bool("check", false, "print the latest published version and exit")
		uninstall   = flag.Bool("uninstall", false, "remove installed binaries + shortcuts")
		inputAccess = flag.Bool("input-access", false, "(Linux) grant input-device access; run as root via pkexec")
	)
	flag.Parse()

	// Privileged subcommand: invoked by this same binary through pkexec
	// after a normal install (see platform_linux.go on Linux). Grant-
	// only-if-missing so callers (install, update, GUI startup, make
	// install) can invoke it unconditionally without ever re-prompting
	// once granted.
	if *inputAccess {
		if err := installer.EnsureInputAccess(); err != nil {
			fatal(err)
		}
		return
	}

	if *uninstall {
		// On Windows the uninstall restores the UAC prompt policy (an
		// HKLM write), so the whole uninstall re-runs elevated — one
		// consent prompt, exactly like installing. Other platforms have
		// no privileged step and run in place.
		if !installer.IsElevated() {
			if err := installer.SelfElevate([]string{"--uninstall"}); err != nil {
				fatal(err)
			}
			return
		}
		if err := installer.Uninstall(printf); err != nil {
			fatal(err)
		}
		return
	}

	dir := selfupdate.InstallDir()

	if *local != "" {
		if err := installer.InstallLocal(*local, printf); err != nil {
			fatal(err)
		}
	} else {
		rel, err := installer.Check(os.Getenv("KVMSHARE_UPSTREAM"))
		if err != nil {
			fatal(err)
		}
		if *check {
			fmt.Printf("%s\n", rel.Tag)
			return
		}

		tag := rel.Tag
		if *version != "" {
			tag = *version
			fmt.Printf("kvmshare-install: installing %s (latest is %s)\n", tag, rel.Tag)
		} else {
			fmt.Printf("kvmshare-install: installing %s\n", tag)
		}
		if err := installer.Install(installer.Options{
			Tag:      tag,
			Upstream: os.Getenv("KVMSHARE_UPSTREAM"),
			Log:      printf,
		}); err != nil {
			fatal(err)
		}
	}

	if err := installer.Integrate(dir, printf); err != nil {
		fmt.Printf("kvmshare-install: warning: %v\n", err)
	}
	fmt.Printf("kvmshare-install: done — binaries in %s\n", dir)
	if runtime.GOOS == "windows" {
		fmt.Println("Launching kvmshare-gui...")
		if err := installer.Launch(dir); err != nil {
			fmt.Printf("kvmshare-install: launch failed: %v (start it manually)\n", err)
		}
	} else {
		fmt.Println("Launch with: kvmshare-gui")
	}
}

func printf(format string, args ...any) {
	fmt.Printf("kvmshare-install: "+format+"\n", args...)
}

func fatal(err error) {
	fmt.Fprintf(os.Stderr, "kvmshare-install: %s\n", strings.TrimSpace(err.Error()))
	os.Exit(1)
}
