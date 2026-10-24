// The Go half of the Rust SDK's conformance test (sdk/rust/tests/conformance.sh): one fixed
// sequence of SDK calls. examples/conformance.rs makes the same calls with hydo-assert; the
// two consoles (HYDO_ASSERT_OUT) and stdouts must be byte-identical.
package main

import (
	"fmt"
	"math"
	"os"
	"strings"
	"time"

	"conf/hydoassert"
)

type D = hydoassert.D

func rich() D {
	return D{
		"s":     "<>&\"\\\n\r\t\b\f\x01\x1f\x7f é     😀",
		"neg":   -5,
		"umax":  uint64(math.MaxUint64),
		"imin":  int64(math.MinInt64),
		"f1":    0.1,
		"fneg0": math.Copysign(0, -1),
		"f21":   1e21,
		"f20":   1e20,
		"fsm":   1.5e-7,
		"f6":    1e-6,
		"fmix":  123456789.125,
		"f3":    3.0,
		"ftiny": 2.5e-300,
		"fbig":  1e100,
		"f32a":  float32(0.1),
		"f32b":  float32(1e22),
		"f32c":  float32(3.4e-8),
		"t":     true,
		"nil":   nil,
		"list":  []interface{}{1, "a", false, nil, D{"z": 1, "a": 2}},
		"empty": []interface{}{},
		"emap":  D{},
		"nest":  D{"é": 1, "B": 2, "a": 3, "_": 4, "a b": D{"x": "y"}},
	}
}

func main() {
	a := hydoassert.Always("c.always", "always <holds> & \"quotes\"")
	b := hydoassert.AlwaysOrUnreachable("c.aou", "always or unreachable")
	s := hydoassert.Sometimes("c.sometimes", "sometimes é   true")
	r := hydoassert.Reachable("c.reach", "reached")
	u := hydoassert.Unreachable("c.unreach", "never reached")
	hydoassert.Always("c.never", "never evaluated")
	cmp := hydoassert.Always("c.cmp", "a comparison")
	scmp := hydoassert.Sometimes("c.scmp", "a sometimes comparison")
	nan := hydoassert.Always("c.nan", "details with a NaN")
	l1 := hydoassert.Eventually("c.live_ok", "in time", 10*time.Second)
	l2 := hydoassert.Eventually("c.live_late", "never", 0)
	var ps []*hydoassert.Point
	for _, id := range []string{"p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h"} {
		ps = append(ps, hydoassert.Buggify(id, "point "+strings.ToUpper(id)))
	}

	hydoassert.Catalog()

	a.Check(true, nil)
	a.Check(true, D{"x": 1})
	a.Check(false, rich())
	a.Check(false, D{"dup": true})
	b.Check(true, D{})
	s.Check(false, nil)
	s.Check(true, D{"n": 3})
	r.Hit(nil)
	r.Hit(nil)
	u.Hit(D{"why": "test"})
	cmp.Cmp(3, "<=", 5, nil)
	cmp.Cmp(7, "<", 5, D{"extra": "e"})
	cmp.Cmp(1, "??", 1, nil)
	scmp.Cmp(1, ">", 2, nil)
	scmp.Cmp(2, "==", 2, nil)
	nan.Check(true, D{"f": math.NaN()})
	nan.Check(true, D{"f": 1})
	nan.Check(false, D{"f": math.Inf(1)})
	w := l1.Begin()
	w.Done(nil)
	w.Done(nil)
	l1.Await(func() bool { return true }, time.Millisecond, nil)
	w2 := l2.Begin()
	w2.Expire(D{"k": "v"})
	hydoassert.SetupComplete()

	var fires strings.Builder
	for i := 0; i < 50; i++ {
		for j, p := range ps {
			var d D
			if j == 0 {
				d = D{"round": i}
			}
			if p.Fire(d) {
				fires.WriteByte('1')
			} else {
				fires.WriteByte('0')
			}
		}
	}
	fmt.Println(fires.String())
	os.Stdout.Sync()
}
