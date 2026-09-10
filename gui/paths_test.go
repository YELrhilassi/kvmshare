package main

import (
	"kvmshare/gui/internal/fileutil"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// Trusted-id entries accept the full id or its 8-char short form
// (prefix match), and never match on tiny prefixes.
func TestIdTrustedShortAndFull(t *testing.T) {
	full := "70b97d38631dda4b8f6ef627d753022d"
	short := full[:8]
	if !idTrusted([]string{full}, full) {
		t.Fatal("full id should match itself")
	}
	if !idTrusted([]string{short}, full) {
		t.Fatal("short id should match by prefix")
	}
	if idTrusted([]string{short}, "70b97dXXffffffffffffffffffffffffff") {
		t.Fatal("different id sharing only 4 chars must not match")
	}
	if idTrusted([]string{"ab"}, "abcdef") {
		t.Fatal("trusted entries shorter than 4 chars must never match")
	}
	if idTrusted([]string{""}, full) {
		t.Fatal("empty trusted entries must never match")
	}
	if shortID(full) != short {
		t.Fatalf("shortID(%q) = %q, want %q", full, shortID(full), short)
	}
}

// The default machine name is the real host name plus a short random
// suffix derived from the stable machine id — never an invented
// "pc"/"hp" placeholder, and stable across launches.
func TestDefaultClientNameIsHostnamePlusSuffix(t *testing.T) {
	a, _ := newTestApp(t)
	s := a.GetSettings()
	if s.ClientName == "" {
		t.Fatal("default client name should be set")
	}
	if !strings.Contains(s.ClientName, "-") {
		t.Fatalf("default client name %q should carry a hostname-suffix form", s.ClientName)
	}
	host, _ := os.Hostname()
	if host != "" && !strings.HasPrefix(s.ClientName, host) {
		t.Fatalf("default client name %q should start with the real host name %q", s.ClientName, host)
	}
	// Derived from the machine id, so a second App (or a restart) sees
	// the same default.
	a2 := NewApp()
	if a2.GetSettings().ClientName != s.ClientName {
		t.Fatalf("default client name changed between launches: %q vs %q", a2.GetSettings().ClientName, s.ClientName)
	}
}

func TestBinName(t *testing.T) {
	cases := []struct {
		base, goos, want string
	}{
		{"kvmshare-client", "windows", "kvmshare-client.exe"},
		{"kvmshare-server", "windows", "kvmshare-server.exe"},
		{"kvmshare-client", "linux", "kvmshare-client"},
		{"kvmshare-server", "darwin", "kvmshare-server"},
	}
	for _, c := range cases {
		if got := binName(c.base, c.goos); got != c.want {
			t.Errorf("binName(%q, %q) = %q, want %q", c.base, c.goos, got, c.want)
		}
	}
}

func TestListInterfaces(t *testing.T) {
	a, _ := newTestApp(t)
	ifaces, err := a.ListInterfaces()
	if err != nil {
		t.Fatal(err)
	}
	if len(ifaces) == 0 {
		t.Fatal("expected at least one interface")
	}
	found := false
	for _, ifc := range ifaces {
		if ifc.Name == "" {
			t.Fatal("interface with empty name")
		}
		for _, addr := range ifc.Addrs {
			if addr == "" {
				t.Fatal("interface with empty address")
			}
			found = true
		}
	}
	if !found {
		t.Fatal("expected at least one address")
	}
}

func TestAtomicWriteFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "kvmshare-server.toml")
	if err := fileutil.Write(path, []byte("port = 24800\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(path)
	if err != nil || string(raw) != "port = 24800\n" {
		t.Fatalf("content mismatch: %q err=%v", raw, err)
	}
	// Overwriting an existing file works and stays atomic.
	if err := fileutil.Write(path, []byte("port = 9999\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	raw, _ = os.ReadFile(path)
	if string(raw) != "port = 9999\n" {
		t.Fatalf("overwrite mismatch: %q", raw)
	}
	matches, _ := filepath.Glob(filepath.Join(filepath.Dir(path), ".kvmshare-*.tmp"))
	if len(matches) != 0 {
		t.Fatalf("stale temp files left: %v", matches)
	}
}
