//go:build windows

package discovery

// Windows socket shim: SO_BROADCAST via WSAIoctl (there is no plain
// setsockopt for it on Windows).

import (
	"unsafe"

	"golang.org/x/sys/windows"
)

const sioBroadcast = 0x98000004 // IOC_OUT | IOC_IN, socket level, "BROAD"

// setBroadcast enables SO_BROADCAST on the beacon socket so beacons may
// target the limited broadcast address.
func setBroadcast(fd uintptr) error {
	val := int32(1)
	var ret uint32
	return windows.WSAIoctl(
		windows.Handle(fd), sioBroadcast,
		(*byte)(unsafe.Pointer(&val)), uint32(unsafe.Sizeof(val)),
		nil, 0,
		&ret, nil, 0,
	)
}
