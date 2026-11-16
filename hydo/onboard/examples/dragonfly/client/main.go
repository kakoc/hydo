// dfc is the test client of the Dragonfly example: the workload's operations, the wait for the
// replicas, and the checks. It speaks RESP over TCP and uses only Go's standard library.
//
//	dfc op <kind>     one client operation; the composer's HYDO_* variables drive it
//	dfc settle [s]    wait until both replicas are in stable sync with the primary
//	dfc check <rule>  one final rule over the client's records and the servers' data
//	dfc ready <addr>  exit 0 when the server answers a read (not LOADING)
//	dfc dump <addr>   print one server's keys, values and expiry times (for a look by hand)
//
// The servers are DFC_NODES (default: the primary 10.89.0.11, the replicas .12 and .13).
//
// Every operation that the client saw acknowledged writes one token to $HYDO_ACKED, in the
// order the client saw them, and its outcome to $HYDO_OUTCOMES:
//
//	s<id>~<pad>                     SET s:<id> v<id>.<pad x's> (no expiry)
//	t<id>~<kind>                    t:<id>:a, t:<id>:b and t:<id>:c set to v<id> in one MULTI/EXEC
//	                                (kind m) or one Lua script (kind l)
//	x<id>~<value>|<E>|<lo>|<hi>     the primary's view of x:<id> at the end of its operation: the
//	                                value ("-": gone), the absolute expiry in ms, and the bounds
//	                                the client computed for it (0,0: unknown)
//	save~<id>                       a SAVE on the primary answered OK, or a SHUTDOWN of the
//	                                primary (which saves) and its restart completed
//	c~<ms>                          the client's wall clock at the end of an operation
//
// Every key belongs to one composer entry only, so no other operation changes it.
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

var nodes = strings.Split(env("DFC_NODES", "10.89.0.11:6379,10.89.0.12:6379,10.89.0.13:6379"), ",")

const (
	tolMs  = 50 // items that expire this close to a reading are not judged
	stepMs = 20 // a wall-clock change larger than this against the monotonic clock is a clock step
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

func mark() { appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("c~%d", time.Now().UnixMilli())) }

// ---- operations ----

func pick[T any](r *rand.Rand, a []T) T { return a[r.IntN(len(a))] }

// primaryFile holds the index in DFC_NODES of the current primary, written by a takeover
// (opTakeover). Without it the primary is the first node.
var primaryFile = env("DFC_PRIMARY_FILE", "/data/primary")

func primIdx() int {
	b, err := os.ReadFile(primaryFile)
	if err != nil {
		return 0
	}
	i, err := strconv.Atoi(strings.TrimSpace(string(b)))
	if err != nil || i < 0 || i >= len(nodes) {
		return 0
	}
	return i
}

func prim() string { return nodes[primIdx()] }

// reps lists the replicas: every node but the current primary.
func reps() []string {
	var out []string
	for i, a := range nodes {
		if i != primIdx() {
			out = append(out, a)
		}
	}
	return out
}

func primary() (*conn, error) { return dial(prim()) }

// pidOf reads the server's process id on this connection. It names the server's incarnation: a
// write and a save with the same one ran in one process, with no restart between them.
func pidOf(c *conn) string {
	m, err := info(c, "server")
	if err != nil || m["process_id"] == "" {
		return "?"
	}
	return m["process_id"]
}

func setVal(id, pad int) string { return fmt.Sprintf("v%d.", id) + strings.Repeat("x", pad) }

func opSet(id int, r *rand.Rand) {
	pad := r.IntN(2000)
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	rep, err := c.do("SET", fmt.Sprintf("s:%d", id), setVal(id, pad))
	o := classify(rep, err)
	pid := "?"
	if o == ack {
		pid = pidOf(c)
	}
	record(fmt.Sprint(id), fmt.Sprintf("s%d~%d~%s", id, pad, pid), o)
	if o != ack {
		fmt.Println("SET:", rep, err)
		os.Exit(1)
	}
	if n, err := c.do("WAIT", "2", "500"); err == nil && asInt(n) == 2 {
		sometimes("wait_reached_both_replicas", "WAIT 2 counted both replicas")
	}
	mark()
}

const txScript = "redis.call('SET', KEYS[1], ARGV[1]); redis.call('SET', KEYS[2], ARGV[1]); redis.call('SET', KEYS[3], ARGV[1]); return 3"

// opTx sets three keys, which fall on both shards most of the time, in one transaction.
func opTx(id int, r *rand.Rand) {
	v := fmt.Sprintf("v%d", id)
	keys := []string{fmt.Sprintf("t:%d:a", id), fmt.Sprintf("t:%d:b", id), fmt.Sprintf("t:%d:c", id)}
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	kind := "m"
	var o outcome
	if r.IntN(2) == 0 {
		a, err := c.multi([]string{"SET", keys[0], v}, []string{"SET", keys[1], v}, []string{"SET", keys[2], v})
		switch {
		case err != nil && errors.As(err, new(respErr)):
			o = failed
		case err != nil:
			o = unknown
		case len(a) == 3 && a[0] == "OK" && a[1] == "OK" && a[2] == "OK":
			o = ack
		default:
			fmt.Println("EXEC answered", a)
			o = failed
		}
	} else {
		kind = "l"
		rep, err := c.do("EVAL", txScript, "3", keys[0], keys[1], keys[2], v)
		o = classify(rep, err)
		if o == ack && asInt(rep) != 3 {
			fmt.Println("EVAL answered", rep)
			o = failed
		}
	}
	pid := "?"
	if o == ack {
		pid = pidOf(c)
	}
	record(fmt.Sprint(id), fmt.Sprintf("t%d~%s~%s", id, kind, pid), o)
	if o != ack {
		fmt.Println("transaction", kind, "not acknowledged")
		os.Exit(1)
	}
	mark()
}

var ttls = []int64{100, 300, 1000, 3000, 10000, 60000, 600000}

// opPx sets x:<id> with an expiry and reads back the value and the absolute expiry.
func opPx(id int, r *rand.Rand) {
	key := fmt.Sprintf("x:%d", id)
	val := fmt.Sprintf("p%d", id)
	ttl := pick(r, ttls)
	c, err := primary()
	if err != nil {
		record(fmt.Sprint(id), "", failed)
		os.Exit(1)
	}
	defer c.close()
	sp := start()
	var lo, hi int64
	var rep any
	switch r.IntN(4) {
	case 0:
		rep, err = c.do("SET", key, val, "PX", fmt.Sprint(ttl))
		lo, hi = ttl, ttl
	case 1:
		s := ttl/1000 + 1
		rep, err = c.do("SET", key, val, "EX", fmt.Sprint(s))
		lo, hi = s*1000, s*1000
	case 2:
		abs := sp.wall0() + ttl
		rep, err = c.do("SET", key, val, "PXAT", fmt.Sprint(abs))
		lo, hi = abs-sp.wall0(), abs-sp.wall0()
	default:
		rep, err = c.do("SET", key, val)
		if classify(rep, err) == ack {
			rep, err = c.do("PEXPIRE", key, fmt.Sprint(ttl))
			if err == nil && asInt(rep) != 1 {
				err = fmt.Errorf("PEXPIRE answered %v", rep)
			}
		}
		lo, hi = ttl, ttl
	}
	o := classify(rep, err)
	if o != ack {
		record(fmt.Sprint(id), "", o)
		fmt.Println("set with expiry:", rep, err)
		os.Exit(1)
	}
	t1 := time.Now()
	lo += sp.wall0()
	hi += t1.UnixMilli()
	if sp.stepped(t1) {
		lo, hi = 0, 0
	}
	a, err := c.multi([]string{"GET", key}, []string{"PEXPIRETIME", key})
	if err != nil || len(a) != 2 {
		// acknowledged, but the reading failed: the bounds stand for the expiry
		record(fmt.Sprint(id), fmt.Sprintf("x%d~%s|%d|%d|%d~?", id, val, -3, lo, hi), ack)
		mark()
		return
	}
	v, e := "-", asInt(a[1])
	if a[0] != nil {
		v = fmt.Sprint(a[0])
	}
	record(fmt.Sprint(id), fmt.Sprintf("x%d~%s|%d|%d|%d~%s", id, v, e, lo, hi, pidOf(c)), ack)
	mark()
	if v != "-" && lo != 0 && (e < lo-2 || e > hi+2) {
		violation("x:%d: the primary holds expiry %d, outside the %d..%d the client set", id, e, lo, hi)
	}
}

// opSave makes the primary write a snapshot: SAVE (or BGSAVE and a wait) in most rounds, and in
// one round in four a SHUTDOWN, which saves too, followed by the wait for the restart.
func opSave(id int, r *rand.Rand) {
	c, err := primary()
	if err != nil {
		os.Exit(1)
	}
	defer c.close()
	pid := pidOf(c)
	if pid == "?" {
		os.Exit(1)
	}
	switch k := r.IntN(8); {
	case k < 4:
		rep, err := c.do("SAVE")
		if classify(rep, err) != ack {
			fmt.Println("SAVE:", rep, err)
			os.Exit(1)
		}
		fmt.Println("SAVE: OK")
	case k < 6:
		rep, err := c.do("BGSAVE")
		if classify(rep, err) != ack {
			fmt.Println("BGSAVE:", rep, err)
			os.Exit(1)
		}
		// BGSAVE answers at the start; the save is done when INFO persistence says so
		done := false
		for i := 0; i < 100 && !done; i++ {
			time.Sleep(100 * time.Millisecond)
			rep, err := c.do("INFO", "persistence")
			if err != nil {
				os.Exit(1)
			}
			s, _ := rep.(string)
			done = strings.Contains(s, "rdb_bgsave_in_progress:0") && strings.Contains(s, "rdb_last_bgsave_status:ok")
		}
		if !done {
			fmt.Println("BGSAVE did not finish")
			os.Exit(1)
		}
		fmt.Println("BGSAVE: done")
	default:
		// SHUTDOWN saves a snapshot and exits; restart: always starts the primary again
		c.do("SHUTDOWN")
		deadline := time.Now().Add(60 * time.Second)
		back := false
		for time.Now().Before(deadline) && !back {
			time.Sleep(200 * time.Millisecond)
			back = ready(prim()) == 0
		}
		if !back {
			fmt.Println("the primary did not come back after SHUTDOWN")
			os.Exit(1)
		}
		fmt.Println("SHUTDOWN: the primary is back")
	}
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("save~%d~%s", id, pid))
	mark()
}

// roleFile is where a node's start command reads its --replicaof flag (empty: a primary).
func roleFile(i int) string {
	return fmt.Sprintf(env("DFC_ROLE_FILES", "/data/vol/d%d/replicaof"), i+1)
}

func writeSync(path, s string) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	if _, err := f.WriteString(s); err != nil {
		f.Close()
		return err
	}
	if err := f.Sync(); err != nil {
		f.Close()
		return err
	}
	return f.Close()
}

// opTakeover promotes a replica in stable sync with REPLTAKEOVER 10, and checks Dragonfly's
// promise at once: the master waits until the replica has applied all of its writes, so the new
// primary holds every key the old primary held just before. A serial driver runs alone, so no
// write happens between the two readings. It does nothing unless /etc/dfc.conf says
// takeover=yes (the parameter takeover in hydo.yaml).
func opTakeover(id int, r *rand.Rand) {
	conf, _ := os.ReadFile(env("DFC_CONF", "/etc/dfc.conf"))
	if !strings.Contains(string(conf), "takeover=yes") {
		return
	}
	old := primIdx()
	var cands []int
	for i, a := range nodes {
		if i == old {
			continue
		}
		c, err := dial(a)
		if err != nil {
			continue
		}
		m, err := info(c, "replication")
		c.close()
		if err == nil && m["master_link_status"] == "up" && m["master_sync_in_progress"] == "0" {
			cands = append(cands, i)
		}
	}
	if len(cands) == 0 {
		fmt.Println("no replica in stable sync")
		os.Exit(1)
	}
	nw := pick(r, cands)
	before, t0, err := dump(nodes[old])
	if err != nil {
		fmt.Println("old primary unreadable:", err)
		os.Exit(1)
	}
	// the start commands' roles after the takeover: the old primary and the third node follow nw
	for i := range nodes {
		flag := ""
		if i != nw {
			flag = "--replicaof=" + nodes[nw]
		}
		if err := writeSync(roleFile(i), flag); err != nil {
			fmt.Println("role file:", err)
			os.Exit(1)
		}
	}
	undo := func() {
		for i := range nodes {
			flag := ""
			if i != old {
				flag = "--replicaof=" + nodes[old]
			}
			writeSync(roleFile(i), flag)
		}
	}
	c, err := dial(nodes[nw])
	if err != nil {
		undo()
		os.Exit(1)
	}
	defer c.close()
	c.c.SetDeadline(time.Now().Add(20 * time.Second))
	rep, err := c.do("REPLTAKEOVER", "10")
	if err != nil || rep != "OK" {
		// a failed takeover leaves the roles as they were, or the result is unknown
		fmt.Println("REPLTAKEOVER:", rep, err)
		if err == nil {
			undo()
		}
		os.Exit(1)
	}
	if err := writeSync(primaryFile, fmt.Sprint(nw)); err != nil {
		fmt.Println("primary file:", err)
		os.Exit(1)
	}
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("tk~%d~%d", id, nw))
	sometimes("takeover_ok", "REPLTAKEOVER answered OK")
	fmt.Printf("REPLTAKEOVER: %s is the primary instead of %s\n", nodes[nw], nodes[old])
	after, _, err := dump(nodes[nw])
	if err != nil {
		fmt.Println("new primary unreadable:", err)
		os.Exit(1)
	}
	var missing []string
	for k, e := range before {
		if e.e > 0 && e.e < t0+2000 {
			continue // it may expire between the two readings
		}
		if a, ok := after[k]; !ok || a.v != e.v {
			missing = append(missing, k)
		}
	}
	// the third node follows the new primary
	for i, a := range nodes {
		if i != nw && i != old {
			if rc, err := dial(a); err == nil {
				host, port, _ := strings.Cut(nodes[nw], ":")
				rc.do("REPLICAOF", host, port)
				rc.close()
			}
		}
	}
	mark()
	if len(missing) > 0 {
		sort.Strings(missing)
		if len(missing) > 10 {
			missing = append(missing[:10], "...")
		}
		fmt.Println("missing:", strings.Join(missing, " "))
		violation("after REPLTAKEOVER answered OK, the new primary %s lacks %d of the %d keys the old primary held", nodes[nw], len(missing), len(before))
	}
}

// opRead reads a few acknowledged items from one server and applies the expiry rule at once.
func opRead(r *rand.Rand) {
	var xs []string
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if strings.HasPrefix(l, "x") {
			xs = append(xs, l)
		}
	}
	if len(xs) == 0 {
		return
	}
	addr := pick(r, nodes)
	c, err := dial(addr)
	if err != nil {
		os.Exit(1)
	}
	defer c.close()
	for i := 0; i < 8; i++ {
		it, ok := parseX(pick(r, xs))
		if !ok {
			continue
		}
		sp := start()
		a, err := c.multi([]string{"GET", it.key}, []string{"PEXPIRETIME", it.key})
		if err != nil || len(a) != 2 {
			os.Exit(1)
		}
		if a[0] == nil || sp.stepped(time.Now()) {
			continue
		}
		e := asInt(a[1])
		if e > 0 && e < sp.wall0()-tolMs {
			violation("%s returned %s (%v) at %d, after its own expiry %d", addr, it.key, a[0], sp.wall0(), e)
		}
	}
}

// ---- the replicas ----

func info(c *conn, section string) (map[string]string, error) {
	rep, err := c.do("INFO", section)
	if err != nil {
		return nil, err
	}
	s, ok := rep.(string)
	if !ok {
		return nil, fmt.Errorf("INFO answered %v", rep)
	}
	m := map[string]string{}
	for _, l := range strings.Split(s, "\n") {
		l = strings.TrimSpace(l)
		if i := strings.IndexByte(l, ':'); i > 0 {
			m[l[:i]] = l[i+1:]
		}
	}
	return m, nil
}

// settled says whether the primary reports both replicas online with no lag, and both replicas
// report their link up and no full sync in progress.
func settled() (bool, string) {
	c, err := primary()
	if err != nil {
		return false, err.Error()
	}
	defer c.close()
	m, err := info(c, "replication")
	if err != nil {
		return false, err.Error()
	}
	if m["role"] != "master" {
		return false, "the primary reports role " + m["role"]
	}
	online := 0
	var desc []string
	for i := 0; i < len(nodes)-1; i++ {
		v := m[fmt.Sprintf("slave%d", i)]
		desc = append(desc, v)
		if strings.Contains(v, "state=online") && strings.HasSuffix(v, ",lag=0") {
			online++
		}
	}
	if online != len(nodes)-1 {
		return false, fmt.Sprintf("connected_slaves %s: %s", m["connected_slaves"], strings.Join(desc, " "))
	}
	for _, a := range reps() {
		rc, err := dial(a)
		if err != nil {
			return false, a + ": " + err.Error()
		}
		rm, err := info(rc, "replication")
		rc.close()
		if err != nil {
			return false, a + ": " + err.Error()
		}
		if rm["master_link_status"] != "up" || rm["master_sync_in_progress"] != "0" {
			return false, fmt.Sprintf("%s: link %s, full sync %s", a, rm["master_link_status"], rm["master_sync_in_progress"])
		}
	}
	return true, strings.Join(desc, " ")
}

func settle(limit time.Duration) bool {
	deadline := time.Now().Add(limit)
	last := ""
	for time.Now().Before(deadline) {
		ok, why := settled()
		if ok {
			fmt.Println("settled:", why)
			return true
		}
		last = why
		repoint()
		time.Sleep(500 * time.Millisecond)
	}
	fmt.Println("not settled after", limit, ":", last)
	return false
}

// repoint does the operator's part of a takeover that the documentation of REPLTAKEOVER names:
// a replica that still follows another node than the current primary gets REPLICAOF with the
// primary's address. A second node that reports itself a primary is left alone: two primaries
// are a finding, and settle then fails.
//
// A REPLTAKEOVER whose answer the client did not get (a pause or a partition) may still have
// moved the primary. When the recorded primary reports itself a replica and exactly one node
// reports itself a primary, repoint records that node as the primary, as an orchestrator would.
func repoint() {
	conf, _ := os.ReadFile(env("DFC_CONF", "/etc/dfc.conf"))
	if !strings.Contains(string(conf), "takeover=yes") {
		return
	}
	roles := make([]string, len(nodes))
	for i, a := range nodes {
		if rc, err := dial(a); err == nil {
			if m, err := info(rc, "replication"); err == nil {
				roles[i] = m["role"]
			}
			rc.close()
		}
	}
	if roles[primIdx()] != "master" {
		masters := []int{}
		for i, r := range roles {
			if r == "master" {
				masters = append(masters, i)
			}
		}
		if len(masters) == 1 {
			fmt.Println("repoint: the recorded primary", prim(), "is a", roles[primIdx()], "; the primary is", nodes[masters[0]])
			writeSync(primaryFile, fmt.Sprint(masters[0]))
			for i := range nodes {
				flag := ""
				if i != masters[0] {
					flag = "--replicaof=" + nodes[masters[0]]
				}
				writeSync(roleFile(i), flag)
			}
		}
	}
	host, port, _ := strings.Cut(prim(), ":")
	name := fmt.Sprintf("df%d", primIdx()+1) // the Compose name the replicas start with
	for _, a := range reps() {
		rc, err := dial(a)
		if err != nil {
			continue
		}
		rm, err := info(rc, "replication")
		if err == nil && (rm["role"] != "master" && rm["master_host"] != host && rm["master_host"] != name) {
			rep, err := rc.do("REPLICAOF", host, port)
			fmt.Println("repoint:", a, "followed", rm["master_host"], "role", rm["role"], "; REPLICAOF:", rep, err)
		}
		rc.close()
	}
}

// ---- dump and checks ----

type entry struct {
	v string
	e int64
}

// dump reads every key of one server with its value and absolute expiry.
func dump(addr string) (map[string]entry, int64, error) {
	c, err := dial(addr)
	if err != nil {
		return nil, 0, err
	}
	defer c.close()
	t0 := time.Now().UnixMilli()
	out := map[string]entry{}
	cursor := "0"
	for {
		rep, err := c.do("SCAN", cursor, "COUNT", "1000")
		if err != nil {
			return nil, 0, err
		}
		if e, ok := rep.(respErr); ok {
			return nil, 0, e
		}
		a, ok := rep.([]any)
		if !ok || len(a) != 2 {
			return nil, 0, fmt.Errorf("SCAN answered %v", rep)
		}
		cursor = fmt.Sprint(a[0])
		keys, _ := a[1].([]any)
		for _, k := range keys {
			ks := fmt.Sprint(k)
			r, err := c.multi([]string{"GET", ks}, []string{"PEXPIRETIME", ks})
			if err != nil || len(r) != 2 {
				return nil, 0, fmt.Errorf("read %s: %v", ks, err)
			}
			if r[0] == nil {
				continue
			}
			out[ks] = entry{fmt.Sprint(r[0]), asInt(r[1])}
		}
		if cursor == "0" {
			break
		}
	}
	return out, t0, nil
}

type xitem struct {
	id        int
	key, v    string
	e, lo, hi int64
}

func parseX(tok string) (xitem, bool) {
	i := strings.IndexByte(tok, '~')
	if i < 0 || tok[0] != 'x' {
		return xitem{}, false
	}
	id, err := strconv.Atoi(tok[1:i])
	if err != nil {
		return xitem{}, false
	}
	body, _, _ := strings.Cut(tok[i+1:], "~") // the primary's process id follows
	p := strings.Split(body, "|")
	if len(p) != 4 {
		return xitem{}, false
	}
	e, _ := strconv.ParseInt(p[1], 10, 64)
	lo, _ := strconv.ParseInt(p[2], 10, 64)
	hi, _ := strconv.ParseInt(p[3], 10, 64)
	return xitem{id, fmt.Sprintf("x:%d", id), p[0], e, lo, hi}, true
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

func idOf(tok string) int {
	i := strings.IndexByte(tok, '~')
	if i < 2 {
		return -1
	}
	n, err := strconv.Atoi(tok[1:i])
	if err != nil {
		return -1
	}
	return n
}

// check runs one final rule. Exit status: 0 holds, 1 violated, 2 cannot be judged now.
func check(rule string) int {
	acked := readLines(os.Getenv("HYDO_ACKED"))
	now := time.Now().UnixMilli()
	var hw int64 // the latest wall clock the client saw
	for _, l := range acked {
		if strings.HasPrefix(l, "c~") {
			if t, _ := strconv.ParseInt(l[2:], 10, 64); t > hw {
				hw = t
			}
		}
	}
	// A write acknowledged before a SAVE (or a SHUTDOWN) that completed in the same process of
	// the primary is in that snapshot. Every later start of the primary loads the newest
	// snapshot, so the write must be there at the end. A write whose process ended before any
	// such save may be lost: Dragonfly keeps no log without --aof.
	// A takeover's own check (opTakeover) covers the writes before it, so these rules judge only
	// the writes after the last takeover.
	lastSave := map[string]int{} // the primary's process id -> the index of its last save
	lastTk := -1
	for i, l := range acked {
		if p := strings.Split(l, "~"); p[0] == "save" && len(p) == 3 {
			lastSave[p[2]] = i
		} else if p[0] == "tk" {
			lastTk = i
		}
	}
	covered := func(i int, l string) bool {
		p := strings.Split(l, "~")
		if len(p) != 3 || p[2] == "?" || i < lastTk {
			return false
		}
		j, ok := lastSave[p[2]]
		return ok && j > i
	}
	switch rule {
	case "durable":
		d, _, err := dump(prim())
		if err != nil {
			fmt.Println("primary unreadable:", err)
			return 2
		}
		var lost []string
		saved, kept := 0, 0
		for i, l := range acked {
			if !covered(i, l) {
				continue
			}
			id := idOf(l)
			switch {
			case strings.HasPrefix(l, "s") && id >= 0:
				saved++
				pad, _ := strconv.Atoi(strings.Split(l, "~")[1])
				if d[fmt.Sprintf("s:%d", id)].v != setVal(id, pad) {
					lost = append(lost, fmt.Sprintf("s:%d", id))
				} else {
					kept++
				}
			case strings.HasPrefix(l, "t") && id >= 0:
				saved++
				ok := true
				for _, k := range []string{"a", "b", "c"} {
					if d[fmt.Sprintf("t:%d:%s", id, k)].v != fmt.Sprintf("v%d", id) {
						ok = false
					}
				}
				if ok {
					kept++
				} else {
					lost = append(lost, fmt.Sprintf("t:%d", id))
				}
			}
		}
		if len(lost) > 0 {
			sort.Strings(lost)
			if len(lost) > 20 {
				lost = append(lost[:20], "...")
			}
			fmt.Println("lost:", strings.Join(lost, " "))
			fmt.Printf("the primary lost %d of %d writes acknowledged before a completed snapshot\n", len(lost), saved)
			return 1
		}
		fmt.Printf("all %d writes acknowledged before a completed snapshot are on the primary (%d keys there)\n", kept, len(d))
		return 0
	case "atomic":
		// On every server, the three keys of a transaction are all there or none is.
		bad := 0
		var ex []string
		for _, a := range nodes {
			d, _, err := dump(a)
			if err != nil {
				fmt.Println(a, "unreadable:", err)
				return 2
			}
			cnt := map[string]int{}
			for k := range d {
				if strings.HasPrefix(k, "t:") {
					cnt[k[:strings.LastIndexByte(k, ':')]]++
				}
			}
			for t, n := range cnt {
				if n != 3 {
					bad++
					if len(ex) < 10 {
						ex = append(ex, fmt.Sprintf("%s has %d of 3 keys of %s", a, n, t))
					}
				}
			}
		}
		if bad > 0 {
			sort.Strings(ex)
			fmt.Println(strings.Join(ex, "; "))
			fmt.Printf("%d transactions are applied in part on some server\n", bad)
			return 1
		}
		fmt.Println("every transaction is whole or absent on every server")
		return 0
	case "converge":
		// After the faults heal, each replica holds exactly the primary's keys and values, and the
		// primary's expiries. The rule waits up to 40 s for the replicas to catch up.
		deadline := time.Now().Add(40 * time.Second)
		var why []string
		for {
			why = why[:0]
			p, t0, err := dump(prim())
			if err != nil {
				fmt.Println("primary unreadable:", err)
				return 2
			}
			for _, a := range reps() {
				d, _, err := dump(a)
				if err != nil {
					why = append(why, a+" unreadable")
					continue
				}
				diff := 0
				var ex []string
				for k, pe := range p {
					re, ok := d[k]
					near := pe.e > 0 && pe.e < t0+2000
					if near {
						continue
					}
					if !ok || re.v != pe.v || (pe.e > 0) != (re.e > 0) || abs(pe.e-re.e) > 2 {
						diff++
						if len(ex) < 5 {
							ex = append(ex, fmt.Sprintf("%s: primary %.12q e=%d, replica %.12q e=%d", k, pe.v, pe.e, re.v, re.e))
						}
					}
				}
				for k, re := range d {
					if _, ok := p[k]; !ok && !(re.e > 0 && re.e < t0+2000) {
						diff++
						if len(ex) < 5 {
							ex = append(ex, fmt.Sprintf("%s: only on the replica", k))
						}
					}
				}
				if diff > 0 {
					why = append(why, fmt.Sprintf("%s differs from the primary in %d keys (%d on the primary, %d there): %s", a, diff, len(p), len(d), strings.Join(ex, "; ")))
				}
			}
			if len(why) == 0 {
				fmt.Printf("both replicas hold the primary's %d keys\n", len(p))
				return 0
			}
			if time.Now().After(deadline) {
				break
			}
			time.Sleep(time.Second)
		}
		for _, w := range why {
			fmt.Println(w)
		}
		fmt.Printf("%d replicas differ from the primary\n", len(why))
		return 1
	case "expiry":
		// No server returns x:<id> after its own expiry. The primary keeps an item that a
		// completed snapshot holds (as in the rule durable) and whose expiry is later than every
		// clock reading of the run.
		bad := 0
		var ex []string
		for _, a := range nodes {
			d, t0, err := dump(a)
			if err != nil {
				fmt.Println(a, "unreadable:", err)
				return 2
			}
			for k, en := range d {
				if strings.HasPrefix(k, "x:") && en.e > 0 && en.e < t0-tolMs {
					bad++
					if len(ex) < 5 {
						ex = append(ex, fmt.Sprintf("%s returned %s after its expiry %d (now %d)", a, k, en.e, t0))
					}
				}
			}
			if a != prim() {
				continue
			}
			for i, l := range acked {
				it, ok := parseX(l)
				if !ok || it.v == "-" || it.e <= 0 || !covered(i, l) {
					continue
				}
				// 10 s past the last clock reading: a clock step back can follow the reading by a few rounds
				if it.e > max(hw+10000, now+tolMs) && d[it.key].v != it.v {
					bad++
					if len(ex) < 5 {
						ex = append(ex, fmt.Sprintf("%s is gone from the primary before its expiry %d (now %d)", it.key, it.e, now))
					}
				}
			}
		}
		if bad > 0 {
			fmt.Println(strings.Join(ex, "; "))
			fmt.Printf("%d items break the expiry rule\n", bad)
			return 1
		}
		fmt.Println("no server returns an item after its expiry, and the primary keeps the live ones")
		return 0
	}
	fmt.Println("unknown rule", rule)
	return 2
}

func abs(x int64) int64 {
	if x < 0 {
		return -x
	}
	return x
}

// ready answers 0 when the server serves reads: Dragonfly answers LOADING while it loads a
// snapshot.
func ready(addr string) int {
	c, err := dial(addr)
	if err != nil {
		return 1
	}
	defer c.close()
	rep, err := c.do("GET", "ready:probe")
	if err != nil {
		return 1
	}
	if e, ok := rep.(respErr); ok {
		fmt.Println(string(e))
		return 1
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("usage: dfc op <kind> | settle [s] | check <rule> | ready <addr> | dump <addr>")
		os.Exit(2)
	}
	id, _ := strconv.Atoi(os.Getenv("HYDO_CMD_ID"))
	seed, _ := strconv.ParseUint(os.Getenv("HYDO_SEED"), 10, 64)
	r := rand.New(rand.NewPCG(seed, uint64(id)))
	arg := func(i int) string {
		if len(os.Args) > i {
			return os.Args[i]
		}
		return ""
	}
	switch os.Args[1] {
	case "op":
		switch arg(2) {
		case "set":
			opSet(id, r)
		case "tx":
			opTx(id, r)
		case "px":
			opPx(id, r)
		case "save":
			opSave(id, r)
		case "takeover":
			opTakeover(id, r)
		case "read":
			opRead(r)
		default:
			fmt.Println("unknown op", arg(2))
			os.Exit(2)
		}
	case "settle":
		s, _ := strconv.Atoi(arg(2))
		if s == 0 {
			s = 60
		}
		if !settle(time.Duration(s) * time.Second) {
			os.Exit(1)
		}
	case "check":
		os.Exit(check(arg(2)))
	case "ready":
		os.Exit(ready(arg(2)))
	case "dump":
		d, t0, err := dump(arg(2))
		if err != nil {
			fmt.Println(err)
			os.Exit(2)
		}
		keys := make([]string, 0, len(d))
		for k := range d {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			fmt.Printf("%s %.40q %d\n", k, d[k].v, d[k].e)
		}
		fmt.Println(len(d), "keys at", t0)
	default:
		os.Exit(2)
	}
}
