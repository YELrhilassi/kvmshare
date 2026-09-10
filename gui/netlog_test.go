package main

import (
	"os"
	"path/filepath"
	"testing"
)

func TestTailLog(t *testing.T) {
	a, _ := newTestApp(t)

	// Missing log → empty string, no error.
	stateDir := filepath.Dir(a.serverLogPath)
	out, err := a.TailLog(filepath.Join(stateDir, "nope.log"), 10)
	if err != nil || out != "" {
		t.Fatalf("missing log: out=%q err=%v", out, err)
	}

	// Real file with more lines than requested → last N only.
	logFile := filepath.Join(stateDir, "x.log")
	var content string
	for i := 0; i < 20; i++ {
		content += "line " + string(rune('A'+i)) + "\n"
	}
	if err := os.WriteFile(logFile, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	out, err = a.TailLog(logFile, 3)
	if err != nil {
		t.Fatal(err)
	}
	if out != "line R\nline S\nline T" {
		t.Fatalf("tail mismatch: %q", out)
	}

	// Path outside the state dir → refused.
	if _, err := a.TailLog("/etc/passwd", 10); err == nil {
		t.Fatal("expected refusal to read outside log dir")
	}
}
