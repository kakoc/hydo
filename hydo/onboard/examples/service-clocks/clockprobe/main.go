// clockprobe prints its wall clock in milliseconds since the epoch, as Go's time.Now reads it.
// Built static (CGO_ENABLED=0), so it reads the clock through Go's own vDSO code or its own
// system call, never through a libc: the case that a libc preload cannot change.
//
//	clockprobe              one reading
//	clockprobe bench <n>    n readings; prints the guest time one reading took
package main

import (
	"fmt"
	"os"
	"strconv"
	"time"
)

func main() {
	if len(os.Args) == 3 && os.Args[1] == "bench" {
		n, err := strconv.Atoi(os.Args[2])
		if err != nil || n < 1 {
			fmt.Fprintln(os.Stderr, "usage: clockprobe bench <n>")
			os.Exit(2)
		}
		t0 := time.Now()
		var sum int64
		for i := 0; i < n; i++ {
			sum += time.Now().UnixNano() & 1
		}
		d := time.Since(t0) // the monotonic clock, which no own clock moves
		fmt.Printf("reads=%d ns_per_read=%d odd=%d\n", n, d.Nanoseconds()/int64(n), sum)
		return
	}
	fmt.Println(time.Now().UnixMilli())
}
