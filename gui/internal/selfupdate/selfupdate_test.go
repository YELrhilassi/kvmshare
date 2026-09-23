package selfupdate

import (
	"archive/tar"
	"compress/gzip"
	"os"
	"path/filepath"
	"runtime"
	"testing"
)

func TestNewer(t *testing.T) {
	cases := []struct {
		a, b string
		want bool
	}{
		{"v0.2.0", "v0.1.0", true},
		{"v0.1.0", "v0.1.0", false},
		{"v0.1.1", "v0.1.0", true},
		{"v1.0.0", "v0.9.9", true},
		{"v0.1.0", "v0.1.0-dev", true}, // dev builds always see updates
		{"v0.1.0-dev", "v0.1.0", false},
		{"v0.1.0", "v0.0.9", true},
		{"v0.1.0", "v0.1.0-rc1", true}, // a plain release beats its prerelease
		{"v0.1.0-rc1", "v0.1.0", false},
		{"v0.2.0-rc1", "v0.1.0", true},      // a newer prerelease is still newer
		{"v0.1.0-rc2", "v0.1.0-rc1", false}, // same-version prereleases do not auto-upgrade
		{"v0.4.0", "v0.0.0-dev", true},      // the Makefile's dev label sees releases
	}
	for _, c := range cases {
		if got := Newer(c.a, c.b); got != c.want {
			t.Errorf("Newer(%q, %q) = %v, want %v", c.a, c.b, got, c.want)
		}
	}
}

func TestParseVersion(t *testing.T) {
	cases := []struct {
		in   string
		want [3]int
	}{
		{"v0.1.0", [3]int{0, 1, 0}},
		{"v1.2.3-rc1", [3]int{1, 2, 3}},
		{"0.5", [3]int{0, 5, 0}},
		{"v10.20.30", [3]int{10, 20, 30}},
		{"garbage", [3]int{0, 0, 0}},
	}
	for _, c := range cases {
		if got := parseVersion(c.in); got != c.want {
			t.Errorf("parseVersion(%q) = %v, want %v", c.in, got, c.want)
		}
	}
}

func TestAssetNameMatchesPlatform(t *testing.T) {
	ext := ".tar.gz"
	if runtime.GOOS == "windows" {
		ext = ".zip"
	}
	want := "kvmshare_v0.1.0_" + runtime.GOOS + "_" + runtime.GOARCH + ext
	if got := AssetName("v0.1.0"); got != want {
		t.Errorf("AssetName = %q, want %q", got, want)
	}
}

func TestExtractFindsBinariesInNestedArchive(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "test.tar.gz")
	binaries := Binaries()

	// Build a realistic archive: a versioned top directory with the four
	// binaries (plus a README that must be ignored).
	f, err := os.Create(archive)
	if err != nil {
		t.Fatal(err)
	}
	gz := gzip.NewWriter(f)
	tw := tar.NewWriter(gz)
	prefix := "kvmshare_v0.1.0_linux_amd64/"
	writeEntry := func(name, content string, mode int64) {
		t.Helper()
		if err := tw.WriteHeader(&tar.Header{Name: prefix + name, Mode: mode, Size: int64(len(content))}); err != nil {
			t.Fatal(err)
		}
		if _, err := tw.Write([]byte(content)); err != nil {
			t.Fatal(err)
		}
	}
	for _, b := range binaries {
		writeEntry(b, "binary-bytes-"+b, 0o755)
	}
	writeEntry("README.md", "ignore me", 0o644)
	if err := tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	if err := f.Close(); err != nil {
		t.Fatal(err)
	}

	out := t.TempDir()
	found, err := Extract(archive, out)
	if err != nil {
		t.Fatal(err)
	}
	if len(found) != len(binaries) {
		t.Fatalf("extracted %d files, want %d (%v)", len(found), len(binaries), found)
	}
	for _, b := range binaries {
		src, ok := found[b]
		if !ok {
			t.Fatalf("missing binary %s in extraction", b)
		}
		got, err := os.ReadFile(src)
		if err != nil {
			t.Fatal(err)
		}
		if string(got) != "binary-bytes-"+b {
			t.Errorf("content mismatch for %s", b)
		}
	}
}

func TestParseChecksums(t *testing.T) {
	raw := "abc123  kvmshare_v0.1.0_linux_amd64.tar.gz\n" +
		"def456  ./kvmshare-install_v0.1.0_linux_amd64\n" +
		"\n" + // blank lines ignored
		"junk line without hash\n"
	sums := parseChecksums(raw)
	if sums["kvmshare_v0.1.0_linux_amd64.tar.gz"] != "abc123" {
		t.Errorf("plain entry not parsed: %v", sums)
	}
	// A "./" prefix must not break the lookup by bare name.
	if sums["kvmshare-install_v0.1.0_linux_amd64"] != "def456" {
		t.Errorf("./-prefixed entry not parsed: %v", sums)
	}
}

func TestVerifyFileChecksum(t *testing.T) {
	p := filepath.Join(t.TempDir(), "f")
	if err := os.WriteFile(p, []byte("hello kvmshare"), 0o644); err != nil {
		t.Fatal(err)
	}
	sum, err := SHA256Of(p)
	if err != nil {
		t.Fatal(err)
	}
	if err := VerifyFile(p, sum); err != nil {
		t.Errorf("VerifyFile should pass with matching checksum: %v", err)
	}
	if err := VerifyFile(p, "0000000000000000000000000000000000000000000000000000000000000000"); err == nil {
		t.Error("VerifyFile must fail on a mismatched checksum")
	}
}

func TestReplaceAtSurvivesTargetPresence(t *testing.T) {
	dir := t.TempDir()
	src := filepath.Join(dir, "new")
	dst := filepath.Join(dir, "dest")
	if err := os.WriteFile(src, []byte("new-binary"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(dst, []byte("old-binary"), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := ReplaceAt(src, dst); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(dst)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "new-binary" {
		t.Errorf("dst content = %q, want new-binary", got)
	}
	if _, err := os.Stat(dst + ".old"); !os.IsNotExist(err) {
		t.Error("the .old backup should be cleaned up")
	}
}

func TestReplaceSetAllOrNothing(t *testing.T) {
	newFile := func(dir, name, content string) string {
		t.Helper()
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte(content), 0o755); err != nil {
			t.Fatal(err)
		}
		return p
	}

	t.Run("success replaces all and cleans asides", func(t *testing.T) {
		dir := t.TempDir()
		aDst, bDst := filepath.Join(dir, "a"), filepath.Join(dir, "b")
		if err := os.WriteFile(aDst, []byte("old-a"), 0o755); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(bDst, []byte("old-b"), 0o755); err != nil {
			t.Fatal(err)
		}
		repl := map[string]string{
			aDst: newFile(t.TempDir(), "src-a", "new-a"),
			bDst: newFile(t.TempDir(), "src-b", "new-b"),
		}
		if err := ReplaceSet(repl); err != nil {
			t.Fatal(err)
		}
		for dst, want := range map[string]string{aDst: "new-a", bDst: "new-b"} {
			got, err := os.ReadFile(dst)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != want {
				t.Errorf("%s = %q, want %q", dst, got, want)
			}
		}
		if _, err := os.Stat(aDst + ".old"); !os.IsNotExist(err) {
			t.Error("a.old should be removed after success")
		}
	})

	t.Run("mid-failure restores every already-replaced target", func(t *testing.T) {
		dir := t.TempDir()
		aDst, bDst, cDst := filepath.Join(dir, "a"), filepath.Join(dir, "b"), filepath.Join(dir, "c")
		for _, d := range []string{aDst, bDst, cDst} {
			if err := os.WriteFile(d, []byte("old-"+filepath.Base(d)), 0o755); err != nil {
				t.Fatal(err)
			}
		}
		// The third source does not exist: moveInto fails on it, so the
		// failure lands after a and b were already replaced.
		repl := map[string]string{
			aDst: newFile(t.TempDir(), "src-a", "new-a"),
			bDst: newFile(t.TempDir(), "src-b", "new-b"),
			cDst: filepath.Join(t.TempDir(), "missing-src"),
		}
		if err := ReplaceSet(repl); err == nil {
			t.Fatal("ReplaceSet should fail on the un-moveable source")
		}
		for dst, want := range map[string]string{aDst: "old-a", bDst: "old-b", cDst: "old-c"} {
			got, err := os.ReadFile(dst)
			if err != nil {
				t.Fatal(err)
			}
			if string(got) != want {
				t.Errorf("%s = %q, want original %q", dst, got, want)
			}
		}
	})
}

// Dev builds must never be treated as released: they have no upstream
// archive, so self-update has to stay silent for them.
func TestIsRelease(t *testing.T) {
	for _, v := range []string{"", "v0.0.0-dev", "v1.0.0-dev", "v1.2.3-dev"} {
		if IsRelease(v) {
			t.Errorf("%q must not count as a release", v)
		}
	}
	for _, v := range []string{"v0.0.1", "v1.2.3", "v1.2.3-rc1"} {
		if !IsRelease(v) {
			t.Errorf("%q must count as a release", v)
		}
	}
}
