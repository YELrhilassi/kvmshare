package fileutil

// Atomic file writes. The layout config is read by a live process (the
// Rust server's config watcher polls it) and by every GUI start — a
// half-written file would break both. Content is written to a temp file
// in the same directory, fsynced, then renamed over the target, so a
// crash at any point leaves either the old file or the new file, never
// a torn one.

import (
	"os"
	"path/filepath"
)

// Write writes data to path atomically (temp file + rename).
func Write(path string, data []byte, perm os.FileMode) error {
	return WriteLocked(path, data, perm, false)
}

// WriteLocked writes data to path atomically, optionally under the
// cross-process lock file `<path>.lock`. Config writes MUST use the
// lock: two writers exist (the GUI and the running server), both do
// read-modify-write of the whole file, and an unlocked interleaving
// loses one side's change (a trust edit vanishing — the user-visible
// flapping bug). Locking is blocking: writers are rare and quick, and
// the lock is released the moment the rename lands.
func WriteLocked(path string, data []byte, perm os.FileMode, lock bool) error {
	if !lock {
		return writeAtomic(path, data, perm)
	}
	lk, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR, 0o666)
	if err != nil {
		return err
	}
	defer lk.Close()
	if err := syscallLock(lk); err != nil {
		return err
	}
	defer syscallUnlock(lk)
	return writeAtomic(path, data, perm)
}

// writeAtomic is Write's body: temp file in the same directory, fsync,
// rename over the target — a crash at any point leaves either the old
// or the new content, never a torn one.
func writeAtomic(path string, data []byte, perm os.FileMode) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, ".kvmshare-*.tmp")
	if err != nil {
		return err
	}
	tmpName := tmp.Name()
	cleanup := func() {
		_ = os.Remove(tmpName)
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		cleanup()
		return err
	}
	if err := tmp.Sync(); err != nil {
		tmp.Close()
		cleanup()
		return err
	}
	if err := tmp.Close(); err != nil {
		cleanup()
		return err
	}
	if err := replace(tmpName, path); err != nil {
		cleanup()
		return err
	}
	return nil
}

// replaceFile moves tmp over dst. os.Rename overwrites on Unix; Windows
// refuses to overwrite, so fall back to remove-then-rename there (the
// tiny gap is safe: readers keep the old content and retry).
func replace(tmp, dst string) error {
	if err := os.Rename(tmp, dst); err == nil {
		return nil
	}
	if err := os.Remove(dst); err != nil && !os.IsNotExist(err) {
		return err
	}
	return os.Rename(tmp, dst)
}
