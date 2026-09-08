//go:build !windows

package main

import "syscall"

// setBroadcast enables SO_BROADCAST on a UDP socket so beacons can be
// sent to the subnet broadcast address.
func setBroadcast(fd uintptr) error {
	return syscall.SetsockoptInt(int(fd), syscall.SOL_SOCKET, syscall.SO_BROADCAST, 1)
}