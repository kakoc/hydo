// vkc is the test client of the Valkey example: the workload's operations, the wait for the
// replicas, and the checks. It speaks RESP over TCP and uses only Go's standard library.
//
//	vkc op <kind>     one client operation; the composer's HYDO_* variables drive it
//	vkc settle [s]    wait until both replicas are online and hold the primary's whole stream
//	vkc check <rule>  one final rule over the client's records and the servers' data (check)
//	vkc ready <addr>  exit 0 when the server answers PING
//	vkc dump <addr>   print one server's keys, values and expiry times (for a look by hand)
//
// The servers are VKC_NODES (default: the primary 10.89.0.11, the replicas .12 and .13).
//
// Every operation that the client saw acknowledged writes one token to $HYDO_ACKED, and its
// outcome to $HYDO_OUTCOMES:
//
//	s<id>~<pad>                          SET s:<id> v<id>.<pad>xxx... (no expiry)
//	i<id>~<key>|<field>|<value>|<E>|<lo>|<hi>   the primary's view of one item at the end of the op
//
// In an item token, <field> is "-" for a string key, <value> is "-" for an item that was gone,
// <E> is the absolute expiry in ms that the primary reported (-1: none), and <lo>,<hi> are the
// bounds the client computed for it from its own clock (0,0: not known, for example after a
// clock step during the op). Every item belongs to one composer entry only, so no other
// operation changes it.
package main

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

var nodes = strings.Split(env("VKC_NODES", "10.89.0.11:6379,10.89.0.12:6379,10.89.0.13:6379"), ",")

// skew is the clock difference in ms allowed between the client and the servers: they share one
// clock in the guest; VKC_SKEW_MS widens it for servers in another machine (native/).
var skew = int64(atoiDef(os.Getenv("VKC_SKEW_MS"), 2))

func atoiDef(s string, d int) int {
	if n, err := strconv.Atoi(s); err == nil {
		return n
	}
	return d
}

const (
	tolMs    = 50 // items that expire this close to a dump are not compared between servers
	stepMs   = 20 // a wall-clock change larger than this against the monotonic clock is a clock step
	nHashes  = 4  // the shared hash keys h:0..h:3
	readMany = 8
)

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

// ---- RESP ----

type respErr string

func (e respErr) Error() string { return string(e) }

type conn struct {
	c net.Conn
	r *bufio.Reader
}

func dial(addr string) (*conn, error) {
	c, err := net.DialTimeout("tcp", addr, time.Second)
	if err != nil {
		return nil, err
	}
	return &conn{c: c, r: bufio.NewReader(c)}, nil
}

func (c *conn) close() { c.c.Close() }

func (c *conn) send(args ...string) error {
	var b strings.Builder
	fmt.Fprintf(&b, "*%d\r\n", len(args))
	for _, a := range args {
		fmt.Fprintf(&b, "$%d\r\n%s\r\n", len(a), a)
	}
	c.c.SetDeadline(time.Now().Add(3 * time.Second))
	_, err := io.WriteString(c.c, b.String())
	return err
}

// do sends one command and reads its reply: string, nil, int64, []any, or respErr.
func (c *conn) do(args ...string) (any, error) {
	if err := c.send(args...); err != nil {
		return nil, err
	}
	return c.read()
}

func (c *conn) read() (any, error) {
	c.c.SetDeadline(time.Now().Add(3 * time.Second))
	line, err := c.r.ReadString('\n')
	if err != nil {
		return nil, err
	}
	line = strings.TrimRight(line, "\r\n")
	if line == "" {
		return nil, errors.New("empty reply line")
	}
	switch line[0] {
	case '+':
		return line[1:], nil
	case '-':
		return respErr(line[1:]), nil
	case ':':
		n, err := strconv.ParseInt(line[1:], 10, 64)
		return n, err
	case '$':
		n, err := strconv.Atoi(line[1:])
		if err != nil {
			return nil, err
		}
		if n < 0 {
			return nil, nil
		}
		buf := make([]byte, n+2)
		if _, err := io.ReadFull(c.r, buf); err != nil {
			return nil, err
		}
		return string(buf[:n]), nil
	case '*':
		n, err := strconv.Atoi(line[1:])
		if err != nil {
			return nil, err
		}
		if n < 0 {
			return nil, nil
		}
		out := make([]any, n)
		for i := range out {
			if out[i], err = c.read(); err != nil {
				return nil, err
			}
		}
		return out, nil
	}
	return nil, fmt.Errorf("bad reply line %q", line)
}

// multi runs the commands in one MULTI/EXEC and returns EXEC's array.
func (c *conn) multi(cmds ...[]string) ([]any, error) {
	if r, err := c.do("MULTI"); err != nil {
		return nil, err
	} else if e, ok := r.(respErr); ok {
		return nil, e
	}
	for _, cmd := range cmds {
		r, err := c.do(cmd...)
		if err != nil {
			return nil, err
		}
		if e, ok := r.(respErr); ok {
			c.do("DISCARD")
			return nil, e
		}
	}
	r, err := c.do("EXEC")
	if err != nil {
		return nil, err
	}
	if e, ok := r.(respErr); ok {
		return nil, e
	}
	a, ok := r.([]any)
	if !ok {
		return nil, fmt.Errorf("EXEC answered %v", r)
	}
	return a, nil
}

func asInt(v any) int64 {
	switch x := v.(type) {
	case int64:
		return x
	case []any:
		if len(x) == 1 {
			return asInt(x[0])
		}
	case string:
		n, _ := strconv.ParseInt(x, 10, 64)
		return n
	}
	return -3
}

// ---- records ----

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		fmt.Printf("append %s: %v\n", path, err)
		os.Exit(3)
	}
	defer f.Close()
	if _, err := f.WriteString(line + "\n"); err != nil {
		os.Exit(3)
	}
}

func record(id string, token string, o outcome) {
	switch o {
	case ack:
		appendLine(os.Getenv("HYDO_ACKED"), token)
		appendLine(os.Getenv("HYDO_OUTCOMES"), id+" ack")
	case failed:
		appendLine(os.Getenv("HYDO_OUTCOMES"), id+" failed")
	default:
		appendLine(os.Getenv("HYDO_OUTCOMES"), id+" unknown")
	}
}

func sometimes(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	fmt.Fprintf(f, "HYDO-ASSERT %s\n", b)
}

func violation(format string, a ...any) {
	fmt.Printf("VIOLATION: "+format+"\n", a...)
	os.Exit(100)
}

// classify turns a command's error into an outcome: an error reply means the server refused
// the command (not applied); a network error leaves the result unknown.
func classify(r any, err error) outcome {
	if err != nil {
		return unknown
	}
	if _, ok := r.(respErr); ok {
		return failed
	}
	return ack
}

// ---- the clock ----

// span measures an interval with the wall clock and the monotonic clock. A clock step inside it
// makes the two disagree.
type span struct{ t0 time.Time }

func start() span           { return span{time.Now()} }
func (s span) wall0() int64 { return s.t0.UnixMilli() }
func (s span) stepped(t1 time.Time) bool {
	wall := t1.Round(0).Sub(s.t0.Round(0)).Milliseconds()
	mono := t1.Sub(s.t0).Milliseconds()
	d := wall - mono
	return d > stepMs || d < -stepMs
}

// ---- operations ----

type item struct {
	key, field, value string
	e, lo, hi         int64
}

func (it item) token(id int) string {
	return fmt.Sprintf("i%d~%s|%s|%s|%d|%d|%d", id, it.key, it.field, it.value, it.e, it.lo, it.hi)
}

func parseItem(tok string) (item, bool) {
	i := strings.IndexByte(tok, '~')
	if i < 0 || tok[0] != 'i' {
		return item{}, false
	}
	p := strings.Split(tok[i+1:], "|")
	if len(p) != 6 {
		return item{}, false
	}
	e, _ := strconv.ParseInt(p[3], 10, 64)
	lo, _ := strconv.ParseInt(p[4], 10, 64)
	hi, _ := strconv.ParseInt(p[5], 10, 64)
	return item{p[0], p[1], p[2], e, lo, hi}, true
}

var ttls = []int64{30, 100, 300, 1000, 3000, 10000, 60000, 600000}

func pick[T any](r *rand.Rand, a []T) T { return a[r.IntN(len(a))] }

func primary() (*conn, error) { return dial(nodes[0]) }

// readItem reads one item's value and absolute expiry in one transaction.
func readItem(c *conn, key, field string) (string, int64, error) {
	var a []any
	var err error
	if field == "-" {
		a, err = c.multi([]string{"GET", key}, []string{"PEXPIRETIME", key})
	} else {
		a, err = c.multi([]string{"HGET", key, field}, []string{"HPEXPIRETIME", key, "FIELDS", "1", field}, []string{"PEXPIRETIME", key})
		// a field's expiry is the earlier of its own and its hash's
		if err == nil && len(a) == 3 {
			if ke := asInt(a[2]); ke >= 0 && (asInt(a[1]) < 0 || ke < asInt(a[1])) {
				a[1] = ke
			}
			a = a[:2]
		}
	}
	if err != nil {
		return "", 0, err
	}
	if len(a) != 2 {
		return "", 0, fmt.Errorf("transaction answered %v", a)
	}
	if e, ok := a[0].(respErr); ok {
		return "", 0, e
	}
	if a[0] == nil {
		return "-", -2, nil
	}
	return fmt.Sprint(a[0]), asInt(a[1]), nil
}

func opSet(id int, r *rand.Rand) {
	pad := r.IntN(2000)
	key := fmt.Sprintf("s:%d", id)
	val := fmt.Sprintf("v%d.", id) + strings.Repeat("x", pad)
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	rep, err := c.do("SET", key, val)
	o := classify(rep, err)
	record(fmt.Sprint(id), fmt.Sprintf("s%d~%d", id, pad), o)
	if o != ack {
		fmt.Println("SET:", rep, err)
		os.Exit(1)
	}
	if n, err := c.do("WAIT", "2", "500"); err == nil && asInt(n) == 2 {
		sometimes("wait_reached_both_replicas", "WAIT 2 counted both replicas")
	}
}

// ttlStep is one command that sets an expiry, with how to compute its bounds.
type ttlStep struct {
	args    []string
	abs     int64 // >0: the absolute expiry the command names
	rel     int64 // >0: the relative expiry
	persist bool
}

func stringTTL(r *rand.Rand, key, val string) []ttlStep {
	ttl := pick(r, ttls)
	now := time.Now().UnixMilli()
	var steps []ttlStep
	switch r.IntN(5) {
	case 0:
		steps = append(steps, ttlStep{args: []string{"SET", key, val, "PX", fmt.Sprint(ttl)}, rel: ttl})
	case 1:
		steps = append(steps, ttlStep{args: []string{"SET", key, val, "PXAT", fmt.Sprint(now + ttl)}, abs: now + ttl})
	case 2:
		steps = append(steps, ttlStep{args: []string{"SET", key, val}, persist: true},
			ttlStep{args: []string{"PEXPIRE", key, fmt.Sprint(ttl)}, rel: ttl})
	case 3:
		s := (ttl + 999) / 1000
		steps = append(steps, ttlStep{args: []string{"SET", key, val, "EX", fmt.Sprint(s)}, rel: s * 1000})
	case 4:
		steps = append(steps, ttlStep{args: []string{"SET", key, val, "PX", fmt.Sprint(ttl)}, rel: ttl},
			ttlStep{args: []string{"GETEX", key, "PX", fmt.Sprint(pick(r, ttls))}})
	}
	switch r.IntN(4) {
	case 0:
		steps = append(steps, ttlStep{args: []string{"PERSIST", key}, persist: true})
	case 1:
		t2 := pick(r, ttls)
		steps = append(steps, ttlStep{args: []string{"GETEX", key, "PX", fmt.Sprint(t2)}, rel: t2})
	}
	// GETEX in case 4 above sets a relative expiry too; give it its bounds
	for i := range steps {
		if steps[i].args[0] == "GETEX" && steps[i].rel == 0 {
			steps[i].rel, _ = strconv.ParseInt(steps[i].args[3], 10, 64)
		}
	}
	return steps
}

func hashTTL(r *rand.Rand, key, field, val string) []ttlStep {
	ttl := pick(r, ttls)
	now := time.Now().UnixMilli()
	switch r.IntN(4) {
	case 0:
		return []ttlStep{{args: []string{"HSETEX", key, "PX", fmt.Sprint(ttl), "FIELDS", "1", field, val}, rel: ttl}}
	case 1:
		return []ttlStep{{args: []string{"HSETEX", key, "PXAT", fmt.Sprint(now + ttl), "FIELDS", "1", field, val}, abs: now + ttl}}
	case 2:
		s := (ttl + 999) / 1000
		return []ttlStep{{args: []string{"HSETEX", key, "FNX", "EX", fmt.Sprint(s), "FIELDS", "1", field, val}, rel: s * 1000}}
	}
	return []ttlStep{{args: []string{"HSET", key, field, val}, persist: true},
		{args: []string{"HPEXPIRE", key, fmt.Sprint(ttl), "FIELDS", "1", field}, rel: ttl}}
}

// runTTL runs the steps on the primary and records the item with the bounds of its last expiry.
func runTTL(id int, key, field string, steps []ttlStep, r *rand.Rand) {
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	sp := start()
	var lo, hi int64
	known := true
	for i, st := range steps {
		if i > 0 {
			time.Sleep(time.Duration(r.IntN(200)) * time.Millisecond)
		}
		t0 := time.Now()
		rep, err := c.do(st.args...)
		t1 := time.Now()
		if o := classify(rep, err); o != ack {
			fmt.Println(strings.Join(st.args[:2], " "), rep, err)
			record(fmt.Sprint(id), "", o)
			os.Exit(1)
		}
		switch {
		case st.persist:
			lo, hi = -1, -1
		case st.abs > 0:
			lo, hi = st.abs, st.abs
		case st.rel > 0:
			lo, hi = t0.UnixMilli()+st.rel, t1.UnixMilli()+st.rel
		}
		// a step that did not apply its expiry (the item was gone already) leaves the bounds open
		if !applied(st.args[0], rep) {
			known = false
		}
	}
	t0 := time.Now()
	v, e, err := readItem(c, key, field)
	t1 := time.Now()
	if err != nil {
		fmt.Println("read back:", err)
		record(fmt.Sprint(id), "", unknown)
		os.Exit(1)
	}
	if sp.stepped(t1) {
		known = false
	}
	it := item{key: key, field: field, value: v, e: e}
	if known {
		it.lo, it.hi = lo, hi
	}
	record(fmt.Sprint(id), it.token(id), ack)
	if !known {
		return
	}
	// The primary's own view right after the op must agree with the client's clock.
	switch {
	case v == "-" && lo == -1:
		violation("%s %s has no expiry and is gone at %d", key, field, t1.UnixMilli())
	case v == "-" && lo > 0 && lo > t1.UnixMilli()+skew:
		violation("%s %s expired early: gone at %d, its expiry is no earlier than %d", key, field, t1.UnixMilli(), lo)
	case v != "-" && hi > 0 && t0.UnixMilli() > hi+skew:
		violation("%s %s readable at %d after its expiry %d", key, field, t0.UnixMilli(), hi)
	case v != "-" && lo == -1 && e != -1:
		violation("%s %s has expiry %d after PERSIST", key, field, e)
	case v != "-" && lo > 0 && (e < lo-skew || e > hi+skew):
		violation("%s %s: the primary reports expiry %d, the client set it within [%d, %d]", key, field, e, lo, hi)
	}
}

// applied says whether a step of runTTL set what it names, from its reply.
func applied(cmd string, rep any) bool {
	switch cmd {
	case "SET":
		return rep == "OK"
	case "GETEX":
		return rep != nil
	case "PEXPIRE", "HPEXPIRE", "HSETEX", "PERSIST":
		return asInt(rep) == 1
	}
	return true // HSET
}

// hmut changes one field of a shared hash with a seeded sequence of hash commands, with pauses
// in between so that short expiries pass, and records the primary's view of it at the end. One
// time in three the hash is the op's own (hp:<id>), with a key-level expiry of 10 minutes, so
// that the field is the hash's last one.
func opHmut(id int, r *rand.Rand) {
	key := fmt.Sprintf("h:%d", r.IntN(nHashes))
	own := r.IntN(3) == 0
	if own {
		key = fmt.Sprintf("hp:%d", id)
	}
	f := fmt.Sprintf("m%d", id)
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	ttl := func() string { return fmt.Sprint(pick(r, ttls[:6])) }
	menu := []func() []string{
		func() []string { return []string{"HSET", key, f, "10"} },
		func() []string { return []string{"HPEXPIRE", key, ttl(), "FIELDS", "1", f} },
		func() []string {
			return []string{"HPEXPIRE", key, ttl(), pick(r, []string{"NX", "XX", "GT", "LT"}), "FIELDS", "1", f}
		},
		func() []string { return []string{"HINCRBY", key, f, "5"} },
		func() []string { return []string{"HINCRBYFLOAT", key, f, "0.5"} },
		func() []string { return []string{"HPERSIST", key, "FIELDS", "1", f} },
		func() []string { return []string{"HGETEX", key, "PX", ttl(), "FIELDS", "1", f} },
		func() []string { return []string{"HGETEX", key, "PERSIST", "FIELDS", "1", f} },
		func() []string { return []string{"HSETEX", key, "KEEPTTL", "FIELDS", "1", f, "20"} },
		func() []string { return []string{"HSETEX", key, "FXX", "PX", ttl(), "FIELDS", "1", f, "30"} },
		func() []string { return []string{"HSETEX", key, "FNX", "PX", ttl(), "FIELDS", "1", f, "50"} },
		func() []string { return []string{"HSETNX", key, f, "40"} },
		func() []string {
			return []string{"HPEXPIREAT", key, fmt.Sprint(time.Now().UnixMilli() + pick(r, ttls[:6])), "FIELDS", "1", f}
		},
		func() []string {
			return []string{"EVAL", "redis.call('HSETEX', KEYS[1], 'PX', ARGV[1], 'FIELDS', 1, ARGV[2], '60'); return redis.call('HINCRBY', KEYS[1], ARGV[2], 1)", "1", key, ttl(), f}
		},
		func() []string { return []string{"HDEL", key, f} },
		func() []string { return []string{"HGETDEL", key, "FIELDS", "1", f} },
	}
	first := []string{"HSETEX", key, "PX", ttl(), "FIELDS", "1", f, "1"}
	if r.IntN(2) == 0 {
		first = []string{"HSET", key, f, "1"}
	}
	cmds := [][]string{first}
	if own {
		cmds = append(cmds, []string{"PEXPIRE", key, "600000"})
	}
	for n := 1 + r.IntN(4); n > 0; n-- {
		// HDEL and HGETDEL are rare: they end the field's story
		k := r.IntN(len(menu))
		if k >= len(menu)-2 && r.IntN(3) != 0 {
			k = r.IntN(len(menu) - 2)
		}
		cmds = append(cmds, menu[k]())
	}
	var trace []string
	for i, cmd := range cmds {
		if i > 0 {
			time.Sleep(time.Duration(r.IntN(150)) * time.Millisecond)
		}
		rep, err := c.do(cmd...)
		trace = append(trace, fmt.Sprintf("%s=%v", cmd[0], rep))
		if o := classify(rep, err); o != ack {
			fmt.Println(strings.Join(cmd, " "), rep, err)
			// an error reply to a step of the sequence (for example HINCRBY on a float) is not a
			// fault: the field's state is still known after the read below
			if o == unknown {
				record(fmt.Sprint(id), "", unknown)
				os.Exit(1)
			}
		}
	}
	v, e, err := readItem(c, key, f)
	if err != nil {
		fmt.Println("read back:", err)
		record(fmt.Sprint(id), "", unknown)
		os.Exit(1)
	}
	fmt.Println(strings.Join(trace, " "))
	record(fmt.Sprint(id), item{key: key, field: f, value: v, e: e}.token(id), ack)
}

func opRewrite(r *rand.Rand) {
	addr := nodes[0]
	if r.IntN(3) == 0 {
		addr = nodes[1+r.IntN(len(nodes)-1)]
	}
	c, err := dial(addr)
	if err != nil {
		os.Exit(1)
	}
	defer c.close()
	rep, err := c.do("BGREWRITEAOF")
	fmt.Println(addr, rep, err)
	if classify(rep, err) != ack {
		os.Exit(1)
	}
}

// opRead reads some acknowledged items from one server and applies the expiry rule at once.
func opRead(r *rand.Rand) {
	var items []item
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if it, ok := parseItem(l); ok {
			items = append(items, it)
		}
	}
	if len(items) == 0 {
		return
	}
	n := r.IntN(len(nodes))
	c, err := dial(nodes[n])
	if err != nil {
		os.Exit(1)
	}
	defer c.close()
	for k := 0; k < readMany; k++ {
		it := items[r.IntN(len(items))]
		sp := start()
		v, e, err := readItem(c, it.key, it.field)
		t1 := time.Now()
		if err != nil {
			fmt.Println(nodes[n], it.key, it.field, err)
			os.Exit(1)
		}
		if sp.stepped(t1) {
			continue
		}
		if n != 0 {
			// A replica may lag (a partition, a restart from its own AOF): an older value or an
			// expiry it has not received yet is the documented asynchronous replication. What it
			// must never do is return an item after the expiry it holds itself.
			if v != "-" && e >= 0 && sp.wall0() > e+skew {
				violation("%s: %s %s readable at %d after its expiry %d on this replica", nodes[n], it.key, it.field, sp.wall0(), e)
			}
			continue
		}
		if msg := judge(it, v, e, sp.wall0(), t1.UnixMilli(), n == 0, marks()); msg != "" {
			violation("%s: %s", nodes[n], msg)
		}
	}
}

// clockMarks are the wall-clock times the operations wrote to $HYDO_ACKED (t~<ms>, at the
// start and the end of each): the latest one, and whether the clock ever went back by more than
// a second between two of them. An item whose expiry passed before a backward clock step is
// gone although its expiry is later than the clock at the end; the latest mark tells.
type clockMarks struct {
	max  int64
	back bool
}

// hwSlack covers the time between the last mark and a backward clock step.
const hwSlack = 1500

func mark() {
	if p := os.Getenv("HYDO_ACKED"); p != "" {
		appendLine(p, fmt.Sprintf("t~%d", time.Now().UnixMilli()))
	}
}

func marks() clockMarks {
	var m clockMarks
	var last int64
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if !strings.HasPrefix(l, "t~") {
			continue
		}
		t, _ := strconv.ParseInt(l[2:], 10, 64)
		if last > 0 && t < last-1000 {
			m.back = true
		}
		last = t
		m.max = max(m.max, t)
	}
	if now := time.Now().UnixMilli(); now < m.max-1000 {
		m.back = true
	}
	return m
}

// judge applies the rules to one item read from one server between t0 and t1. On the primary,
// an acknowledged item must be there until its expiry.
func judge(it item, v string, e, t0, t1 int64, onPrimary bool, hw clockMarks) string {
	name := it.key
	if it.field != "-" {
		name += " " + it.field
	}
	// the item's expiry as the primary reported it at the end of its operation
	exp := it.e
	if it.value == "-" {
		// gone at the end of its op; it must stay gone, unless the clock went back (an expired
		// item that was not reclaimed yet is readable again then, as with any clock)
		if v != "-" && !hw.back {
			return fmt.Sprintf("%s was gone when its operation ended and is back with %q", name, v)
		}
		return ""
	}
	if exp >= 0 && t0 > exp+skew && v != "-" {
		return fmt.Sprintf("%s readable at %d with %q, after its expiry %d", name, t0, v, exp)
	}
	if v != "-" && v != it.value {
		return fmt.Sprintf("%s has %q, the client's acknowledged value is %q", name, v, it.value)
	}
	if v != "-" && e != exp {
		return fmt.Sprintf("%s has expiry %d, the primary reported %d when its operation ended", name, e, exp)
	}
	if onPrimary && v == "-" && (exp == -1 || exp > max(t1, hw.max)+hwSlack) {
		return fmt.Sprintf("%s acknowledged with %q (expiry %d) is gone at %d", name, it.value, exp, t1)
	}
	return ""
}

// ---- settle ----

// replication returns the primary's offset and each replica's state and offset from INFO.
func replication(c *conn) (int64, map[string]string, error) {
	rep, err := c.do("INFO", "replication")
	if err != nil {
		return 0, nil, err
	}
	s, ok := rep.(string)
	if !ok {
		return 0, nil, fmt.Errorf("INFO answered %v", rep)
	}
	var off int64 = -1
	repl := map[string]string{}
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimSpace(l)
		if strings.HasPrefix(l, "master_repl_offset:") {
			off, _ = strconv.ParseInt(l[len("master_repl_offset:"):], 10, 64)
		}
		if strings.HasPrefix(l, "slave") && strings.Contains(l, ":ip=") {
			i := strings.IndexByte(l, ':')
			repl[l[:i]] = l[i+1:]
		}
		if strings.HasPrefix(l, "role:") && l != "role:master" {
			return 0, nil, fmt.Errorf("the primary reports %s", l)
		}
	}
	return off, repl, nil
}

func settle(limit time.Duration) bool {
	deadline := time.Now().Add(limit)
	last := ""
	for time.Now().Before(deadline) {
		time.Sleep(500 * time.Millisecond)
		c, err := primary()
		if err != nil {
			last = err.Error()
			continue
		}
		off, repl, err := replication(c)
		if err != nil {
			last = err.Error()
			c.close()
			continue
		}
		online := 0
		for _, v := range repl {
			if strings.Contains(v, "state=online") && strings.Contains(v, fmt.Sprintf("offset=%d,", off)) {
				online++
			}
		}
		last = fmt.Sprintf("offset %d, replicas %v", off, repl)
		if online == len(nodes)-1 {
			ok := true
			for _, a := range nodes[1:] {
				if !replicaUp(a) {
					ok = false
				}
			}
			c.close()
			if ok {
				fmt.Println("settled:", last)
				return true
			}
			continue
		}
		c.close()
	}
	fmt.Println("not settled after", limit, ":", last)
	return false
}

func replicaUp(addr string) bool {
	c, err := dial(addr)
	if err != nil {
		return false
	}
	defer c.close()
	rep, err := c.do("INFO", "replication")
	if err != nil {
		return false
	}
	s, _ := rep.(string)
	return strings.Contains(s, "master_link_status:up") && !strings.Contains(s, "master_sync_in_progress:1")
}

// ---- dump and check ----

type entry struct {
	v string
	e int64
}

// dump reads every key of one server: strings and hash fields, with their absolute expiries.
func dump(addr string) (map[string]entry, int64, int64, error) {
	c, err := dial(addr)
	if err != nil {
		return nil, 0, 0, err
	}
	defer c.close()
	t0 := time.Now().UnixMilli()
	out := map[string]entry{}
	cursor := "0"
	for {
		rep, err := c.do("SCAN", cursor, "COUNT", "1000")
		if err != nil {
			return nil, 0, 0, err
		}
		a, ok := rep.([]any)
		if !ok || len(a) != 2 {
			return nil, 0, 0, fmt.Errorf("SCAN answered %v", rep)
		}
		cursor = fmt.Sprint(a[0])
		keys, _ := a[1].([]any)
		for _, k := range keys {
			key := fmt.Sprint(k)
			t, err := c.do("TYPE", key)
			if err != nil {
				return nil, 0, 0, err
			}
			switch t {
			case "string":
				v, e, err := readItem(c, key, "-")
				if err != nil {
					return nil, 0, 0, err
				}
				if v != "-" {
					out[key] = entry{v, e}
				}
			case "hash":
				// the key-level expiry as an entry of its own
				a, err := c.multi([]string{"EXISTS", key}, []string{"PEXPIRETIME", key})
				if err != nil {
					return nil, 0, 0, err
				}
				if len(a) == 2 && asInt(a[0]) == 1 {
					out[key+" (key)"] = entry{"hash", asInt(a[1])}
				}
				h, err := c.do("HGETALL", key)
				if err != nil {
					return nil, 0, 0, err
				}
				fv, _ := h.([]any)
				for i := 0; i+1 < len(fv); i += 2 {
					f := fmt.Sprint(fv[i])
					v, e, err := readItem(c, key, f)
					if err != nil {
						return nil, 0, 0, err
					}
					if v != "-" {
						out[key+" "+f] = entry{v, e}
					}
				}
			case "none":
			default:
				out[key] = entry{"type " + fmt.Sprint(t), -1}
			}
		}
		if cursor == "0" {
			break
		}
	}
	return out, t0, time.Now().UnixMilli(), nil
}

func readLines(path string) []string {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []string
	for _, l := range strings.Split(string(b), "\n") {
		if l = strings.TrimSpace(l); l != "" {
			out = append(out, l)
		}
	}
	return out
}

func near(e, t0, t1 int64) bool { return e >= 0 && e >= t0-tolMs && e <= t1+tolMs }

// check applies one rule at the end of the run (after eventually_settle):
//
//	durable   every acknowledged SET and every acknowledged item is on the primary as the client
//	          last saw it, unless its expiry has passed
//	expiry    no server returns an item after its expiry, and the primary keeps the expiry the
//	          client set
//	converge  each replica holds exactly the primary's keys, values and expiries
//
// The detail lines come first; the last line counts, so that the explorer groups findings by
// what broke, not by which key.
func check(rule string) int {
	prim, p0, p1, err := dump(nodes[0])
	if err != nil {
		fmt.Println("primary unreadable:", err)
		return 2
	}
	var details []string
	bad := 0
	add := func(s string) {
		bad++
		if len(details) < 20 {
			details = append(details, s)
		}
	}
	nItems, nSets := 0, 0
	hw := marks()
	if hw.back {
		fmt.Println("the clock went back during the run: an item gone at the end of its operation may be readable again")
	}
	if rule == "durable" || rule == "expiry" {
		for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
			switch l[0] {
			case 's':
				if rule != "durable" {
					continue
				}
				nSets++
				var id, pad int
				fmt.Sscanf(l, "s%d~%d", &id, &pad)
				key := fmt.Sprintf("s:%d", id)
				want := fmt.Sprintf("v%d.", id) + strings.Repeat("x", pad)
				if got, ok := prim[key]; !ok {
					add(fmt.Sprintf("acknowledged SET %s is gone from the primary", key))
				} else if got.v != want {
					add(fmt.Sprintf("acknowledged SET %s has another value (%d bytes)", key, len(got.v)))
				}
			case 'i':
				it, ok := parseItem(l)
				if !ok {
					continue
				}
				nItems++
				name := it.key
				if it.field != "-" {
					name += " " + it.field
				}
				v, e := "-", int64(-2)
				if got, ok := prim[name]; ok {
					v, e = got.v, got.e
				}
				if near(it.e, p0, p1) {
					continue
				}
				msg := judge(it, v, e, p0, p1, true, hw)
				isExpiry := strings.Contains(msg, "after its expiry") || strings.Contains(msg, "has expiry")
				if msg != "" && isExpiry == (rule == "expiry") {
					add("primary: " + msg)
				}
			}
		}
	}
	if rule == "expiry" {
		// the replicas: nothing readable after its expiry
		for _, addr := range nodes[1:] {
			rep, r0, _, err := dump(addr)
			if err != nil {
				fmt.Println(addr, "unreadable:", err)
				continue
			}
			for k, re := range rep {
				if re.e >= 0 && r0 > re.e+skew {
					add(fmt.Sprintf("replica %s returns %s (%.20q) at %d after its expiry %d", addr, k, re.v, r0, re.e))
				}
			}
		}
	}
	diverged := 0
	if rule == "converge" {
		for _, addr := range nodes[1:] {
			rep, r0, r1, err := dump(addr)
			if err != nil {
				fmt.Println(addr, "unreadable:", err)
				return 2
			}
			var diffs []string
			lo, hi := min(p0, r0), max(p1, r1)
			for k, pe := range prim {
				re, ok := rep[k]
				if near(pe.e, lo, hi) || ok && near(re.e, lo, hi) {
					continue
				}
				switch {
				case !ok:
					diffs = append(diffs, fmt.Sprintf("%s missing (primary: %.20q expiry %d)", k, pe.v, pe.e))
				case re.v != pe.v || re.e != pe.e:
					diffs = append(diffs, fmt.Sprintf("%s: %.20q expiry %d (primary: %.20q expiry %d)", k, re.v, re.e, pe.v, pe.e))
				}
			}
			for k, re := range rep {
				if _, ok := prim[k]; !ok && !near(re.e, lo, hi) {
					diffs = append(diffs, fmt.Sprintf("%s extra: %.20q expiry %d", k, re.v, re.e))
				}
			}
			sort.Strings(diffs)
			for _, d := range diffs {
				add(fmt.Sprintf("replica %s: %s", addr, d))
			}
			if len(diffs) > 0 {
				diverged++
			}
		}
	}
	for _, d := range details {
		fmt.Println(d)
	}
	switch rule {
	case "durable":
		if bad > 0 {
			fmt.Printf("%d of %d acknowledged writes lost or changed on the primary\n", bad, nSets+nItems)
			return 1
		}
		fmt.Printf("all %d acknowledged SETs and %d items as acknowledged\n", nSets, nItems)
	case "expiry":
		if bad > 0 {
			fmt.Printf("%d items readable after their expiry or with another expiry\n", bad)
			return 1
		}
		fmt.Printf("no item past its expiry on any server (%d items)\n", nItems)
	case "converge":
		if bad > 0 {
			fmt.Printf("%d replicas differ from the primary in %d keys or fields\n", diverged, bad)
			return 1
		}
		fmt.Printf("both replicas hold the primary's %d keys and fields\n", len(prim))
	default:
		fmt.Println("unknown rule", rule)
		return 2
	}
	return 0
}

// ready exits 0 when the server at addr answers PING with PONG (not LOADING).
func ready(addr string) int {
	c, err := dial(addr)
	if err != nil {
		return 1
	}
	defer c.close()
	if r, err := c.do("PING"); err != nil || r != "PONG" {
		return 1
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: vkc op <kind> | settle [s] | check durable|expiry|converge | ready <addr> | dump <addr>")
		os.Exit(2)
	}
	seed, _ := strconv.ParseUint(env("HYDO_SEED", "1"), 10, 64)
	id, _ := strconv.Atoi(env("HYDO_CMD_ID", "0"))
	r := rand.New(rand.NewPCG(seed, uint64(id)))
	switch os.Args[1] {
	case "op":
		if len(os.Args) < 3 {
			os.Exit(2)
		}
		mark()
		switch os.Args[2] {
		case "set":
			opSet(id, r)
		case "px":
			key := fmt.Sprintf("e:%d", id)
			runTTL(id, key, "-", stringTTL(r, key, fmt.Sprintf("e%d", id)), r)
		case "hsx":
			key := fmt.Sprintf("h:%d", r.IntN(nHashes))
			f := fmt.Sprintf("f%d", id)
			runTTL(id, key, f, hashTTL(r, key, f, fmt.Sprintf("f%d", id)), r)
		case "hmut":
			opHmut(id, r)
		case "rewrite":
			opRewrite(r)
		case "read":
			opRead(r)
		default:
			fmt.Fprintln(os.Stderr, "unknown op", os.Args[2])
			os.Exit(2)
		}
	case "settle":
		s := 90
		if len(os.Args) > 2 {
			s, _ = strconv.Atoi(os.Args[2])
		}
		if !settle(time.Duration(s) * time.Second) {
			os.Exit(1)
		}
	case "check":
		if len(os.Args) < 3 {
			os.Exit(2)
		}
		os.Exit(check(os.Args[2]))
	case "ready":
		os.Exit(ready(os.Args[2]))
	case "dump":
		m, _, _, err := dump(os.Args[2])
		if err != nil {
			fmt.Println(err)
			os.Exit(1)
		}
		keys := make([]string, 0, len(m))
		for k := range m {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			fmt.Printf("%s\t%.40q\t%d\n", k, m[k].v, m[k].e)
		}
	default:
		os.Exit(2)
	}
}
