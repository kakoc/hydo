// hydo Milestone 97: check dev_hvx.txt against hvx_abi.h. Every ioctl number encodes the
// size of its argument struct (bits 16..29); this compares that size with the size syzkaller
// computes for the pointee of each ioctl$HVX_* description, so a field missing from or added
// to a syzlang struct is caught at build time. build-syzkaller.sh copies this file to
// tools/hvx-check-sizes/main.go in the syzkaller tree and runs it after `make descriptions`.
package main

import (
	"fmt"
	"os"
	"strings"

	"github.com/google/syzkaller/prog"
	_ "github.com/google/syzkaller/sys"
)

func main() {
	target, err := prog.GetTarget("linux", "amd64")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	checked, bad := 0, 0
	for _, c := range target.Syscalls {
		if c.CallName != "ioctl" || !strings.HasPrefix(c.Name, "ioctl$HVX_") {
			continue
		}
		cmd := c.Args[1].Type.(*prog.ConstType).Val
		want := (cmd >> 16) & 0x3fff
		// HVX_GET_CONSOLE encodes a __u32 but transfers the whole 64 KiB struct
		if want == 0 || c.Name == "ioctl$HVX_GET_CONSOLE" {
			continue
		}
		pt, ok := c.Args[2].Type.(*prog.PtrType)
		if !ok {
			continue
		}
		got := pt.Elem.Size()
		checked++
		if got != want {
			fmt.Printf("SIZE MISMATCH %s: description %d bytes, hvx_abi.h %d bytes\n", c.Name, got, want)
			bad++
		}
	}
	fmt.Printf("hvx-check-sizes: %d ioctl argument sizes checked, %d mismatches\n", checked, bad)
	if bad != 0 || checked == 0 {
		os.Exit(1)
	}
}
