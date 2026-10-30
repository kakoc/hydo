//go:build linux

package hydohist

import (
	"syscall"
	"unsafe"
)

const clockMonotonic = 1

func monoNS() int64 {
	var ts syscall.Timespec
	if _, _, e := syscall.Syscall(syscall.SYS_CLOCK_GETTIME, clockMonotonic, uintptr(unsafe.Pointer(&ts)), 0); e != 0 {
		return -1
	}
	return ts.Nano()
}
