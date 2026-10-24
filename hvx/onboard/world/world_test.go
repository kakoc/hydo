package main

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestParseRule(t *testing.T) {
	r, err := parseRule(1, "payments.charge@3-6:status=429,retry-after=2,p=50")
	if err != nil {
		t.Fatal(err)
	}
	if r.Target != "payments" || r.Route != "charge" || r.Status != 429 || r.RetryAfter != "2" || r.P != 50 || r.applied() {
		t.Fatalf("bad rule %+v", r)
	}
	r, _ = parseRule(2, "pay@0-1:status=UNAVAILABLE,effect=yes")
	if r.Route != "" || r.Status != 14 || !r.applied() {
		t.Fatalf("bad grpc rule %+v", r)
	}
	r, _ = parseRule(3, "payments.charge@0-1:delay=3000")
	if !r.requestRule() || r.hookRule() || !r.applied() {
		t.Fatalf("delay: %+v", r)
	}
	r, _ = parseRule(4, "payments.charge@0-1:dup,late=500")
	if r.requestRule() || !r.hookRule() || !r.Dup || r.LateMs != 500 {
		t.Fatalf("hook rule: %+v", r)
	}
	for _, bad := range []string{"payments.charge@0-1", "payments@0-1:frob", "payments@0-1:delay=x", "@0-1:drop", "a@0-1:effect=maybe"} {
		if _, err := parseRule(9, bad); err == nil {
			t.Errorf("%q: no error", bad)
		}
	}
}

// the decision sequence is a pure function of the token: two rules from the same text agree, a
// different token gives a different sequence, and p=100 always fires
func TestFiresDeterministic(t *testing.T) {
	seq := func(tok string) string {
		r, _ := parseRule(1, tok)
		var b strings.Builder
		for i := 0; i < 64; i++ {
			fmt.Fprint(&b, map[bool]int{true: 1, false: 0}[r.fires()])
		}
		return b.String()
	}
	a, b, c := seq("x@0-1:drop,p=50"), seq("x@0-1:drop,p=50"), seq("x@1-2:drop,p=50")
	if a != b || a == c || !strings.Contains(a, "1") || !strings.Contains(a, "0") {
		t.Fatalf("a=%s b=%s c=%s", a, b, c)
	}
	if s := seq("x@0-1:drop"); strings.Contains(s, "0") {
		t.Fatalf("p=100 did not always fire: %s", s)
	}
}

func TestPbAndExtract(t *testing.T) {
	msg := []byte{0x08, 0x96, 0x01, 0x12, 2, 'o', '5'} // field 1 = 150 (varint), field 2 = "o5"
	if pbField(msg, 1) != "150" || pbField(msg, 2) != "o5" || pbField(msg, 3) != "" {
		t.Fatal("pbField")
	}
	v := &reqView{path: "/v1/orders/42/cancel", body: []byte(`{"order":"o7","n":{"amount":12.5}}`),
		query: map[string][]string{"x": {"y"}}, vars: map[string]string{"id": "42"}}
	for spec, want := range map[string]string{"json:order": "o7", "json:n.amount": "12.5", "path:id": "42",
		"path:4": "cancel", "query:x": "y", "": ""} {
		if got := v.extract(spec); got != want {
			t.Errorf("%s = %q, want %q", spec, got, want)
		}
	}
	if got := render(`{"id":"ch_{seq}","order":"{ref}","a":{json:n.amount}}`, map[string]string{"seq": "3", "ref": "o7"}, v); got != `{"id":"ch_3","order":"o7","a":12.5}` {
		t.Errorf("render: %s", got)
	}
	if esc("a b,c%") != "a%20b%2Cc%25" || unesc(esc("a b,c%")) != "a b,c%" || unesc(esc("")) != "" {
		t.Error("esc")
	}
}

func testWorld(t *testing.T) (*World, string) {
	dir := t.TempDir()
	cfg := &Config{Fakes: []*Endpoint{{Kind: "fake", Name: "payments", Routes: []*Route{
		{Name: "charge", Webhook: &Webhook{URL: "http://127.0.0.1:1/x", Retries: 1}}}}}}
	f, err := os.Create(filepath.Join(dir, "world.log"))
	if err != nil {
		t.Fatal(err)
	}
	return &World{cfg: cfg, logf: f, idem: map[string]int64{}, pending: map[int64]*hook{}, held: map[string]*hook{},
		healCh: make(chan struct{})}, dir
}

// a power cut carries effects, events and pending webhooks; the recovery world sees them, in order,
// and remembers the idempotency keys
func TestCarryRoundTrip(t *testing.T) {
	w, dir := testWorld(t)
	w.record("req", kv("ep", "payments"), kv("route", "charge"), kv("ref", "o 1"), kv("idem", "k1"), "fx=1", "out=ok")
	w.record("req", kv("ep", "payments"), kv("route", "charge"), kv("ref", "o2"), kv("idem", "k2"), "fx=0", "out=-")
	w.record("event", kv("ep", "-"), kv("route", "cancelled"), kv("ref", "o 1"), "fx=1")
	w.pending[1] = &hook{id: 1, ep: w.cfg.Fakes[0], rt: w.cfg.Fakes[0].Routes[0], ref: "o 1", idem: "k1", src: 1}
	out := w.carry()
	if !strings.HasSuffix(out, "END\n") || strings.Count(out, "w:") != 3 || strings.Contains(out, "o2") {
		t.Fatalf("carry: %q", out)
	}
	cf := filepath.Join(dir, "carry")
	os.WriteFile(cf, []byte(strings.TrimSuffix(out, "END\n")), 0644)
	w2, _ := testWorld(t)
	w2.frozen = true // no webhook delivery attempt in the test
	if err := w2.loadCarry(cf); err != nil {
		t.Fatal(err)
	}
	if w2.seq != 3 || w2.idem["payments/charge/k1"] != 1 || len(w2.pending) != 1 {
		t.Fatalf("recovery world: seq=%d idem=%v pending=%d", w2.seq, w2.idem, len(w2.pending))
	}
	b, _ := os.ReadFile(w2.logf.Name())
	if !strings.Contains(string(b), "ref=o%201") || !strings.Contains(string(b), "kind=event") {
		t.Fatalf("recovery log: %s", b)
	}
}

func TestCarryTooLong(t *testing.T) {
	w, _ := testWorld(t)
	for i := 0; i < 200; i++ {
		w.record("req", kv("ep", "payments"), kv("route", "charge"), kv("ref", fmt.Sprintf("order-%d", i)), "fx=1", "out=ok")
	}
	if out := w.carry(); out != "w:trunc\nEND\n" {
		t.Fatalf("carry of %d records: %d bytes, not truncated", 200, len(out))
	}
}

func TestChecks(t *testing.T) {
	_, dir := testWorld(t)
	log := filepath.Join(dir, "w.log")
	acked := filepath.Join(dir, "acked")
	os.WriteFile(log, []byte(strings.Join([]string{
		"seq=1 t=0 kind=req ep=payments route=charge ref=o1 idem=a fx=1 out=ok",
		"seq=2 t=0 kind=req ep=payments route=charge ref=o2 idem=b fx=1 out=ok",
		"seq=3 t=0 kind=event ep=- route=cancelled ref=o2 fx=1",
		"seq=4 t=0 kind=req ep=payments route=charge ref=o1 idem=a fx=0 out=replay",
		"seq=5 t=0 kind=req ep=payments route=charge ref=o2 idem=c fx=1 out=ok", ""}, "\n")), 0644)
	os.WriteFile(acked, []byte("1\n2\n3\nw:1,e,x,y,z,-\nclock=5\n"), 0644)
	t.Setenv("HYDO_WORLD_LOG", log)
	for _, c := range []struct {
		args []string
		rc   int
	}{
		{[]string{"at-most-once", "payments.charge"}, 1},                                         // o2 twice
		{[]string{"at-most-once", "payments.charge", "--by", "idem"}, 0},                         // keys a, b, c
		{[]string{"effect-for-acked", "payments.charge", "--ref", "o{id}", "--acked", acked}, 1}, // o3 missing
		{[]string{"none-after", "payments.charge", "--after", "event:cancelled"}, 1},             // seq 5 after 3
		{[]string{"none-after", "payments.charge"}, 2},
		{[]string{"count", "payments.charge"}, 0},
	} {
		if rc := checkMain(c.args); rc != c.rc {
			t.Errorf("%v: rc %d, want %d", c.args, rc, c.rc)
		}
	}
	t.Setenv("HYDO_WORLD_LOG", filepath.Join(dir, "missing"))
	if rc := checkMain([]string{"count", "payments.charge"}); rc != 2 {
		t.Errorf("no log: rc %d", rc)
	}
}
