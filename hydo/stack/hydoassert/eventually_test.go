package hydoassert

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func captureTo(t *testing.T) string {
	p := filepath.Join(t.TempDir(), "console")
	os.WriteFile(p, nil, 0o644)
	os.Setenv("HYDO_ASSERT_OUT", p)
	outOnce, out, off = sync.Once{}, nil, false
	return p
}

func records(t *testing.T, p string) []evalRec {
	b, _ := os.ReadFile(p)
	var rs []evalRec
	for _, line := range strings.Split(string(b), "\n") {
		if !strings.HasPrefix(line, "HYDO-ASSERT {") {
			continue
		}
		var r evalRec
		if err := json.Unmarshal([]byte(strings.TrimPrefix(line, "HYDO-ASSERT ")), &r); err != nil {
			t.Fatal(err)
		}
		rs = append(rs, r)
	}
	return rs
}

func TestEventually(t *testing.T) {
	p := captureTo(t)
	inTime := Eventually("t.in_time", "becomes true in time", 200*time.Millisecond)
	late := Eventually("t.late", "never becomes true", 50*time.Millisecond)
	n := 0
	if !inTime.Await(func() bool { n++; return n >= 3 }, time.Millisecond, nil) {
		t.Fatal("in-time condition reported late")
	}
	if late.Await(func() bool { return false }, 10*time.Millisecond, D{"k": 1}) {
		t.Fatal("never-true condition reported in time")
	}
	w := Eventually("t.done_late", "true, but after the deadline", time.Millisecond).Begin()
	time.Sleep(5 * time.Millisecond)
	if w.Done(nil) {
		t.Fatal("late Done reported in time")
	}
	rs := records(t, p)
	if len(rs) != 3 {
		t.Fatalf("want 3 records, got %d", len(rs))
	}
	want := []struct {
		id string
		ok bool
	}{{"t.in_time", true}, {"t.late", false}, {"t.done_late", false}}
	for i, r := range rs {
		if r.T != TEventually || r.ID != want[i].id || r.OK != want[i].ok {
			t.Fatalf("record %d = %+v, want %v", i, r, want[i])
		}
		if _, ok := r.Details["elapsed_ms"]; !ok {
			t.Fatalf("record %d has no elapsed_ms", i)
		}
	}
	os.Setenv("HYDO_ASSERT_OUT", "off")
	outOnce, out, off = sync.Once{}, nil, false
}
