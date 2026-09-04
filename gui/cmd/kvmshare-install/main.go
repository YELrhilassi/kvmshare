// kvmshare-install — the one file you download to get kvmshare on a
// machine (and the file the release installer runs to update it).
//
// It fetches the latest release archive for this platform from GitHub,
// verifies its checksum against the release's SHA256SUMS, extracts it,
// and installs the binaries plus per-platform desktop integration (Linux:
// desktop entry + icon + sample config; Windows: Start Menu and desktop
// shortcuts, app icon, Add/Remove Programs entry). Running it again
// updates everything in place. A --local directory installs straight from
// a folder of built binaries, no network needed (used for testing).
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
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"

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
	// after a normal install (see integrate_linux.go).
	if *inputAccess {
		if os.Geteuid() != 0 {
			fmt.Fprintln(os.Stderr, "kvmshare-install: --input-access must run as root (pkexec does this automatically)")
			os.Exit(1)
		}
		if err := integrateInputAccess(); err != nil {
			fatal(err)
		}
		fmt.Println("kvmshare-install: input access granted")
		return
	}

	if *uninstall {
		if err := uninstallAll(); err != nil {
			fatal(err)
		}
		return
	}

	dir := selfupdate.InstallDir()

	if *local != "" {
		if err := installLocal(*local); err != nil {
			fatal(err)
		}
	} else {
		rel, err := selfupdate.FetchRelease(os.Getenv("KVMSHARE_UPSTREAM"))
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
		if err := installRelease(tag, rel); err != nil {
			fatal(err)
		}
	}

	if err := integrateDesktop(dir); err != nil {
		fmt.Printf("kvmshare-install: warning: %v\n", err)
	}
	fmt.Printf("kvmshare-install: done — binaries in %s\n", dir)
	if runtime.GOOS == "windows" {
		fmt.Println("Launching kvmshare-gui...")
		if err := launchGUI(dir); err != nil {
			fmt.Printf("kvmshare-install: launch failed: %v (start it manually)\n", err)
		}
	} else {
		fmt.Println("Launch with: kvmshare-gui")
	}
}

// installRelease downloads `tag`'s archive for this platform (reusing the
// `rel` metadata), verifies it, extracts and applies it.
func installRelease(tag string, rel *selfupdate.Release) error {
	tmp, err := os.MkdirTemp("", "kvmshare-install-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	// If the pinned tag is not the latest release we fetched, its
	// metadata (asset URLs) may differ — refetch by tag.
	if tag != rel.Tag {
		r2, err := selfupdate.FetchReleaseTag(os.Getenv("KVMSHARE_UPSTREAM"), tag)
		if err != nil {
			return err
		}
		rel = r2
	}

	asset, err := rel.AssetFor()
	if err != nil {
		return err
	}
	fmt.Printf("kvmshare-install: downloading %s (%d bytes)\n", asset.Name, asset.Size)
	archive := filepath.Join(tmp, asset.Name)
	if err := selfupdate.Download(asset.URL, archive); err != nil {
		return err
	}

	sums, err := selfupdate.FetchChecksums(rel)
	if err != nil {
		return err
	}
	expected, ok := sums[asset.Name]
	if !ok {
		return fmt.Errorf("SHA256SUMS has no entry for %s", asset.Name)
	}
	if err := selfupdate.VerifyFile(archive, expected); err != nil {
		return err
	}
	fmt.Println("kvmshare-install: checksum ok")

	extracted, err := selfupdate.Extract(archive, tmp)
	if err != nil {
		return err
	}
	written, err := selfupdate.Apply(extracted)
	if err != nil {
		return err
	}
	for _, p := range written {
		fmt.Printf("kvmshare-install:   %s\n", p)
	}
	return nil
}

func uninstallAll() error {
	dir := selfupdate.InstallDir()
	removed := 0
	for _, bin := range selfupdate.Binaries() {
		p := filepath.Join(dir, bin)
		if _, err := os.Stat(p); err == nil {
			if err := os.Remove(p); err != nil {
				return fmt.Errorf("remove %s: %w", p, err)
			}
			fmt.Printf("kvmshare-install: removed %s\n", p)
			removed++
		}
	}
	if err := removeDesktopIntegration(dir); err != nil {
		fmt.Printf("kvmshare-install: warning cleaning desktop integration: %v\n", err)
	}
	if removed == 0 {
		fmt.Println("kvmshare-install: nothing installed")
	}
	return nil
}

// installLocal installs from a directory that already contains the built
// binaries (e.g. a dist/ folder or a manually extracted archive). Used for
// testing before a release is published and for fully offline installs.
//
// The sources are staged as copies in a temp dir: Apply renames binaries
// into place (fast, atomic on Windows), and renaming would otherwise
// consume the user's source directory.
func installLocal(dir string) error {
	abs, err := filepath.Abs(dir)
	if err != nil {
		return err
	}
	st, err := os.Stat(abs)
	if err != nil {
		return err
	}
	if !st.IsDir() {
		return fmt.Errorf("--local %s is not a directory", abs)
	}

	tmp, err := os.MkdirTemp("", "kvmshare-local-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	extracted := make(map[string]string, len(selfupdate.Binaries()))
	for _, bin := range selfupdate.Binaries() {
		src := filepath.Join(abs, bin)
		if _, err := os.Stat(src); err != nil {
			return fmt.Errorf("--local dir is missing %s", bin)
		}
		staged := filepath.Join(tmp, bin)
		if err := copyFile(src, staged); err != nil {
			return fmt.Errorf("stage %s: %w", bin, err)
		}
		extracted[bin] = staged
	}
	fmt.Printf("kvmshare-install: installing from %s\n", abs)
	written, err := selfupdate.Apply(extracted)
	if err != nil {
		return err
	}
	for _, p := range written {
		fmt.Printf("kvmshare-install:   %s\n", p)
	}
	return nil
}

// copyFile copies src to dst (used to stage --local sources so the
// install's renames never touch the user's files).
func copyFile(src, dst string) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.Create(dst)
	if err != nil {
		return err
	}
	if _, err := io.Copy(out, in); err != nil {
		out.Close()
		return err
	}
	if err := out.Close(); err != nil {
		return err
	}
	return os.Chmod(dst, 0o755)
}

func fatal(err error) {
	fmt.Fprintf(os.Stderr, "kvmshare-install: %s\n", strings.TrimSpace(err.Error()))
	os.Exit(1)
}
