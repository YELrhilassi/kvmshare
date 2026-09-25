//go:build windows

package main

// Process CPU time on Windows: GetProcessTimes (user + kernel, all
// threads of this process) via golang.org/x/sys/windows, which the GUI
// already depends on.

import (
	"time"

	"golang.org/x/sys/windows"
)

// cpuPercent returns this process's total CPU time (user + kernel) as a
// percentage of one core since process start; the watchdog differences
// consecutive samples, so only the delta matters.
func cpuPercent() float64 {
	h, err := windows.GetCurrentProcess()
	if err != nil {
		return 0
	}
	var creation, exit, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(h, &creation, &exit, &kernel, &user); err != nil {
		return 0
	}
	used := filetimeSeconds(kernel) + filetimeSeconds(user)
	return used * 100
}

func filetimeSeconds(ft windows.Filetime) float64 {
	return float64(ft.Nanoseconds()) / float64(time.Second)
}
