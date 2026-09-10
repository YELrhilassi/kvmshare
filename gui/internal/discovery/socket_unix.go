//go:build !windows

package discovery

// Unix socket shim: SO_BROADCAST on the beacon socket's fd.

import (
	"syscall"
)

// setBroadcast enables SO_BROADCAST so beacons may target the limited
// broadcast address. Windows needs WSAIoctl instead (socket_windows.go).
func setBroadcast(fd uintptr) error {
	return syscall.SetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_BROADCAST, 1)
}
