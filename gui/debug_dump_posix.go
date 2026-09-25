//go:build !windows

package main

// Process CPU time on POSIX (Linux, macOS): getrusage via os.Wait4's
// rusage is awkward for self, so use /proc on Linux and sysctl rusage
// through syscall.Getrusage everywhere POSIX — RUSAGE_SELF covers the
// whole process (all threads), which is exactly what the watchdog
// wants. Go's syscall package exposes Getrusage on all unix targets.

import "syscall"

// cpuPercent returns this process's total CPU time (user + sys, all
// threads) as a percentage of one core, measured since process start.
// The watchdog differences consecutive samples, so the reference point
// does not matter — only the delta.
func cpuPercent() float64 {
	var ru syscall.Rusage
	if err := syscall.Getrusage(syscall.RUSAGE_SELF, &ru); err != nil {
		return 0
	}
	used := timesec(ru.Utime) + timesec(ru.Stime)
	return used * 100 // seconds of CPU = percent-of-one-core since start
}

func timesec(tv syscall.Timeval) float64 {
	return float64(tv.Sec) + float64(tv.Usec)/1e6
}
