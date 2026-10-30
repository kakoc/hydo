// The Go half of the Python history recorder's conformance test
// (sdk/python/tests/history_conformance.sh): one fixed sequence of hydohist calls.
// tests/history_conformance.py makes the same calls with hydo_assert.history; the records
// (HYDO_HIST_OUT, with the time field masked) and the stdouts must be byte-identical.
//
// The first part is the JavaScript SDK's sequence (sdk/js/test/history-go/main.go). Where the
// Python side passes a lone surrogate outside U+DC80..U+DCFF, this side passes U+FFFD; the last
// part records invalid UTF-8, which Python passes as bytes or as surrogateescape strings.
package main

import (
	"fmt"
	"math"
	"strconv"
	"strings"

	"conf/hydohist"
)

func main() {
	kv := hydohist.New("kv")
	la := hydohist.New("la")
	other := hydohist.New("Other.hist_2-x")

	var ascii strings.Builder
	for c := 0; c < 0x80; c++ {
		ascii.WriteByte(byte(c))
	}
	vals := []string{
		"", "nil", "Nil", "nil ", " nil", "nilnil", "%", "%6Eil", "@", " ", "\t", "\n", "\r",
		"\x00", "\x01", "\x1f", "\x7f", ",", "[", "]", ":", "a,b[c]:d@e%f\tg", "a b",
		"plain-Token_1.2", "~!#$^&*()_+-={}|\\;'\"<>?/`", "é", "café au lait", "中文",
		"日本語,テスト", "😀", "a😀b c", "\u00a0", "\u2028\u2029", "\uFFFD", "\uFFFF",
		"a\uFFFDb",     // Python: 'a\ud800b'
		"\uFFFD\uFFFD", // Python: '\udc00\ud83d'
		"42",           // Python: 42
		"-7",           // Python: -7
		ascii.String(),
	}
	for i, v := range vals {
		fmt.Println(hydohist.Enc(v))
		k := "k" + strconv.Itoa(i)
		op := kv.Invoke(i%3, "write", k, hydohist.Enc(v))
		switch i % 4 {
		case 0:
			op.Ok()
		case 1:
			op.Ok(k, hydohist.Ver(v, int64(i)*1000003))
		case 2:
			op.Fail()
		default:
			op.Info()
		}
	}

	vers := []int64{0, 1, -1, 83, 0, 9007199254740992, math.MaxInt64, math.MinInt64, 9007199254740993, -9007199254740993}
	for i, n := range vers {
		fmt.Println(hydohist.Ver("v", n))
		op := kv.Invoke(10+i, "write", "kv", "v")
		op.Ok("kv", hydohist.Ver("v", n))
	}
	fmt.Println(hydohist.Ver("", 5), hydohist.Ver("nil", -5), hydohist.Ver("a@b", 7), hydohist.Ver("12", 12))

	von := []string{
		hydohist.ValOrNil("", true), hydohist.ValOrNil("", false), hydohist.ValOrNil("nil", true),
		hydohist.ValOrNil("x y", true), hydohist.ValOrNil("x", false), hydohist.ValOrNil("é", true),
		hydohist.ValOrNil("", false), hydohist.ValOrNil("", false), hydohist.ValOrNil("v", true),
	}
	fmt.Println(strings.Join(von, " "))
	op := kv.Invoke(20, "read", "k1", "k2", "k3")
	op.Ok("k1", von[0], "k2", von[1], "k3", hydohist.Nil)
	op = kv.Invoke(20, "read", "k4")
	op.Ok("k4", hydohist.ValOrNil("x y", true))

	lists := []string{
		hydohist.List(), hydohist.List(""), hydohist.List("a"),
		hydohist.List("a", "b,c", "[x]", "nil", "", "é", "1 2", "%", ":"),
		hydohist.List("1", "2", "3"),
	}
	fmt.Println(strings.Join(lists, " "))
	la.Invoke(1, "append", "x", "1").Ok()
	la.Invoke(1, "read", "x").Ok("x", hydohist.List("1"))
	la.Invoke(2, "read", "empty").Ok("empty", hydohist.List())
	la.Invoke(2, "read", "odd").Ok("odd", lists[3])

	mops := []string{
		hydohist.Append("x", "2"), hydohist.Read("y"), hydohist.Append("k:1", "e,2"), hydohist.Read(""),
		hydohist.ReadResult("y"), hydohist.ReadResult("", "a", "b c"), hydohist.ReadResult("nil", "nil"),
		hydohist.Append("", ""), hydohist.Read("@"),
	}
	fmt.Println(strings.Join(mops, " "))
	tx := la.Invoke(3, "txn", mops[0], mops[1], mops[2], mops[3])
	tx.Ok(mops[0], mops[4], mops[2], mops[5])
	tx = la.Invoke(3, "txn", hydohist.Append("x", "3"))
	tx.Fail()
	tx = la.Invoke(4, "txn", hydohist.Read("x"), hydohist.Append("y", "1"))
	tx.Info()

	other.Record(7, "invoke", "cas", "k", "a", "b")
	other.Record(7, "ok", "cas", "k", "a", "b@9")
	other.Record(8, "info", "read")
	other.Record(0, "fail", "write", "", "x", "")

	op = kv.Invoke(9, "write", "", "")
	op.Ok("", "")
	op = kv.Invoke(9, "write", "", "")
	op.Ok()

	op = kv.Invoke(5, "noop")
	op.Ok()
	op = kv.Invoke(5, "noop")
	op.Fail()
	op = kv.Invoke(5, "noop")
	op.Info()
	kv.Invoke(1<<40, "write", "big", "1").Ok()
	kv.Invoke(-5, "write", "neg", "1").Ok()
	kv.Invoke(math.MaxInt64, "write", "max", "1").Info()
	kv.Invoke(math.MinInt64, "write", "min", "1").Fail()

	a := kv.Invoke(30, "write", "i", "1")
	b := la.Invoke(30, "append", "i", "1")
	c := other.Invoke(31, "read", "i")
	b.Ok()
	c.Ok("i", "1")
	a.Info()

	other.Invoke(40, "write", "x\uFFFD", "é").Ok() // Python: 'x\udc00'
	other.Invoke(41, "write", "n", "42").Ok("n", "42", "-7")
	fmt.Println(hydohist.Nil)

	// Invalid UTF-8 (Python: bytes, bytearray, memoryview, or surrogateescape str).
	bad := []string{"\xff", "a\xffb", "\x80", "\xc3", "\xc3(", "\xed\xa0\x80", "\xe2\x82",
		"é\xff中", "\xff", "\uFFFD", "\xff\xfe nil", "nil"}
	for i, v := range bad {
		fmt.Println(hydohist.Enc(v), hydohist.Ver(v, int64(-i)), hydohist.ValOrNil(v, true))
		op := kv.Invoke(50+i, "write", "b", hydohist.Enc(v))
		op.Ok("b", hydohist.Ver(v, int64(i)))
	}
	fmt.Println(hydohist.List("\xff", "ok", "\xc3"), hydohist.Append("\xff", "\x80"), hydohist.ReadResult("\xfe", "\xff"))
	kv.Invoke(60, "write", "\xffraw", "x").Ok()
	kv.Invoke(61, "write", "k", "\xc3\xa9\xff").Ok("k", hydohist.List("\xff", "ok", "\xc3"))
	other.Record(62, "ok", "read", "\xe2\x82", "")
}
