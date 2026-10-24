// Package hydohist is hydo's operation-history recorder for Go guest programs: a workload
// client records every operation it performs -- the invocation before the request, the
// completion after the reply -- and the host checks the history after the run
// (linearizability of a register / CAS object per key, Elle-style list-append checks).
// Wire format and checkers: hvx/HISTORIES.md; manifests enable them with `checks:`.
//
//	kv := hydohist.New("kv")
//	op := kv.Invoke(3, "write", "k1", "v1")          // process 3, before the request
//	if err := put("k1", "v1"); err == nil {
//		op.Ok("k1", hydohist.Ver("v1", rev))         // the store's version, if it returns one
//	} else if definitelyNotApplied(err) {
//		op.Fail()                                     // e.g. rejected before it was sent
//	} else {
//		op.Info()                                     // a timeout: it may or may not have happened
//	}
//	op = kv.Invoke(3, "read", "k1")
//	v, ok := get("k1")
//	op.Ok("k1", hydohist.ValOrNil(v, ok))
//
// One record = one line = one write(), on the guest console (/dev/console; HYDO_HIST_OUT
// overrides it: a path, "stderr" or "off"). The host orders operations by the position of
// their records, so record the invocation before sending and the completion after receiving.
// A process has at most one operation in flight: give each concurrent client (goroutine) its
// own process number. The time field is CLOCK_MONOTONIC in ns (deterministic under hydo); the
// checkers do not order by it.
package hydohist

import (
	"os"
	"strconv"
	"strings"
	"sync"
	"syscall"
)

// Nil is the value of an absent key (register model) / an empty list.
const Nil = "nil"

// History is one named history (a check's `history:`).
type History struct{ name string }

// New returns the recorder for history `name` ([A-Za-z0-9_.-]+).
func New(name string) *History { return &History{name: name} }

// Op is an invoked operation waiting for its completion.
type Op struct {
	h    *History
	proc string
	f    string
	args []string
}

// Invoke records the invocation of f(args...) by process proc and returns the operation.
// args are tokens: encode free-form values with Enc (Ver, List and Nil help).
func (h *History) Invoke(proc int, f string, args ...string) *Op {
	o := &Op{h: h, proc: strconv.Itoa(proc), f: f, args: args}
	h.record(o.proc, "invoke", f, args)
	return o
}

// Ok records a successful completion. args are the operation's results in the same layout as
// the invocation (a read lists <key> <value> pairs, a txn its micro-ops with read results);
// with no args the invocation's are repeated.
func (o *Op) Ok(args ...string) {
	if len(args) == 0 {
		args = o.args
	}
	o.h.record(o.proc, "ok", o.f, args)
}

// Fail records that the operation definitely had no effect.
func (o *Op) Fail() { o.h.record(o.proc, "fail", o.f, o.args) }

// Info records that the outcome is unknown (it may take effect at any later time, or never).
func (o *Op) Info() { o.h.record(o.proc, "info", o.f, o.args) }

// Record writes one raw record (for programs that keep their own bookkeeping).
func (h *History) Record(proc int, typ, f string, args ...string) {
	h.record(strconv.Itoa(proc), typ, f, args)
}

// Enc makes a value one token: '%', whitespace, control characters, '@' and the list / txn
// separators ',[]:' become %xx; the empty string is the token "%".
func Enc(s string) string {
	if s == "" {
		return "%"
	}
	var b strings.Builder
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c == '%' || c == '@' || c <= ' ' || c == 0x7f || strings.IndexByte(",[]:", c) >= 0 {
			b.WriteString("%")
			b.WriteString(strings.ToUpper(strconv.FormatInt(int64(c)|0x100, 16)[1:]))
		} else {
			b.WriteByte(c)
		}
	}
	return b.String()
}

// Ver is value v (encoded) with the version the store assigned it: "v@83".
func Ver(v string, version int64) string { return Enc(v) + "@" + strconv.FormatInt(version, 10) }

// ValOrNil is Enc(v) if ok, else Nil.
func ValOrNil(v string, ok bool) string {
	if !ok {
		return Nil
	}
	return Enc(v)
}

// List is a list value "[e1,e2,...]" (list-append reads).
func List(elems ...string) string {
	e := make([]string, len(elems))
	for i, x := range elems {
		e[i] = Enc(x)
	}
	return "[" + strings.Join(e, ",") + "]"
}

// Append and Read are list-append txn micro-ops: "a:<k>:<e>", "r:<k>" (invocation) and
// "r:<k>:[...]" (completion, ReadResult).
func Append(k, e string) string             { return "a:" + Enc(k) + ":" + Enc(e) }
func Read(k string) string                  { return "r:" + Enc(k) }
func ReadResult(k string, l ...string) string { return "r:" + Enc(k) + ":" + List(l...) }

var (
	mu      sync.Mutex
	out     *os.File
	outOnce sync.Once
	off     bool
)

func sink() *os.File {
	outOnce.Do(func() {
		p := os.Getenv("HYDO_HIST_OUT")
		switch p {
		case "off":
			off = true
			return
		case "stderr":
			out = os.Stderr
			return
		case "":
			p = "/dev/console"
		}
		f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|os.O_CREATE|syscall.O_NOCTTY, 0644)
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

func (h *History) record(proc, typ, f string, args []string) {
	f0 := sink()
	if f0 == nil {
		return
	}
	var b strings.Builder
	b.WriteString("HYDO-HIST ")
	b.WriteString(h.name)
	b.WriteByte(' ')
	b.WriteString(proc)
	b.WriteByte(' ')
	b.WriteString(typ)
	b.WriteByte(' ')
	if t := monoNS(); t >= 0 {
		b.WriteString(strconv.FormatInt(t, 10))
	} else {
		b.WriteByte('-')
	}
	b.WriteByte(' ')
	b.WriteString(f)
	for _, a := range args {
		b.WriteByte(' ')
		if a == "" {
			a = "%" // the empty string (Enc("") gives it too)
		}
		b.WriteString(a)
	}
	b.WriteByte('\n')
	mu.Lock()
	f0.Write([]byte(b.String())) // one write(): never interleaved with other console output
	mu.Unlock()
}
