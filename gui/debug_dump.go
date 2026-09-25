package main

// debug_dump.go — the GUI's self-diagnosis. A goroutine spinning at full
// CPU (a loop missing its sleep, a wait that never blocks) is invisible
// from the outside: the process works, the UI works, the fan spins, and
// Task Manager shows the cost but never the cause. This watchdog
// samples the process's own CPU time; when it has burned roughly a
// whole core for several consecutive samples, it writes every
// goroutine's stack to `gui-cpudump.txt` in the state dir (overwritten
// each time, timestamped) — exactly the artifact needed to name the
// spinning loop, on any machine, without a debugger.
//
// The CPU reader is per-platform (`cpuPercent` in debug_dump_posix.go /
// debug_dump_windows.go): getrusage on Unix, GetProcessTimes on
// Windows — no third-party dependency for one number.
//
// Arms once via `armDebugWatchdog` from main (after startup), so boot
// bursts never false-trigger. Dumps are rate-limited to one per
// cooldown; the file is overwritten so the latest spin is the
// interesting one.

import (
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"strconv"
	"sync"
	"time"
)

// Sampling: ~100% of one core for 4 consecutive samples (~12 s) is a
// spin, not a burst. One dump per cooldown at most.
const (
	sampleEvery   = 3 * time.Second
	spinThreshold = 90.0 // percent of one core
	samplesToTrip = 4
	dumpCooldown  = time.Minute
)

var (
	dumpMu      sync.Mutex
	dumpArmed   bool
	dumpLastRun time.Time
)

// armDebugWatchdog starts the CPU watchdog once. Called from main after
// the application is built.
func armDebugWatchdog(a *App) {
	dumpMu.Lock()
	if dumpArmed {
		dumpMu.Unlock()
		return
	}
	dumpArmed = true
	dumpMu.Unlock()

	go func() {
		hot := 0
		lastCPU := cpuPercent()
		for {
			time.Sleep(sampleEvery)
			now := cpuPercent()
			// Percent of one core during this window; a spin sits at
			// ~100, idle at ~0. First sample establishes the baseline.
			pct := now - lastCPU
			lastCPU = now
			if pct >= spinThreshold {
				hot++
			} else {
				hot = 0
			}
			if hot >= samplesToTrip {
				hot = 0
				writeCPUDump(a, pct)
			}
		}
	}()
}

// writeCPUDump writes every goroutine's stack (debug=2: argument values
// and creation sites — the fastest route to the culprit), rate-limited.
func writeCPUDump(a *App, pct float64) {
	dumpMu.Lock()
	defer dumpMu.Unlock()
	if time.Since(dumpLastRun) < dumpCooldown {
		return
	}
	dumpLastRun = time.Now()

	path := filepath.Join(a.stateDir, "gui-cpudump.txt")
	f, err := os.Create(path)
	if err != nil {
		return
	}
	defer f.Close()

	now := time.Now()
	buf := make([]byte, 0, 64)
	buf = now.AppendFormat(buf, "2006-01-02 15:04:05")
	f.Write(buf)
	f.WriteString(" — CPU watchdog: process at " +
		strconv.FormatFloat(pct, 'f', 0, 64) + "% of one core for " +
		strconv.Itoa(samplesToTrip) + " samples\n")
	f.WriteString("goroutines: " + strconv.Itoa(runtime.NumGoroutine()) + "\n\n")
	_ = pprof.Lookup("goroutine").WriteTo(f, 2)
}
