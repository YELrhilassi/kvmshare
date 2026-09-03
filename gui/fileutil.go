package main

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

// atomicWriteFile writes data to path atomically (temp file + rename).
func atomicWriteFile(path string, data []byte, perm os.FileMode) error {
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
	if err := replaceFile(tmpName, path); err != nil {
		cleanup()
		return err
	}
	return nil
}

// replaceFile moves tmp over dst. os.Rename overwrites on Unix; Windows
// refuses to overwrite, so fall back to remove-then-rename there (the
// tiny gap is safe: readers keep the old content and retry).
func replaceFile(tmp, dst string) error {
	if err := os.Rename(tmp, dst); err == nil {
		return nil
	}
	if err := os.Remove(dst); err != nil && !os.IsNotExist(err) {
		return err
	}
	return os.Rename(tmp, dst)
}
