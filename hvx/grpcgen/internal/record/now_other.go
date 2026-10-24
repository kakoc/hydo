//go:build !linux

package record

import "time"

var start = time.Now()

// Now is nanoseconds since the process started (only Linux, the guest, has CLOCK_BOOTTIME).
func Now() int64 { return int64(time.Since(start)) }
