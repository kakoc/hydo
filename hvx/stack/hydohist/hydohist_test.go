package hydohist

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRecords(t *testing.T) {
	p := filepath.Join(t.TempDir(), "hist")
	os.Setenv("HYDO_HIST_OUT", p)
	kv := New("kv")
	op := kv.Invoke(3, "write", "k1", Enc("a b"))
	op.Ok("k1", Ver("a b", 83))
	op = kv.Invoke(3, "read", "k1")
	op.Ok("k1", ValOrNil("", false))
	op = kv.Invoke(4, "txn", Append("x", "1"), Read("y"))
	op.Ok(Append("x", "1"), ReadResult("y", "1", "2"))
	kv.Invoke(5, "write", "k2", "").Info()
	b, err := os.ReadFile(p)
	if err != nil {
		t.Fatal(err)
	}
	lines := strings.Split(strings.TrimSpace(string(b)), "\n")
	want := []string{
		"HYDO-HIST kv 3 invoke * write k1 a%20b",
		"HYDO-HIST kv 3 ok * write k1 a%20b@83",
		"HYDO-HIST kv 3 invoke * read k1",
		"HYDO-HIST kv 3 ok * read k1 nil",
		"HYDO-HIST kv 4 invoke * txn a:x:1 r:y",
		"HYDO-HIST kv 4 ok * txn a:x:1 r:y:[1,2]",
		"HYDO-HIST kv 5 invoke * write k2 %",
		"HYDO-HIST kv 5 info * write k2 %",
	}
	if len(lines) != len(want) {
		t.Fatalf("got %d lines: %q", len(lines), lines)
	}
	for i, l := range lines {
		f := strings.Fields(l)
		f[4] = "*" // the time
		if g := strings.Join(f, " "); g != want[i] {
			t.Errorf("line %d: got %q want %q", i, g, want[i])
		}
	}
}

func TestEnc(t *testing.T) {
	if g := Enc("a,b[c]:d@e%f\tg"); g != "a%2Cb%5Bc%5D%3Ad%40e%25f%09g" {
		t.Errorf("Enc: %q", g)
	}
}
