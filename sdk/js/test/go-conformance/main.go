// The Go half of the JavaScript SDK's conformance test (sdk/js/test/conformance.sh): one fixed
// sequence of SDK calls. test/conformance.js makes the same calls with hydo-assert; the two
// consoles (HYDO_ASSERT_OUT) and stdouts must be byte-identical. Every Go value here has a
// JavaScript counterpart (no float32; uint64/int64 extremes are bigints on the JS side).
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
		"s":      "<>&\"\\\n\r\t\b\f\x01\x1f\x7f é     😀 ￿ 中文 ok",
		"bad":    "a\xffb",
		"neg":    -5,
		"zero":   0,
		"umax":   uint64(math.MaxUint64),
		"imin":   int64(math.MinInt64),
		"i53":    int64(9007199254740993),
		"f1":     0.1,
		"fneg0":  math.Copysign(0, -1),
		"f21":    1e21,
		"fn21":   -1e21,
		"f20":    1e20,
		"f64":    float64(1 << 64),
		"fsm":    1.5e-7,
		"fsm2":   9.99e-7,
		"f6":     1e-6,
		"f6b":    1.234e-6,
		"fmix":   123456789.125,
		"f3":     3.0,
		"ftiny":  2.5e-300,
		"fsub":   5e-324,
		"fmax":   math.MaxFloat64,
		"fbig":   1e100,
		"fthird": 1.0 / 3.0,
		"t":      true,
		"f":      false,
		"nil":    nil,
		"bytes":  []byte{0, 1, 2, 250, 255},
		"hello":  []byte("hello"),
		"ints":   []int32{1, -2, 3},
		"list":   []interface{}{1, "a", false, nil, D{"z": 1, "a": 2}, []interface{}{}},
		"empty":  []interface{}{},
		"emap":   D{},
		"nest": D{"é": 1, "B": 2, "a": 3, "_": 4, "a b": D{"x": "y"}, "￿": 5, "😀": 6, "": 7,
			"Z": D{"deep": []interface{}{D{"k": "<v>"}}}},
	}
}

func main() {
	a := hydoassert.Always("c.always", "always <holds> & \"quotes\"")
	b := hydoassert.AlwaysOrUnreachable("c.aou", "always or unreachable")
	s := hydoassert.Sometimes("c.sometimes", "sometimes é   true 😀")
	r := hydoassert.Reachable("c.reach", "reached")
	u := hydoassert.Unreachable("c.unreach", "never reached")
	hydoassert.Always("c.never", "never evaluated")
	cmp := hydoassert.Always("c.cmp", "a comparison")
	scmp := hydoassert.Sometimes("c.scmp", "a sometimes comparison")
	nan := hydoassert.Always("c.nan", "details with a NaN")
	uni := hydoassert.Always("c.ünï", "ünïcödé id \x01")
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
	nan.Check(false, D{"f": math.Inf(-1)})
	uni.Check(true, D{"k": "v"})
	w := l1.Begin()
	w.Done(nil)
	w.Done(nil)
	l1.Await(func() bool { return true }, time.Millisecond, nil)
	w2 := l2.Begin()
	w2.Expire(D{"k": "v"})
	hydoassert.SetupComplete()

	var fires strings.Builder
	for i := 0; i < 50; i++ {
		shared := D{"round": i, "stmt": "x"} // passed to two points: the first one's "hit" is seen by the second
		for j, p := range ps {
			var d D
			switch j {
			case 0:
				d = D{"round": i}
			case 1, 2:
				d = shared
			}
			if p.Fire(d) {
				fires.WriteByte('1')
			} else {
				fires.WriteByte('0')
			}
		}
	}
	hydoassert.Catalog() // a second catalog writes the same lines again
	fmt.Println(fires.String())
	os.Stdout.Sync()
}
