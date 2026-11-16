//go:build linux

package main

import (
	"syscall"
	"unsafe"
)

// clockGettime reads CLOCK_MONOTONIC (1).
func clockGettime(ts *syscall.Timespec) error {
	if _, _, e := syscall.Syscall(syscall.SYS_CLOCK_GETTIME, 1, uintptr(unsafe.Pointer(ts)), 0); e != 0 {
		return e
	}
	return nil
}
