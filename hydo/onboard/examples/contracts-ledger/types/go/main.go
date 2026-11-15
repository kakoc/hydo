// types: the values function contracts read in a Go program (docs/design/contracts.md, "Reading
// values"): strings, slices, results after the first, and an error. `types run <n>` calls each
// function n times with values the contracts know, and `types bad` breaks clauses once.
// hydo-contracts' tests compile this program's plan; the example's hydo-types-go.yaml runs it.
package main

import (
	"errors"
	"fmt"
	"os"
	"strconv"
)

// Pt is a point.
type Pt struct {
	X uint64
	Y int32
}

var sink []any

// text reads strings and slices: their lengths and their first elements.
// hydo: requires len(s) == 5 && s == "hello" && s[4] == 111
// hydo: requires len(v) == 4 && v[0] == 10 && v[3] == 40 && v[k] == 10 * (k + 1)
// hydo: requires len(ps) == 1 && ps[0].X == 1 && ps[0].Y == -2
// hydo: ensures result == len(s) + len(v)
//
//go:noinline
func text(s string, v []uint32, ps []Pt, k int) int {
	sink = append(sink[:0], s, v, ps, k)
	return len(s) + len(v)
}

// split returns three results: a string, a count and an error.
// hydo: requires len(s) == 5
// hydo: ensures result.0 == "hel" && len(result.0) == 3 && result.1 == len(s) - 3
// hydo: ensures result.2 == nil && err == nil && n == 2
//
//go:noinline
func split(s string, at int) (head string, n int, err error) {
	if at > len(s) {
		return "", 0, errors.New("too far")
	}
	return s[:at], len(s) - at, nil
}

// fail returns an error as its second result.
// hydo: ensures result.0 == 0 && result.1 != nil
//
//go:noinline
func fail(code int) (int, error) {
	if code != 0 {
		return 0, fmt.Errorf("code %d", code)
	}
	return 1, nil
}

func round(bad bool) {
	s := "hello"
	if bad {
		s = "hellO"
	}
	v := []uint32{10, 20, 30, 40}
	if bad {
		v[3] = 41
	}
	ps := []Pt{{X: 1, Y: -2}}
	sink = append(sink, text(s, v, ps, 2))
	at := 3
	if bad {
		at = 2
	}
	h, n, err := split(s, at)
	sink = append(sink, h, n, err)
	code := 7
	if bad {
		code = 0
	}
	r, err := fail(code)
	sink = append(sink, r, err)
}

func main() {
	if len(os.Args) == 3 && os.Args[1] == "run" {
		n, _ := strconv.Atoi(os.Args[2])
		for i := 0; i < n; i++ {
			round(false)
		}
		fmt.Printf("types: %d rounds\n", n)
		return
	}
	if len(os.Args) == 2 && os.Args[1] == "bad" {
		round(true)
		fmt.Println("types: one round with broken clauses")
		return
	}
	fmt.Fprintln(os.Stderr, "usage: types run <n> | types bad")
	os.Exit(2)
}
