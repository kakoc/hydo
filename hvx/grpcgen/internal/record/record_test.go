package record

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"hydogrpc/internal/rng"
)

func TestEncDec(t *testing.T) {
	r := rng.New(5)
	for _, s := range []string{"", "%", "a b", "{\"k\":\"v w\"}", "naïve\n\t", "100%"} {
		e := Enc(s)
		if strings.ContainsAny(e, " \n\t") || (s != "" && e == "%") {
			t.Fatalf("Enc(%q) = %q", s, e)
		}
		if d, err := Dec(e); err != nil || d != s {
			t.Fatalf("Dec(Enc(%q)) = %q, %v", s, d, err)
		}
	}
	for i := 0; i < 1000; i++ {
		b := make([]byte, r.Intn(40))
		for j := range b {
			b[j] = byte(r.Uint64())
		}
		if d, err := Dec(Enc(string(b))); err != nil || d != string(b) {
			t.Fatalf("round trip of %x", b)
		}
	}
	if _, err := Dec("%4"); err == nil {
		t.Fatal("truncated escape accepted")
	}
}

func TestRecorder(t *testing.T) {
	dir := t.TempDir()
	p := filepath.Join(dir, "calls")
	os.Setenv("HYDO_GRPC_CONSOLE", "off")
	rec := Open(p, "brief")
	rec.Invoke(3, 17, "pkg.S/M", `{"a":"b c"}`)
	rec.Complete(3, 17, "info", "pkg.S/M", "DEADLINE_EXCEEDED", "context deadline exceeded")
	rec.Complete(3, 18, "ok", "pkg.S/M", "OK", "")
	b, _ := os.ReadFile(p)
	lines := strings.Split(strings.TrimSpace(string(b)), "\n")
	if len(lines) != 3 {
		t.Fatalf("%d lines", len(lines))
	}
	e, ok := Parse("console junk " + lines[0])
	if !ok || e.Process != "3" || e.Op != "17" || e.Type != "invoke" || e.RPC != "pkg.S/M" || e.Body != `{"a":"b c"}` {
		t.Fatalf("invoke parsed as %+v", e)
	}
	e, ok = Parse(lines[1])
	if !ok || e.Type != "info" || e.Code != "DEADLINE_EXCEEDED" || e.Body != "context deadline exceeded" {
		t.Fatalf("completion parsed as %+v", e)
	}
	e, ok = Parse(lines[2])
	if !ok || e.Type != "ok" || e.Body != "" {
		t.Fatalf("empty body parsed as %+v", e)
	}
}
