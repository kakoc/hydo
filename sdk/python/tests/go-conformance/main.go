// The Go half of the Python SDK's conformance test (sdk/python/tests/conformance.sh): one fixed
// sequence of SDK calls. tests/conformance.py makes the same calls with hydo_assert; the two
// consoles (HYDO_ASSERT_OUT) and stdouts must be byte-identical.
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
		"s":      "<>&\"\\\n\r\t\b\f\x01\x1f\x7f é     😀 日本語",
		"neg":    -5,
		"zero":   0,
		"umax":   uint64(math.MaxUint64),
		"imin":   int64(math.MinInt64),
		"imax":   int64(math.MaxInt64),
		"f1":     0.1,
		"fneg0":  math.Copysign(0, -1),
		"f0":     0.0,
		"f21":    1e21,
		"f20":    1e20,
		"fsm":    1.5e-7,
		"f6":     1e-6,
		"f6b":    9.99e-7,
		"fmix":   123456789.125,
		"f3":     3.0,
		"fneg":   -2.5,
		"ftiny":  2.5e-300,
		"fbig":   1e100,
		"fmaxf":  math.MaxFloat64,
		"fsub":   5e-324,
		"fthird": 1.0 / 3.0,
		"f1e15":  123456789012345680.0,
		"t":      true,
		"f":      false,
		"nil":    nil,
		"bytes":  []byte("hi\x00\xff"),
		"ebytes": []byte{},
		"list":   []interface{}{1, "a", false, nil, D{"z": 1, "a": 2}, []interface{}{[]interface{}{}}},
		"empty":  []interface{}{},
		"emap":   D{},
		"nest":   D{"é": 1, "B": 2, "a": 3, "_": 4, "a b": D{"x": "y"}, "😀": 5, "Z": 6, "ÿ": 7, "~": 8},
		"deep":   D{"l1": D{"l2": D{"l3": []interface{}{D{"k": "v", "j": []interface{}{1.25, -0.5}}}}}},
		"<key>":  "&",
	}
}

func main() {
	r0 := rich()
	if os.Getenv("HYDO_CONF_INVALID_UTF8") != "" { // Go <= 1.26 escapes invalid UTF-8 as \ufffd
		r0["inv"] = "a\xffb\xc3"
		r0["\xffk"] = D{"a\xfe": 1, "a": 2}
	}
	a := hydoassert.Always("c.always", "always <holds> & \"quotes\"")
	b := hydoassert.AlwaysOrUnreachable("c.aou", "always or unreachable")
	s := hydoassert.Sometimes("c.sometimes", "sometimes é   true")
	r := hydoassert.Reachable("c.reach", "reached")
	u := hydoassert.Unreachable("c.unreach", "never reached")
	hydoassert.Always("c.never", "never evaluated")
	cmp := hydoassert.Always("c.cmp", "a comparison")
	scmp := hydoassert.Sometimes("c.scmp", "a sometimes comparison")
	gt := hydoassert.Always("c.gt", "greater than")
	ge := hydoassert.Sometimes("c.ge", "greater than or equal")
	lt := hydoassert.Always("c.lt", "less than")
	le := hydoassert.Always("c.le", "less than or equal")
	nan := hydoassert.Always("c.nan", "details with a NaN")
	uni := hydoassert.Always("c.ünï/日本", "unicode id and \x01 control")
	loop := hydoassert.Always("c.loop", "evaluated many times")
	l1 := hydoassert.Eventually("c.live_ok", "in time", 10*time.Second)
	l2 := hydoassert.Eventually("c.live_late", "never", 0)
	l3 := hydoassert.Eventually("c.live_await", "await, true at once", 1500*time.Millisecond)
	var ps []*hydoassert.Point
	for _, id := range []string{"p.a", "p.b", "p.c", "p.d", "p.e", "p.f", "p.g", "p.h", "p.é"} {
		ps = append(ps, hydoassert.Buggify(id, "point "+strings.ToUpper(id)))
	}

	hydoassert.Catalog()

	a.Check(true, nil)
	a.Check(true, D{"x": 1})
	a.Check(false, r0)
	a.Check(false, D{"dup": true})
	b.Check(true, D{})
	b.Check(false, D{"why": nil})
	s.Check(false, nil)
	s.Check(true, D{"n": 3})
	s.Check(true, D{"n": 4})
	r.Hit(nil)
	r.Hit(D{"again": 1})
	u.Hit(D{"why": "test"})
	u.Hit(nil)
	cmp.Cmp(3, "<=", 5, nil)
	cmp.Cmp(7, "<", 5, D{"extra": "e", "left": "overwritten"})
	cmp.Cmp(1, "??", 1, nil)
	scmp.Cmp(1, ">", 2, nil)
	scmp.Cmp(2, "==", 2, nil)
	gt.Cmp(2, ">", 1, nil)
	gt.Cmp(1, ">", 2, D{"k": "v"})
	ge.Cmp(-1, ">=", 0, nil)
	ge.Cmp(0, ">=", 0, nil)
	lt.Cmp(math.MinInt64, "<", math.MaxInt64, nil)
	le.Cmp(5, "<=", 4, nil)
	le.Cmp(4, "!=", 4, nil)
	nan.Check(true, D{"f": math.NaN()})
	nan.Check(true, D{"f": 1})
	nan.Check(false, D{"nested": D{"f": math.Inf(1)}})
	nan.Check(false, D{"f": 2})
	uni.Check(false, D{"ключ": "значение", "\t": "\t"})
	for i := 0; i < 1000; i++ {
		loop.Check(i != 500, D{"i": i})
	}
	w := l1.Begin()
	w.Done(nil)
	w.Done(nil)
	l1.Await(func() bool { return true }, time.Millisecond, nil)
	w2 := l2.Begin()
	w2.Expire(D{"k": "v"})
	w2.Done(nil)
	l3.Await(func() bool { return true }, time.Millisecond, D{"probe": "ping"})
	hydoassert.SetupComplete()

	var fires strings.Builder
	for i := 0; i < 60; i++ {
		for j, p := range ps {
			var d D
			if j == 0 {
				d = D{"round": i}
			}
			if j == 1 {
				d = D{"hit": "replaced", "x": []interface{}{i}}
			}
			if p.Fire(d) {
				fires.WriteByte('1')
			} else {
				fires.WriteByte('0')
			}
		}
	}
	fmt.Println(fires.String())
	hydoassert.SetupComplete()
	os.Stdout.Sync()
}
