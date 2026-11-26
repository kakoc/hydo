// This file exists only in the public module (sdk/go), as messages.go does.

package hydohist

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// The recorder opens its file once per process, so the record is written by this test program
// run again with HYDO_HIST_OUT set and only this function selected.
func TestDrainedHelper(t *testing.T) {
	if os.Getenv("HYDOHIST_DRAINED_HELPER") != "1" {
		t.Skip("run by TestDrainedWritesTheSharedCasesLine")
	}
	New("q").Drained(9, "orders", "-")
}

// Drained writes the `drained` line of the shared case sdk/cases/history/messages, which the
// Rust SDK's Messages recorder writes with q.drained(9, "orders", "-").
func TestDrainedWritesTheSharedCasesLine(t *testing.T) {
	p := filepath.Join(t.TempDir(), "hist")
	c := exec.Command(os.Args[0], "-test.run=^TestDrainedHelper$")
	c.Env = append(os.Environ(), "HYDOHIST_DRAINED_HELPER=1", "HYDO_HIST_OUT="+p)
	if out, err := c.CombinedOutput(); err != nil {
		t.Fatalf("%v: %s", err, out)
	}
	got, err := os.ReadFile(p)
	if err != nil {
		t.Fatal(err)
	}
	want, err := os.ReadFile(filepath.Join("..", "..", "cases", "history", "messages.out"))
	if err != nil {
		t.Fatal(err)
	}
	norm := func(l string) string {
		f := strings.Fields(l)
		f[4] = "*" // the time
		return strings.Join(f, " ")
	}
	var line string
	for _, l := range strings.Split(strings.TrimSpace(string(want)), "\n") {
		if strings.Contains(l, " drained ") {
			line = l
		}
	}
	if line == "" {
		t.Fatal("the shared case has no drained line")
	}
	if g := norm(strings.TrimSpace(string(got))); g != norm(line) {
		t.Errorf("got %q want %q", g, norm(line))
	}
	if g := itemEnc("a;b=c d"); g != "a%3Bb%3Dc%20d" {
		t.Errorf("itemEnc: %q", g)
	}
}
