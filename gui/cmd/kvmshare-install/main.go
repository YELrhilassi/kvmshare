// kvmshare-install — the one file you download to get kvmshare on a
// machine (and the file the release installer runs to update it).
//
// It fetches the latest release archive for this platform from GitHub,
// verifies its checksum against the release's SHA256SUMS, extracts it,
// and installs the binaries (plus Linux desktop integration). Running it
// again updates everything in place.
//
//	kvmshare-install                 install the latest release
//	kvmshare-install --version v0.1.0   install a specific version
//	kvmshare-install --check         print the latest version and exit
//	kvmshare-install --uninstall     remove installed binaries
//
// No shell scripts, no curl pipelines: this binary is the installer.
package main

import (
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"kvmshare/gui/internal/selfupdate"
)

func main() {
	var (
		version   = flag.String("version", "", "install this exact version (default: latest)")
		check     = flag.Bool("check", false, "print the latest published version and exit")
		uninstall = flag.Bool("uninstall", false, "remove installed binaries")
	)
	flag.Parse()

	if *uninstall {
		if err := uninstallAll(); err != nil {
			fatal(err)
		}
		return
	}

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
	fmt.Printf("kvmshare-install: done — binaries in %s\n", selfupdate.InstallDir())
	fmt.Println("Launch with: kvmshare-gui")
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
	if removed == 0 {
		fmt.Println("kvmshare-install: nothing installed")
	}
	return nil
}

func fatal(err error) {
	fmt.Fprintf(os.Stderr, "kvmshare-install: %s\n", strings.TrimSpace(err.Error()))
	os.Exit(1)
}
