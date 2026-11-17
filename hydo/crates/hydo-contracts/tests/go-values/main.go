// The value history's readers (Milestone 285): a float64 field, a value of a
// map[string]interface{} by a constant key, an interface's dynamic value, and a string key. The
// test types.rs builds this program and compiles its `record` and `key` lines.
package main

import (
	"errors"
	"fmt"
	"os"
)

type txn struct {
	Ref   string
	Limit float64
	Meta  map[string]interface{}
	Any   interface{}
	Err   error
	Count map[int64]int64
}

// hydo: key t.Ref
// hydo: record t.Limit as limit
// hydo: record t.Meta["limit"] as meta.limit at return
// hydo: record t.Meta["allow"] as meta.allow at return
// hydo: record t.Any as any
// hydo: record t.Err as err at return
// hydo: record t.Count[7] as count.7 at entry
//
//go:noinline
func mark(t *txn) {
	t.Meta["allow"] = true
	if len(os.Args) > 2 {
		t.Meta["limit"] = t.Limit
		t.Err = errors.New("over")
	}
}

// hydo: requires t.Limit > 0.5
//
//go:noinline
func check(t *txn) bool {
	return t.Limit > 1
}

func main() {
	t := &txn{Ref: "d17", Limit: 50, Meta: map[string]interface{}{}, Any: 2.5, Count: map[int64]int64{7: 3}}
	mark(t)
	fmt.Println(check(t), t.Meta)
}
