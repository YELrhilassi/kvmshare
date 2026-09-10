package main

// paths.go — where everything lives: config, binaries, state files.
//
// Resolution rules, in priority order:
//   - an explicit KVMSHARE_* env var (the operator owns it)
//   - PATH (Unix convention)
//   - next to the executable (portable installs; the GUI and its role
//     binaries ship and upgrade together in one directory)
//
// The GUI does not pre-create the server's layout config: the server
// owns it and writes a machine-accurate default on first start, so GUI
// and server can never disagree about who wrote it first.

import (
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

// Paths reports where everything lives (config, logs, binaries).
type Paths struct {
	ConfigPath string `json:"configPath"`
	ServerLog  string `json:"serverLog"`
	ClientLog  string `json:"clientLog"`
	ServerBin  string `json:"serverBin"`
	ClientBin  string `json:"clientBin"`
}

// resolveBin locates a role binary: env var, then sibling of this
// executable, then PATH, then the portable fallback.
func resolveBin(envVar, name, dir string) string {
	if p := os.Getenv(envVar); p != "" {
		return p
	}
	return lookPathElse(name, filepath.Join(dir, binName(name, runtime.GOOS)))
}

// NewAppPaths resolves every file the GUI needs and creates the state
// directory. See the file comment for the resolution order.
func NewAppPaths() (stateDir, configPath, serverPath, clientPath, installPath string) {
	dir := executableDir()
	home, _ := os.UserHomeDir()

	// An explicit KVMSHARE_CONFIG wins as-is. Otherwise the canonical
	// per-user location is used.
	configPath = os.Getenv("KVMSHARE_CONFIG")
	if configPath == "" {
		if home != "" {
			configPath = filepath.Join(home, ".config", "kvmshare", "kvmshare-server.toml")
		} else {
			// No home at all (rare): fall back next to the executable.
			configPath = filepath.Join(dir, "kvmshare-server.toml")
		}
	}

	serverPath = resolveBin("KVMSHARE_SERVER", "kvmshare-server", dir)
	clientPath = resolveBin("KVMSHARE_CLIENT", "kvmshare-client", dir)
	// The standalone installer/bootstrap: kept current by the updater so
	// the portable update path never lags behind the GUI's.
	installPath = resolveBin("KVMSHARE_INSTALL", "kvmshare-install", dir)

	stateDir = filepath.Join(home, ".local", "state", "kvmshare")
	if home == "" {
		stateDir = dir
	}
	_ = os.MkdirAll(stateDir, 0o755)
	return
}

// GetPaths reports the resolved file locations.
func (a *App) GetPaths() Paths {
	a.mu.Lock()
	defer a.mu.Unlock()
	return Paths{
		ConfigPath: a.configPath,
		ServerLog:  a.serverLogPath,
		ClientLog:  a.clientLogPath,
		ServerBin:  a.serverPath,
		ClientBin:  a.clientPath,
	}
}

// lookPathElse finds `name`: sibling of this executable first (the GUI
// and its role binaries ship, and are upgraded, together in one install
// directory — searching PATH first let a stale copy from an older
// install silently win over the freshly deployed one), then PATH, then
// the fallback.
func lookPathElse(name, fallback string) string {
	if sib, err := os.Executable(); err == nil {
		sibling := filepath.Join(filepath.Dir(sib), binName(name, runtime.GOOS))
		if st, err := os.Stat(sibling); err == nil && !st.IsDir() {
			return sibling
		}
	}
	if p, err := exec.LookPath(name); err == nil {
		return p
	}
	return fallback
}

// binName returns the executable file name for `base` on `goos`:
// Windows binaries carry .exe, elsewhere they are bare. Used for the
// "next to the GUI" fallback, so an installed Windows GUI finds the
// role binaries installed beside it in %LOCALAPPDATA%\kvmshare.
func binName(base, goos string) string {
	if goos == "windows" {
		return base + ".exe"
	}
	return base
}

// hostnameOr returns the machine's host name, or the fallback when the
// OS refuses to name it.
func hostnameOr(fallback string) string {
	if h, err := os.Hostname(); err == nil && h != "" {
		return h
	}
	return fallback
}

// machineName is this machine's default friendly name: the real host
// name plus a short random suffix derived from the stable machine id
// ("bliss-8f3a"). The suffix keeps two machines with the same host
// name distinct on the network, and being derived from the persisted id
// it never changes between launches. Users can override it on the
// Client page (the "name on the server"); nothing else in the product
// invents "pc"/"hp"-style defaults.
func machineName(host, machineID string) string {
	h := strings.TrimSpace(host)
	if h == "" {
		h = "machine"
	}
	suffix := shortID(machineID)
	if len(suffix) > 4 {
		suffix = suffix[:4]
	}
	return h + "-" + suffix
}

// defaultClientName returns the name this machine presents to servers
// when the user has not chosen one yet.
func (a *App) defaultClientName() string {
	return machineName(hostnameOr("machine"), a.GetMachineId())
}

// displayName is the friendly name advertised on the network (beacons,
// pairing requests, mDNS): the user-chosen name when there is one,
// otherwise the generated machine name.
func (a *App) displayName() string {
	a.mu.Lock()
	defer a.mu.Unlock()
	if n := strings.TrimSpace(a.settings.ClientName); n != "" {
		return n
	}
	return a.defaultClientName()
}

func fileExists(p string) bool {
	_, err := os.Stat(p)
	return err == nil
}

func executableDir() string {
	exe, err := os.Executable()
	if err != nil {
		return "."
	}
	return filepath.Dir(exe)
}

// stateFilePath joins a file name into the state directory (the single
// place all shared state files live).
func (a *App) stateFilePath(name string) string {
	return filepath.Join(a.stateDir, name)
}
