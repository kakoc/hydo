//go:build !linux

package hydohist

import "time"

var t0 = time.Now()

func monoNS() int64 { return int64(time.Since(t0)) } // tests on a developer machine only
