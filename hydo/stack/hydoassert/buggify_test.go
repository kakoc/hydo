package hydoassert

import (
	"os"
	"sync"
	"testing"
)

func resetBG(cfg string) {
	os.Setenv("HYDO_ASSERT_OUT", "off")
	os.Setenv("HYDO_BUGGIFY", cfg)
	os.Setenv("HYDO_BUGGIFY_FILE", "/nonexistent")
	bgOnce, bgOn, bgSeed, bgEnable, bgFire = sync.Once{}, false, 0, 250, 250
	for _, p := range points {
		p.hits = 0
	}
}

func trace(ps []*Point, n int) []bool {
	var out []bool
	for i := 0; i < n; i++ {
		for _, p := range ps {
			out = append(out, p.Fire(nil))
		}
	}
	return out
}

func TestBuggify(t *testing.T) {
	var ps []*Point
	for _, id := range []string{"a", "b", "c", "d", "e", "f", "g", "h"} {
		ps = append(ps, Buggify("t."+id, id))
	}
	resetBG("")
	for _, f := range trace(ps, 50) {
		if f {
			t.Fatal("a point fired with BUGGIFY off")
		}
	}
	resetBG("1234abcd")
	a := trace(ps, 200)
	resetBG("1234abcd")
	if b := trace(ps, 200); len(a) != len(b) {
		t.Fatal("length")
	} else {
		for i := range a {
			if a[i] != b[i] {
				t.Fatal("same seed, different decisions")
			}
		}
	}
	// rates over many seeds: about 25% of points enabled, about 25% of their hits fire
	en, fired, hits := 0, 0, 0
	for s := 1; s <= 400; s++ {
		resetBG(strconvHexForTest(uint64(s)))
		bgOnce.Do(bgLoad)
		for _, p := range ps {
			if p.enabled() {
				en++
				for i := 0; i < 20; i++ {
					hits++
					if p.Fire(nil) {
						fired++
					}
				}
			}
		}
	}
	pe := float64(en) / float64(400*len(ps))
	pf := float64(fired) / float64(hits)
	if pe < 0.2 || pe > 0.3 || pf < 0.2 || pf > 0.3 {
		t.Fatalf("rates off: enabled %.3f fire %.3f", pe, pf)
	}
	t.Logf("enabled %.3f, fire %.3f", pe, pf)
}

func strconvHexForTest(v uint64) string {
	const hx = "0123456789abcdef"
	if v == 0 {
		return "0"
	}
	var b []byte
	for v > 0 {
		b = append([]byte{hx[v%16]}, b...)
		v /= 16
	}
	return string(b)
}
