// Package selfupdate knows how to fetch and apply kvmshare releases
// from GitHub. It is shared by two consumers:
//
//   - kvmshare-install — the standalone installer/bootstrap binary you
//     download once; it fetches the full release archive for this
//     platform and installs it (and re-runs it to update).
//   - the GUI — bound methods let the user check for and apply updates
//     in place, then the GUI restarts into the new version.
//
// Everything is compiled Go — no shell scripts, no curl pipelines. The
// only external knowledge is the GitHub repository and the asset naming
// convention (see AssetName).
package selfupdate

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"time"
)

// Version is the build's version, injected at link time:
//
//	go build -ldflags "-X kvmshare/gui/internal/selfupdate.Version=v0.1.0"
//
// The default keeps un-tagged dev builds identifiable.
var Version = "v0.1.0-dev"

// DefaultUpstream is the GitHub repository releases are pulled from.
// Overridable with KVMSHARE_UPSTREAM (useful for forks).
const DefaultUpstream = "YELrhilassi/kvmshare"

// AssetSuffix is the platform part of release asset names.
func AssetSuffix() string {
	switch runtime.GOOS {
	case "windows":
		return "windows_amd64"
	default:
		return runtime.GOOS + "_" + runtime.GOARCH
	}
}

// AssetName returns the release archive name for this platform:
// kvmshare_<tag>_<os>_<arch>.tar.gz (zip on Windows).
func AssetName(tag string) string {
	ext := ".tar.gz"
	if runtime.GOOS == "windows" {
		ext = ".zip"
	}
	return fmt.Sprintf("kvmshare_%s_%s%s", tag, AssetSuffix(), ext)
}

// Binaries are the executables a release archive must contain (with the
// platform extension).
func Binaries() []string {
	ext := ""
	if runtime.GOOS == "windows" {
		ext = ".exe"
	}
	return []string{"kvmshare-gui" + ext, "kvmshare-server" + ext, "kvmshare-client" + ext, "kvmshare-install" + ext}
}

// Release is the subset of a GitHub release we care about.
type Release struct {
	Tag    string  `json:"tag_name"`
	Assets []Asset `json:"assets"`
}

// Asset is one downloadable file of a release.
type Asset struct {
	Name string `json:"name"`
	URL  string `json:"browser_download_url"`
	Size int64  `json:"size"`
}

// FetchRelease returns the newest published release from GitHub.
func FetchRelease(upstream string) (*Release, error) {
	return fetchRelease(upstream, "latest")
}

// FetchReleaseTag returns a specific release by tag (e.g. "v0.1.0").
func FetchReleaseTag(upstream, tag string) (*Release, error) {
	return fetchRelease(upstream, "tags/"+tag)
}

func fetchRelease(upstream, ref string) (*Release, error) {
	if upstream == "" {
		upstream = DefaultUpstream
	}
	url := fmt.Sprintf("https://api.github.com/repos/%s/releases/%s", upstream, ref)
	req, err := http.NewRequest(http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("User-Agent", "kvmshare-installer")
	client := &http.Client{Timeout: 15 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("reach GitHub: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("GitHub API %s", resp.Status)
	}
	var rel Release
	if err := json.NewDecoder(resp.Body).Decode(&rel); err != nil {
		return nil, fmt.Errorf("decode release: %w", err)
	}
	return &rel, nil
}

// AssetFor finds the archive for this platform in a release.
func (r *Release) AssetFor() (*Asset, error) {
	want := AssetName(r.Tag)
	for i := range r.Assets {
		if r.Assets[i].Name == want {
			return &r.Assets[i], nil
		}
	}
	return nil, fmt.Errorf("release %s has no asset %q (available: %s)",
		r.Tag, want, assetNames(r.Assets))
}

func assetNames(as []Asset) string {
	names := make([]string, 0, len(as))
	for _, a := range as {
		names = append(names, a.Name)
	}
	sort.Strings(names)
	return strings.Join(names, ", ")
}

// Download streams `url` into `dest` (a temp file the caller owns).
func Download(url, dest string) error {
	out, err := os.Create(dest)
	if err != nil {
		return err
	}
	defer out.Close()
	client := &http.Client{Timeout: 15 * time.Minute}
	resp, err := client.Get(url)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("download %s: %s", url, resp.Status)
	}
	if _, err := io.Copy(out, resp.Body); err != nil {
		return err
	}
	return out.Sync()
}

// FetchChecksums downloads the SHA256SUMS asset of a release.
func FetchChecksums(rel *Release) (map[string]string, error) {
	var asset *Asset
	for i := range rel.Assets {
		if rel.Assets[i].Name == "SHA256SUMS" {
			asset = &rel.Assets[i]
			break
		}
	}
	if asset == nil {
		return nil, fmt.Errorf("release %s has no SHA256SUMS (refusing to install unverified)", rel.Tag)
	}
	tmp, err := os.CreateTemp("", "kvmshare-sha256-*")
	if err != nil {
		return nil, err
	}
	defer os.Remove(tmp.Name())
	defer tmp.Close()
	if err := Download(asset.URL, tmp.Name()); err != nil {
		return nil, err
	}
	if _, err := tmp.Seek(0, io.SeekStart); err != nil {
		return nil, err
	}
	raw, err := io.ReadAll(tmp)
	if err != nil {
		return nil, err
	}
	sums := map[string]string{}
	for _, line := range strings.Split(string(raw), "\n") {
		fields := strings.Fields(line)
		if len(fields) == 2 {
			sums[fields[1]] = fields[0]
		}
	}
	return sums, nil
}

// Newer reports whether version `a` is newer than `b`. Tags are vX.Y.Z
// with optional suffixes; missing components count as 0, and a dev build
// ("vX.Y.Z-dev") is always older than the plain release so development
// machines still see published updates.
func Newer(a, b string) bool {
	if strings.HasSuffix(b, "-dev") {
		return true
	}
	return compareVersion(a, b) > 0
}

func compareVersion(a, b string) int {
	pa := parseVersion(a)
	pb := parseVersion(b)
	for i := 0; i < 3; i++ {
		if pa[i] != pb[i] {
			if pa[i] > pb[i] {
				return 1
			}
			return -1
		}
	}
	return 0
}

// parseVersion turns "v1.2.3-anything" into [1, 2, 3].
func parseVersion(v string) [3]int {
	var out [3]int
	trimmed := strings.TrimPrefix(strings.TrimSpace(v), "v")
	parts := strings.SplitN(trimmed, "-", 2)
	nums := strings.Split(parts[0], ".")
	for i := 0; i < 3 && i < len(nums); i++ {
		n := 0
		for _, ch := range nums[i] {
			if ch < '0' || ch > '9' {
				break
			}
			n = n*10 + int(ch-'0')
		}
		out[i] = n
	}
	return out
}

// ReplaceAt moves `src` over `dst` in place, surviving a running
// process: the old file is renamed aside first (renaming a running
// binary is allowed on Linux and Windows; deleting it is not).
func ReplaceAt(src, dst string) error {
	return replaceFile(src, dst)
}

// SHA256Of returns the hex sha256 of a file.
func SHA256Of(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// VerifyFile checks a file's sha256 against `expected`.
func VerifyFile(path, expected string) error {
	got, err := SHA256Of(path)
	if err != nil {
		return err
	}
	if !strings.EqualFold(got, expected) {
		return fmt.Errorf("checksum mismatch: want %s got %s", expected, got)
	}
	return nil
}

// Extract unpacks a release archive (tar.gz or zip) into `dest`. The
// archive has a versioned top directory; the binaries are found by name
// wherever they sit inside, and returned as dest -> source name pairs
// for the caller to place.
func Extract(archive, dest string) (map[string]string, error) {
	if err := os.MkdirAll(dest, 0o755); err != nil {
		return nil, err
	}
	found := map[string]string{}
	if strings.HasSuffix(archive, ".zip") {
		r, err := zip.OpenReader(archive)
		if err != nil {
			return nil, err
		}
		defer r.Close()
		for _, f := range r.File {
			if f.FileInfo().IsDir() {
				continue
			}
			name := filepath.Base(f.Name)
			if !isBinary(name) {
				continue
			}
			if err := extractZipEntry(f, filepath.Join(dest, name)); err != nil {
				return nil, err
			}
			found[name] = filepath.Join(dest, name)
		}
		return found, nil
	}

	f, err := os.Open(archive)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	gz, err := gzip.NewReader(f)
	if err != nil {
		return nil, err
	}
	defer gz.Close()
	tr := tar.NewReader(gz)
	for {
		hdr, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, err
		}
		if hdr.Typeflag != tar.TypeReg {
			continue
		}
		name := filepath.Base(hdr.Name)
		if !isBinary(name) {
			continue
		}
		out, err := os.OpenFile(filepath.Join(dest, name), os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o755)
		if err != nil {
			return nil, err
		}
		if _, err := io.Copy(out, tr); err != nil {
			out.Close()
			return nil, err
		}
		out.Close()
		found[name] = filepath.Join(dest, name)
	}
	return found, nil
}

func extractZipEntry(f *zip.File, dest string) error {
	rc, err := f.Open()
	if err != nil {
		return err
	}
	defer rc.Close()
	out, err := os.OpenFile(dest, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0o755)
	if err != nil {
		return err
	}
	defer out.Close()
	_, err = io.Copy(out, rc)
	return err
}

func isBinary(name string) bool {
	for _, b := range Binaries() {
		if name == b {
			return true
		}
	}
	return false
}
