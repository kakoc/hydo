// Package hydoassert is hydo's assertion SDK for Go guest programs: the way a program running
// under hvx DESCRIBES ITS INVARIANTS (the contract is hvx/ASSERTIONS.md).
//
// Each assertion has a stable id and a type:
//
//	Always              cond must hold every time it is evaluated (a false is a violation)
//	AlwaysOrUnreachable as Always, but never being evaluated is also fine
//	Sometimes           cond must be true at least once across an exploration (coverage)
//	Reachable           the line must be reached at least once across an exploration
//	Unreachable         the line must never be reached (reaching it is a violation)
//
// Declare assertions as package-level variables, so the whole catalog is known up front:
//
//	var ackedInMySQL = hydoassert.Always("order.acked_in_mysql", "an ACKed order is in MySQL with its amount")
//	...
//	ackedInMySQL.Check(got == want, hydoassert.D{"order": id, "got": got})
//
// Output goes straight to the guest console (/dev/ttyS0 by default), NOT to stdout, so it is
// seen even when the program's stdout is redirected to a log file. Wire format, one record
// per line, each written with a single write():
//
//	HYDO-ASSERT-DECL {"t":"always","id":"order.acked_in_mysql","msg":"..."}
//	HYDO-ASSERT {"t":"always","id":"order.acked_in_mysql","ok":false,"msg":"...","details":{...}}
//
// To keep the console small (hvx keeps at most 64 KiB of it per VM), a process emits each
// (assertion, outcome) pair only the FIRST time it happens, and never emits a Sometimes that
// evaluated false (it only matters that one evaluation somewhere was true). Determinism: the
// output is a pure function of the evaluation sequence -- no clocks, no host state, catalog
// in declaration order, details encoded by encoding/json (which sorts map keys).
package hydoassert

import (
	"encoding/json"
	"os"
	"strconv"
	"strings"
	"sync"
	"syscall"
)

// Type is an assertion type as it appears on the wire.
type Type string

const (
	TAlways              Type = "always"
	TAlwaysOrUnreachable Type = "always_or_unreachable"
	TSometimes           Type = "sometimes"
	TReachable           Type = "reachable"
	TUnreachable         Type = "unreachable"
)

// D is the free-form details object of an evaluation.
type D map[string]interface{}

// Assertion is one declared property.
type Assertion struct {
	t       Type
	id, msg string
	seen    [2]bool // outcome already emitted by this process: [false, true]
}

var (
	mu      sync.Mutex
	catalog []*Assertion
	out     *os.File
	outOnce sync.Once
	off     bool
)

func declare(t Type, id, msg string) *Assertion {
	a := &Assertion{t: t, id: id, msg: msg}
	mu.Lock()
	catalog = append(catalog, a)
	mu.Unlock()
	return a
}

// Always declares a property that must hold whenever it is evaluated.
func Always(id, msg string) *Assertion { return declare(TAlways, id, msg) }

// AlwaysOrUnreachable declares a property that must hold whenever evaluated; it is also fine
// for it never to be evaluated.
func AlwaysOrUnreachable(id, msg string) *Assertion { return declare(TAlwaysOrUnreachable, id, msg) }

// Sometimes declares a property that must be true at least once across an exploration.
func Sometimes(id, msg string) *Assertion { return declare(TSometimes, id, msg) }

// Reachable declares a code location that must be reached at least once.
func Reachable(id, msg string) *Assertion { return declare(TReachable, id, msg) }

// Unreachable declares a code location that must never be reached.
func Unreachable(id, msg string) *Assertion { return declare(TUnreachable, id, msg) }

// sink opens the console once. HYDO_ASSERT_OUT overrides the device (a path, "stderr", or
// "off"); if the console cannot be opened the records go to stderr.
func sink() *os.File {
	outOnce.Do(func() {
		p := os.Getenv("HYDO_ASSERT_OUT")
		switch p {
		case "off":
			off = true
			return
		case "stderr":
			out = os.Stderr
			return
		case "":
			p = "/dev/ttyS0"
		}
		f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|syscall.O_NOCTTY, 0)
		if err != nil {
			out = os.Stderr
			return
		}
		out = f
	})
	if off {
		return nil
	}
	return out
}

func emit(prefix string, rec interface{}) {
	b, err := json.Marshal(rec)
	if err != nil {
		return
	}
	f := sink()
	if f == nil {
		return
	}
	line := append([]byte(prefix+" "), b...)
	line = append(line, '\n')
	f.Write(line) // one write(): a record is never interleaved with other console output
}

type declRec struct {
	T   Type   `json:"t"`
	ID  string `json:"id"`
	Msg string `json:"msg"`
}

type evalRec struct {
	T       Type   `json:"t"`
	ID      string `json:"id"`
	OK      bool   `json:"ok"`
	Msg     string `json:"msg"`
	Details D      `json:"details,omitempty"`
}

// Catalog emits one HYDO-ASSERT-DECL line per declared assertion, in declaration order, so
// properties that are never evaluated can still be reported. Call it once, early, from the
// process that should carry the catalog into the console.
func Catalog() {
	mu.Lock()
	defer mu.Unlock()
	for _, a := range catalog {
		emit("HYDO-ASSERT-DECL", declRec{a.t, a.id, a.msg})
	}
}

func (a *Assertion) record(ok bool, d D) {
	mu.Lock()
	defer mu.Unlock()
	i := 0
	if ok {
		i = 1
	}
	if a.seen[i] {
		return
	}
	a.seen[i] = true
	emit("HYDO-ASSERT", evalRec{a.t, a.id, ok, a.msg, d})
}

// Check evaluates an Always / AlwaysOrUnreachable / Sometimes assertion. For Sometimes only a
// true outcome is recorded. It returns cond, so it can be used inline.
func (a *Assertion) Check(cond bool, d D) bool {
	if a.t == TSometimes && !cond {
		return cond
	}
	a.record(cond, d)
	return cond
}

// Hit marks a Reachable (recorded as ok=true) or Unreachable (recorded as ok=false: reaching
// it is a violation) location as reached.
func (a *Assertion) Hit(d D) {
	a.record(a.t != TUnreachable, d)
}

// Cmp is a numeric-comparison helper: it evaluates left <op> right (op one of
// "<", "<=", "==", "!=", ">=", ">") as a Check, with left/op/right in the details.
func (a *Assertion) Cmp(left int64, op string, right int64, d D) bool {
	var ok bool
	switch op {
	case "<":
		ok = left < right
	case "<=":
		ok = left <= right
	case "==":
		ok = left == right
	case "!=":
		ok = left != right
	case ">=":
		ok = left >= right
	case ">":
		ok = left > right
	}
	if d == nil {
		d = D{}
	}
	d["left"], d["op"], d["right"] = left, op, right
	return a.Check(ok, d)
}

// SetupComplete emits the lifecycle line HYDO-SETUP-COMPLETE: the system is provisioned and
// ready, faults may begin, and the point is a good snapshot/branch base.
func SetupComplete() {
	f := sink()
	if f != nil {
		f.Write([]byte("HYDO-SETUP-COMPLETE\n"))
	}
}

// ---- BUGGIFY: code-level fault points (FoundationDB style, Milestone 56) ----
//
// A fault point is a named place in the program where a real dependency can misbehave (a slow
// call, an error, a reply lost after the work was done). The program asks the point whether to
// misbehave right now:
//
//	var kafkaLostAck = hydoassert.Buggify("kafka.produce.lost_ack", "the record is written but the produce reports a timeout")
//	...
//	if kafkaLostAck.Fire(nil) { return errTimeout }
//
// Activation is two-level, as in FoundationDB (flow/include/flow/Buggify.h:
// P_BUGGIFIED_SECTION_ACTIVATED = 0.25, P_BUGGIFIED_SECTION_FIRES = 0.25): for a run, each point
// is ENABLED with probability enable/1000; an enabled point FIRES on each evaluation with
// probability fire/1000; a disabled point never fires. Both decisions are pure functions of the
// run's seed, the point's id and the evaluation's index at that point -- no clocks, no
// host state -- so a run stays a pure function of its schedule.
//
// Off unless configured: $HYDO_BUGGIFY, or else the file $HYDO_BUGGIFY_FILE (default
// /tmp/hydo-buggify), holds "<seed-hex>[,<enable-permille>[,<fire-permille>]]". It is read
// once, at the first evaluation (so a long-running server picks up a seed written after it
// started). Each point is declared as a Sometimes assertion "buggify.<id>", recorded when it
// first fires; at the first evaluation the process also writes one line
//
//	HYDO-BUGGIFY {"seed":"...","enable_pm":250,"fire_pm":250,"enabled":["id",...]}
//
// listing the enabled points (declaration order).

// Point is one declared fault point.
type Point struct {
	id   string
	a    *Assertion
	hits uint64
}

var (
	points   []*Point
	bgOnce   sync.Once
	bgOn     bool
	bgSeed   uint64
	bgEnable uint64 = 250
	bgFire   uint64 = 250
)

// Buggify declares a fault point.
func Buggify(id, msg string) *Point {
	p := &Point{id: id, a: declare(TSometimes, "buggify."+id, "fault point fired: "+msg)}
	mu.Lock()
	points = append(points, p)
	mu.Unlock()
	return p
}

func bgMix(z uint64) uint64 { // splitmix64 finalizer
	z += 0x9e3779b97f4a7c15
	z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
	z = (z ^ (z >> 27)) * 0x94d049bb133111eb
	return z ^ (z >> 31)
}

func bgKey(id string) uint64 { // FNV-1a
	h := uint64(14695981039346656037)
	for i := 0; i < len(id); i++ {
		h ^= uint64(id[i])
		h *= 1099511628211
	}
	return h
}

func (p *Point) enabled() bool { return bgMix(bgSeed^bgKey(p.id))%1000 < bgEnable }

type bgRec struct {
	Seed    string   `json:"seed"`
	Enable  uint64   `json:"enable_pm"`
	Fire    uint64   `json:"fire_pm"`
	Enabled []string `json:"enabled"`
}

// bgLoad reads the configuration "<seed-hex>[,<enable-permille>[,<fire-permille>]]".
func bgLoad() {
	cfg := os.Getenv("HYDO_BUGGIFY")
	if cfg == "" {
		f := os.Getenv("HYDO_BUGGIFY_FILE")
		if f == "" {
			f = "/tmp/hydo-buggify"
		}
		if b, err := os.ReadFile(f); err == nil {
			cfg = string(b)
		}
	}
	parts := strings.Split(strings.TrimSpace(cfg), ",")
	seed, err := strconv.ParseUint(parts[0], 16, 64)
	if err != nil {
		return // no configuration: every point stays off
	}
	bgOn, bgSeed = true, seed
	if len(parts) > 1 {
		if v, err := strconv.ParseUint(parts[1], 10, 64); err == nil {
			bgEnable = v
		}
	}
	if len(parts) > 2 {
		if v, err := strconv.ParseUint(parts[2], 10, 64); err == nil {
			bgFire = v
		}
	}
	rec := bgRec{Seed: strconv.FormatUint(bgSeed, 16), Enable: bgEnable, Fire: bgFire, Enabled: []string{}}
	for _, p := range points {
		if p.enabled() {
			rec.Enabled = append(rec.Enabled, p.id)
		}
	}
	emit("HYDO-BUGGIFY", rec)
}

// Fire reports whether the point misbehaves on this evaluation. Always false when BUGGIFY is
// not configured; then it has no other effect either.
func (p *Point) Fire(d D) bool {
	mu.Lock()
	bgOnce.Do(bgLoad)
	if !bgOn || !p.enabled() {
		mu.Unlock()
		return false
	}
	p.hits++
	k := p.hits
	fire := bgMix(bgMix(bgSeed^bgKey(p.id))+k*0x9e3779b97f4a7c15)%1000 < bgFire
	mu.Unlock()
	if fire {
		if d == nil {
			d = D{}
		}
		d["hit"] = k
		p.a.record(true, d)
	}
	return fire
}
