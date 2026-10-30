package record

import (
	"syscall"
	"unsafe"
)

// Now is guest time: nanoseconds since boot (CLOCK_BOOTTIME). Under hydo it is a function of
// the virtual clock, so it is the same on every replay of a run.
func Now() int64 {
	var ts syscall.Timespec
	const clockBoottime = 7
	if _, _, e := syscall.Syscall(syscall.SYS_CLOCK_GETTIME, clockBoottime, uintptr(unsafe.Pointer(&ts)), 0); e != 0 {
		return 0
	}
	return ts.Nano()
}
