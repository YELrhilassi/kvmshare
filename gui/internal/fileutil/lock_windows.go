//go:build windows

package fileutil

// Windows lock primitives for WriteLocked: LockFileEx over the whole
// file, blocking (no FAIL_IMMEDIATELY). Mirrors the Rust side's fs2
// lock on the same `<config>.lock` file — the two are the same kernel
// byte-range lock, so GUI and server serialize for real.

import (
	"os"

	"golang.org/x/sys/windows"
)

// syscallLock takes an exclusive whole-file byte-range lock, blocking.
func syscallLock(f *os.File) error {
	ol := new(windows.Overlapped)
	// Lock the entire 64-bit address range; LockFileEx blocks until the
	// holder releases (no FAIL_IMMEDIATELY flag).
	return windows.LockFileEx(
		windows.Handle(f.Fd()),
		windows.LOCKFILE_EXCLUSIVE_LOCK,
		0, 0xFFFFFFFF, 0xFFFFFFFF, ol,
	)
}

// syscallUnlock releases the whole-file lock.
func syscallUnlock(f *os.File) error {
	ol := new(windows.Overlapped)
	return windows.UnlockFileEx(windows.Handle(f.Fd()), 0, 0xFFFFFFFF, 0xFFFFFFFF, ol)
}
