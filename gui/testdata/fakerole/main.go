// Command fakerole mimics the role-lock contract of the real
// kvmshare-server / kvmshare-client binaries (see crates/app/src/guard.rs)
// so the GUI's process tests can exercise background discovery, adopt-on-
// start and stop-by-pid without building the Rust binaries.
//
// Role detection mirrors how the GUI launches the real binaries:
//
//	kvmshare-server --config <path>   → role "server"
//	kvmshare-client <addr> --name x   → role "client"
//
// It takes the role's flock (refusing when the other role holds its lock),
// records its pid in the lock file, then idles until signalled.
package main

import (
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
)

func stateDir() string {
	if p := os.Getenv("KVMSHARE_STATE"); p != "" {
		return p
	}
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return ".kvmshare-state"
	}
	return filepath.Join(home, ".local", "state", "kvmshare")
}

// lockFile opens (creating if needed) and flocks path, mirroring the Rust
// guard's non-blocking exclusive flock. Returns the open file on success.
func lockFile(path string) (*os.File, error) {
	f, err := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0o644)
	if err != nil {
		return nil, err
	}
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func main() {
	role := "client"
	if len(os.Args) >= 2 && os.Args[1] == "--config" {
		role = "server"
	}
	dir := stateDir()
	if err := os.MkdirAll(dir, 0o755); err != nil {
		fmt.Fprintln(os.Stderr, "fakerole: state dir:", err)
		os.Exit(2)
	}

	// Our own lock first, then make sure the other role is free — exactly
	// like the real guard.
	ours, err := lockFile(filepath.Join(dir, role+".lock"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "fakerole: another kvmshare", role, "is running")
		os.Exit(3)
	}
	other := "client"
	if role == "client" {
		other = "server"
	}
	probe, err := lockFile(filepath.Join(dir, other+".lock"))
	if err != nil {
		ours.Close()
		fmt.Fprintln(os.Stderr, "fakerole: a kvmshare", other, "is already running")
		os.Exit(3)
	}
	probe.Close()

	if err := ours.Truncate(0); err != nil {
		fmt.Fprintln(os.Stderr, "fakerole: truncate:", err)
		os.Exit(2)
	}
	if _, err := fmt.Fprintf(ours, "%d\n", os.Getpid()); err != nil {
		fmt.Fprintln(os.Stderr, "fakerole: write pid:", err)
		os.Exit(2)
	}

	// Idle until SIGTERM/SIGINT (the GUI stops roles by signalling the
	// pid recorded above).
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGTERM, syscall.SIGINT)
	<-sig
	ours.Close()
}
