package main

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
)

// InterfaceInfo is one network interface with its addresses.
type InterfaceInfo struct {
	Name  string   `json:"name"`
	Addrs []string `json:"addrs"`
}

// ListInterfaces returns every network interface with its addresses.
func (a *App) ListInterfaces() ([]InterfaceInfo, error) {
	ifaces, err := net.Interfaces()
	if err != nil {
		return nil, fmt.Errorf("interfaces: %w", err)
	}
	out := make([]InterfaceInfo, 0, len(ifaces))
	for _, ifc := range ifaces {
		addrs, err := ifc.Addrs()
		if err != nil {
			continue
		}
		info := InterfaceInfo{Name: ifc.Name, Addrs: make([]string, 0, len(addrs))}
		for _, addr := range addrs {
			ip := strings.Split(addr.String(), "/")[0]
			if ip == "" || strings.HasPrefix(ip, "fe80:") {
				continue
			}
			info.Addrs = append(info.Addrs, ip)
		}
		if len(info.Addrs) > 0 || ifc.Flags&net.FlagUp != 0 {
			out = append(out, info)
		}
	}
	return out, nil
}

// TailLog returns the last `lines` lines of the given log file. The path
// comes from GetPaths; it is validated to live under the state dir.
func (a *App) TailLog(path string, lines int) (string, error) {
	a.mu.Lock()
	stateDir := filepath.Dir(a.serverLogPath)
	a.mu.Unlock()

	if !strings.HasPrefix(filepath.Clean(path), filepath.Clean(stateDir)) {
		return "", fmt.Errorf("refusing to read outside the log directory")
	}
	if lines <= 0 || lines > 2000 {
		lines = 200
	}

	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", err
	}
	defer f.Close()

	// Read the tail of the file: start at most 64 KiB before the end.
	st, err := f.Stat()
	if err != nil {
		return "", err
	}
	start := st.Size() - 64*1024
	if start < 0 {
		start = 0
	}
	if _, err := f.Seek(start, 0); err != nil {
		return "", err
	}

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 256*1024)
	var ring []string
	for sc.Scan() {
		line := sc.Text()
		ring = append(ring, line)
		if len(ring) > lines {
			ring = ring[len(ring)-lines:]
		}
	}
	if err := sc.Err(); err != nil {
		return "", err
	}
	return strings.Join(ring, "\n"), nil
}
