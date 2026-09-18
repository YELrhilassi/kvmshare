//go:build !windows

package fileutil

// Unix lock primitives for WriteLocked: BSD flock, blocking.

import (
	"os"
	"syscall"
)

// syscallLock takes an exclusive flock (blocking).
func syscallLock(f *os.File) error {
	return syscall.Flock(int(f.Fd()), syscall.LOCK_EX)
}

// syscallUnlock releases the lock.
func syscallUnlock(f *os.File) error {
	return syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
}
