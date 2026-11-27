// rqc is the test client of the rqlite example: the cluster's setup, the workload's writes and
// transactions, the reads during the run, the backups, the read-backs and the final checks. It
// speaks rqlite's HTTP API with net/http and sends each request once, without retries.
//
//	rqc ready <addr>      the API at <addr> answers, whatever it says (a readiness check)
//	rqc init              wait for one leader and three nodes, create the tables
//	rqc primary           print the current leader, waiting up to 15 s for one (the group's primary: command)
//	rqc op <kind>         one write of the history w; the composer's HYDO_* variables drive it
//	rqc txn               one transaction of the history la (list appends and reads)
//	rqc read              every node's tables at level none, against the leader's (anytime_read)
//	rqc backup            a backup through a node, against the acknowledged appends (anytime_backup)
//	rqc probe             every node's Raft state, recorded for check available (anytime_leader)
//	rqc readback          the final: of the messages check: each write's effect, read back
//	rqc lafinal           the final read of every list of the history la (list_append_la)
//	rqc check <rule>      one check: converge, serves, available
//	rqc raw <node> <method> <path> [<body>]   one request (for a look by hand)
//
// The nodes are rq1, rq2 and rq3 (10.89.0.11 to .13), with the API on port 4001. A request goes to
// a node that the seed picks; a follower forwards it to the leader, as rqlite does for every
// client that talks to any node.
//
// The history w (a messages check): writes of the table kv(id TEXT PRIMARY KEY, v TEXT), one id
// each: <letter>.<key>.<seq>.<value>. A key is a register: its value is the tag w<cmd> of the
// write that last set it, or "-" when the row does not exist.
//
//	insert  i  INSERT of a new key k<cmd> through /db/execute
//	upsert  r  INSERT OR REPLACE of a known key, deleted ones too
//	update  u  UPDATE of an existing key
//	delete  d  DELETE of an existing key
//	tx      t  2 or 3 of the writes above on other keys in one request with ?transaction: one
//	           send of all of their ids, each with t before its own letter (ti, tr, tu, td)
//	queue   q  INSERT of a new key through /db/execute?queue&wait: the queued write, acknowledged
//	           when rqlite says that the queue has committed it
//
// One command at a time works on a key (a lock file under /tmp/rqc/lock). A key's last known
// value and seq are in /tmp/rqc/keys/<key>, for the keys whose every write got a certain answer;
// a key whose write got no certain answer is never written again.
//
// Outcomes ($HYDO_ACKED, $HYDO_OUTCOMES "<id> ack|failed|unknown"): HTTP 200 without an error
// acknowledges; a statement's error (the transaction rolled back), 400, and 503 "leader not
// found" (no node was asked) fail; every other answer, an error in the body, a timeout and no
// answer are unknown.
//
// The history la (a list-append check at strict serializability): the table la(k, e), a list
// per key x0 to x7, read in rowid order. A transaction of 1 to 3 appends and reads goes to
// /db/request?transaction with level=linearizable (one in four with level=strong); a
// transaction with an append goes through the Raft log.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

const (
	port      = "4001"
	stateDir  = "/tmp/rqc"
	probeFile = "/tmp/rqc/probes"
	laKeys    = 8
)

var nodes = []string{"rq1", "rq2", "rq3"}

var addrs = map[string]string{"rq1": "10.89.0.11", "rq2": "10.89.0.12", "rq3": "10.89.0.13"}

func envDef(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func atoiDef(s string, d int) int {
	if v, err := strconv.Atoi(s); err == nil {
		return v
	}
	return d
}

func cmdID() int { return atoiDef(os.Getenv("HYDO_CMD_ID"), 0) }

func rng() *rand.Rand {
	s := uint64(atoiDef(os.Getenv("HYDO_SEED"), 0))
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	f.WriteString(line + "\n")
	f.Close()
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

// console writes one line to the guest's console, where reactions (A=) and the run's log see it.
func console(line string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(line + "\n")
}

func uptimeNs() string {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return "-"
	}
	f := strings.Fields(string(b))
	if len(f) == 0 {
		return "-"
	}
	return strings.Replace(f[0], ".", "", 1) + "0000000"
}

func uptimeMs() int64 {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return 0
	}
	f := strings.Fields(string(b))
	if len(f) == 0 {
		return 0
	}
	v, _ := strconv.ParseFloat(f[0], 64)
	return int64(v * 1000)
}

// hrec writes one history record: HYDO-HIST <history> <process> <type> <time> <args>.
func hrec(hist string, proc int, typ string, args ...string) {
	out := envDef("HYDO_HIST_OUT", "/dev/console")
	if out == "off" {
		return
	}
	f, err := os.OpenFile(out, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(fmt.Sprintf("HYDO-HIST %s %d %s %s %s\n", hist, proc, typ, uptimeNs(), strings.Join(args, " ")))
}

// event writes an SDK assertion record to the console, the event of a reaction
// (A=rq.write_acked+<ms>:cut, hydo explore --reactive-at rq.write_acked@20).
func event(id string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	fmt.Fprintf(f, "HYDO-ASSERT {\"t\":\"sometimes\",\"id\":%q,\"ok\":true}\n", id)
}

func enc(s string) string {
	var b strings.Builder
	for _, c := range []byte(s) {
		if c <= ' ' || strings.IndexByte("%@,[]:;=", c) >= 0 {
			fmt.Fprintf(&b, "%%%02X", c)
		} else {
			b.WriteByte(c)
		}
	}
	return b.String()
}

// ---- HTTP ----

type answer struct {
	status int
	body   []byte
	err    error
}

func (a answer) text() string {
	if a.err != nil {
		return a.err.Error()
	}
	s := strings.TrimSpace(string(a.body))
	if len(s) > 200 {
		s = s[:200] + "..."
	}
	return fmt.Sprintf("HTTP %d %s", a.status, s)
}

func call(node, method, path string, body []byte, timeout time.Duration) answer {
	cl := &http.Client{Timeout: timeout, Transport: &http.Transport{DisableKeepAlives: true},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	var rd io.Reader
	if body != nil {
		rd = bytes.NewReader(body)
	}
	host := node
	if a, ok := addrs[node]; ok {
		host = a
	}
	req, err := http.NewRequest(method, "http://"+host+":"+port+path, rd)
	if err != nil {
		return answer{err: err}
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	res, err := cl.Do(req)
	if err != nil {
		return answer{err: err}
	}
	defer res.Body.Close()
	b, err := io.ReadAll(res.Body)
	if err != nil {
		return answer{status: res.StatusCode, err: err}
	}
	return answer{status: res.StatusCode, body: b}
}

// result is one statement's result of /db/execute, /db/query or /db/request.
type result struct {
	Error        string  `json:"error"`
	RowsAffected int64   `json:"rows_affected"`
	Values       [][]any `json:"values"`
}

type resp struct {
	Results []result `json:"results"`
	Error   string   `json:"error"`
}

// sql sends statements (each a list: the SQL and its parameters) to path with the parameters ps.
func sql(node, path, ps string, stmts [][]any, timeout time.Duration) (answer, resp) {
	b, _ := json.Marshal(stmts)
	p := path
	if ps != "" {
		p += "?" + ps
	}
	a := call(node, "POST", p, b, timeout)
	var r resp
	if a.err == nil && a.status == 200 {
		if json.Unmarshal(a.body, &r) != nil {
			r.Error = "the answer is not JSON"
		}
	}
	return a, r
}

// outcome turns a write's answer into ack, failed or unknown, with a reason.
func outcome(a answer, r resp) (string, string) {
	switch {
	case a.err != nil:
		return "unknown", a.err.Error()
	case a.status == 200 && r.Error != "":
		return "unknown", r.Error
	case a.status == 200:
		for _, x := range r.Results {
			if x.Error != "" {
				return "failed", x.Error
			}
		}
		return "ack", ""
	case a.status == 400:
		return "failed", a.text()
	case a.status == 503 && strings.Contains(string(a.body), "leader not found"):
		return "failed", a.text()
	}
	return "unknown", a.text()
}

// state is a node's Raft state from /status ("Leader", "Follower", "Candidate", ...), or "-".
func state(node string) string {
	a := call(node, "GET", "/status", nil, 3*time.Second)
	if a.err != nil || a.status != 200 {
		return "-"
	}
	var d struct {
		Store struct {
			Raft struct {
				State string `json:"state"`
			} `json:"raft"`
		} `json:"store"`
	}
	if json.Unmarshal(a.body, &d) != nil || d.Store.Raft.State == "" {
		return "-"
	}
	return d.Store.Raft.State
}

func leader() string {
	for _, n := range nodes {
		if state(n) == "Leader" {
			return n
		}
	}
	return ""
}

// waitLeader waits until a node is the leader and answers a strong read.
func waitLeader(until time.Time) string {
	for {
		if l := leader(); l != "" {
			if _, r := sql(l, "/db/query", "level=strong", [][]any{{"SELECT 1"}}, 10*time.Second); r.Error == "" && len(r.Results) == 1 && r.Results[0].Error == "" {
				return l
			}
		}
		if time.Now().After(until) {
			return ""
		}
		time.Sleep(time.Second)
	}
}

// ---- the history w: writes of the table kv ----

var kindLetter = map[string]string{"insert": "i", "upsert": "r", "update": "u", "delete": "d", "tx": "t", "queue": "q"}

type op struct {
	kind, key string
	seq       int
	val       string
}

func (o op) id() string { return fmt.Sprintf("%s.%s.%d.%s", o.kind, o.key, o.seq, o.val) }

func parseID(id string) (op, bool) {
	f := strings.SplitN(id, ".", 4)
	if len(f) != 4 {
		return op{}, false
	}
	s, err := strconv.Atoi(f[2])
	if err != nil {
		return op{}, false
	}
	return op{f[0], f[1], s, f[3]}, true
}

type keyState struct {
	seq int
	val string
}

func lock(key string) bool {
	os.MkdirAll(filepath.Join(stateDir, "lock"), 0o755)
	f, err := os.OpenFile(filepath.Join(stateDir, "lock", key), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o644)
	if err != nil {
		return false
	}
	f.Close()
	return true
}

func unlock(key string) { os.Remove(filepath.Join(stateDir, "lock", key)) }

func loadState(key string) (keyState, bool) {
	b, err := os.ReadFile(filepath.Join(stateDir, "keys", key))
	if err != nil {
		return keyState{}, false
	}
	f := strings.Fields(string(b))
	if len(f) != 2 {
		return keyState{}, false
	}
	return keyState{atoiDef(f[0], 0), f[1]}, true
}

func saveState(key string, s keyState) {
	os.MkdirAll(filepath.Join(stateDir, "keys"), 0o755)
	os.WriteFile(filepath.Join(stateDir, "keys", key), []byte(fmt.Sprintf("%d %s", s.seq, s.val)), 0o644)
}

func retire(key string) { os.Remove(filepath.Join(stateDir, "keys", key)) }

// pickKey locks a known key; exists true asks for a key whose row exists.
func pickKey(r *rand.Rand, exists bool, not map[string]bool) (string, keyState) {
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var ks []string
	for _, e := range ents {
		ks = append(ks, e.Name())
	}
	sort.Strings(ks)
	r.Shuffle(len(ks), func(i, j int) { ks[i], ks[j] = ks[j], ks[i] })
	for _, k := range ks {
		if not[k] {
			continue
		}
		s, ok := loadState(k)
		if !ok || (exists && s.val == "-") {
			continue
		}
		if lock(k) {
			if s2, ok := loadState(k); ok && (!exists || s2.val != "-") {
				return k, s2
			}
			unlock(k)
		}
	}
	return "", keyState{}
}

func begin(ops []op) {
	var ids []string
	for _, o := range ops {
		appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" unknown")
		ids = append(ids, o.id())
	}
	hrec("w", cmdID(), "invoke", append([]string{"send", "w"}, ids...)...)
}

func finish(ops []op, oc, why string, before map[string]keyState) {
	var ids []string
	for _, o := range ops {
		appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" "+oc)
		ids = append(ids, o.id())
		switch oc {
		case "ack":
			appendLine(os.Getenv("HYDO_ACKED"), o.id())
			saveState(o.key, keyState{o.seq, o.val})
		case "failed":
			if s, ok := before[o.key]; ok {
				saveState(o.key, s)
			}
		default:
			retire(o.key)
		}
	}
	t := map[string]string{"ack": "ok", "failed": "fail", "unknown": "info"}[oc]
	hrec("w", cmdID(), t, append([]string{"send", "w"}, ids...)...)
	if oc == "ack" {
		event("rq.write_acked")
	}
	if oc != "ack" {
		fmt.Fprintf(os.Stderr, "rqc: %s: %s: %s\n", strings.Join(ids, " "), oc, why)
	}
}

// stmt is the SQL of one write.
func stmt(o op) []any {
	switch strings.TrimPrefix(o.kind, "t") {
	case "i", "q":
		return []any{"INSERT INTO kv(id, v) VALUES(?, ?)", o.key, o.val}
	case "r":
		return []any{"INSERT OR REPLACE INTO kv(id, v) VALUES(?, ?)", o.key, o.val}
	case "u":
		return []any{"UPDATE kv SET v = ? WHERE id = ?", o.val, o.key}
	}
	return []any{"DELETE FROM kv WHERE id = ?", o.key}
}

func runOp(kind string) int {
	r := rng()
	c := cmdID()
	node := nodes[r.IntN(len(nodes))]
	tag := fmt.Sprintf("w%d", c)
	before := map[string]keyState{}
	var ops []op
	var locked []string
	defer func() {
		for _, k := range locked {
			unlock(k)
		}
	}()
	one := func(k string, exclude map[string]bool) (op, bool) {
		switch k {
		case "insert", "queue":
			key := fmt.Sprintf("k%d", c)
			if exclude[key] {
				key = fmt.Sprintf("k%dx%d", c, len(exclude))
			}
			if !lock(key) {
				return op{}, false
			}
			locked = append(locked, key)
			return op{kindLetter[k], key, 1, tag}, true
		case "upsert", "update", "delete":
			key, s := pickKey(r, k != "upsert", exclude)
			if key == "" {
				return op{}, false
			}
			locked = append(locked, key)
			before[key] = s
			v := tag
			if k == "delete" {
				v = "-"
			}
			return op{kindLetter[k], key, s.seq + 1, v}, true
		}
		return op{}, false
	}
	params := "timeout=10s"
	path := "/db/execute"
	switch kind {
	case "tx":
		n := 2 + r.IntN(2)
		ex := map[string]bool{}
		for i := 0; i < n; i++ {
			k := []string{"insert", "upsert", "update", "delete"}[r.IntN(4)]
			if o, ok := one(k, ex); ok {
				o.kind = "t" + o.kind
				ops = append(ops, o)
				ex[o.key] = true
			}
		}
		if len(ops) < 2 {
			return 0
		}
		params += "&transaction"
	case "queue":
		o, ok := one("queue", nil)
		if !ok {
			return 0
		}
		ops = append(ops, o)
		params = "queue&wait&timeout=20s"
	default:
		o, ok := one(kind, nil)
		if !ok {
			return 0
		}
		ops = append(ops, o)
	}
	var stmts [][]any
	for _, o := range ops {
		stmts = append(stmts, stmt(o))
	}
	begin(ops)
	a, rs := sql(node, path, params, stmts, 25*time.Second)
	oc, why := outcome(a, rs)
	finish(ops, oc, why+" (through "+node+")", before)
	return 0
}

// ---- the history la: list appends and reads in transactions ----

func laRead(k int) []any { return []any{"SELECT e FROM la WHERE k = ? ORDER BY rowid", k} }

func listOf(x result) []int64 {
	var out []int64
	for _, row := range x.Values {
		if len(row) == 1 {
			if f, ok := row[0].(float64); ok {
				out = append(out, int64(f))
			}
		}
	}
	return out
}

func fmtList(l []int64) string {
	var s []string
	for _, e := range l {
		s = append(s, strconv.FormatInt(e, 10))
	}
	return "[" + strings.Join(s, ",") + "]"
}

func runTxn() int {
	r := rng()
	c := cmdID()
	node := nodes[r.IntN(len(nodes))]
	n := 1 + r.IntN(3)
	type mop struct {
		app  bool
		k    int
		e    int64
		read []int64
	}
	var ms []mop
	for i := 0; i < n; i++ {
		m := mop{app: r.IntN(2) == 0, k: r.IntN(laKeys)}
		if m.app {
			m.e = int64(c*4 + i + 1)
		}
		ms = append(ms, m)
	}
	level := "linearizable"
	if r.IntN(4) == 0 {
		level = "strong"
	}
	var stmts [][]any
	var inv []string
	for _, m := range ms {
		if m.app {
			stmts = append(stmts, []any{"INSERT INTO la(k, e) VALUES(?, ?)", m.k, m.e})
			inv = append(inv, fmt.Sprintf("a:x%d:%d", m.k, m.e))
		} else {
			stmts = append(stmts, laRead(m.k))
			inv = append(inv, fmt.Sprintf("r:x%d", m.k))
		}
	}
	hrec("la", c, "invoke", append([]string{"txn"}, inv...)...)
	a, rs := sql(node, "/db/request", "transaction&timeout=10s&level="+level, stmts, 25*time.Second)
	oc, why := outcome(a, rs)
	if oc == "ack" && len(rs.Results) != len(ms) {
		oc, why = "unknown", fmt.Sprintf("%d results for %d statements", len(rs.Results), len(ms))
	}
	switch oc {
	case "ack":
		var out []string
		for i, m := range ms {
			if m.app {
				out = append(out, fmt.Sprintf("a:x%d:%d", m.k, m.e))
				appendLine(filepath.Join(stateDir, "la-acked"), fmt.Sprintf("%d %d", m.k, m.e))
			} else {
				out = append(out, fmt.Sprintf("r:x%d:%s", m.k, fmtList(listOf(rs.Results[i]))))
			}
		}
		hrec("la", c, "ok", append([]string{"txn"}, out...)...)
		event("rq.txn_acked")
	case "failed":
		hrec("la", c, "fail", append([]string{"txn"}, inv...)...)
		fmt.Fprintf(os.Stderr, "rqc: txn failed through %s: %s\n", node, why)
	default:
		hrec("la", c, "info", append([]string{"txn"}, inv...)...)
		fmt.Fprintf(os.Stderr, "rqc: txn unknown through %s: %s\n", node, why)
	}
	return 0
}

// lists reads every list of la, and every row of kv, from a node at a level.
func tables(node, level string, timeout time.Duration) (map[int][]int64, map[string]string, error) {
	stmts := [][]any{}
	for k := 0; k < laKeys; k++ {
		stmts = append(stmts, laRead(k))
	}
	stmts = append(stmts, []any{"SELECT id, v FROM kv ORDER BY id"})
	ps := "level=" + level
	if level != "none" {
		ps += "&timeout=10s"
	}
	a, rs := sql(node, "/db/query", ps, stmts, timeout)
	if a.err != nil || a.status != 200 {
		return nil, nil, fmt.Errorf("%s: %s", node, a.text())
	}
	if rs.Error != "" || len(rs.Results) != laKeys+1 {
		return nil, nil, fmt.Errorf("%s: %s (%d results)", node, rs.Error, len(rs.Results))
	}
	la := map[int][]int64{}
	for k := 0; k < laKeys; k++ {
		if rs.Results[k].Error != "" {
			return nil, nil, fmt.Errorf("%s: %s", node, rs.Results[k].Error)
		}
		la[k] = listOf(rs.Results[k])
	}
	kv := map[string]string{}
	if rs.Results[laKeys].Error != "" {
		return nil, nil, fmt.Errorf("%s: %s", node, rs.Results[laKeys].Error)
	}
	for _, row := range rs.Results[laKeys].Values {
		if len(row) == 2 {
			kv[fmt.Sprint(row[0])] = fmt.Sprint(row[1])
		}
	}
	return la, kv, nil
}

func isPrefix(a, b []int64) bool {
	if len(a) > len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

// readNodes reads every node's lists at level none, then the leader's at level strong: each
// node's list must be a prefix of the leader's later list, because a node applies only
// committed entries, in log order (anytime_read; exit 100 is a violation).
func readNodes() int {
	got := map[string]map[int][]int64{}
	for _, n := range nodes {
		la, _, err := tables(n, "none", 5*time.Second)
		if err == nil {
			got[n] = la
		}
	}
	l := leader()
	if l == "" {
		return 0
	}
	want, _, err := tables(l, "strong", 15*time.Second)
	if err != nil {
		return 0
	}
	bad := 0
	for _, n := range nodes {
		la, ok := got[n]
		if !ok {
			continue
		}
		for k := 0; k < laKeys; k++ {
			if !isPrefix(la[k], want[k]) {
				if bad++; bad <= 5 {
					fmt.Printf("%s read x%d as %s, which is not a prefix of the leader %s's later %s\n", n, k, fmtList(la[k]), l, fmtList(want[k]))
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("a node's list was not a prefix of the leader's later list (%d lists)\n", bad)
		return 100
	}
	return 0
}

// backup takes a backup in SQL through a node that the seed picks and checks that it holds
// every append that was acknowledged before the request (anytime_backup; exit 100 is a violation).
func backup() int {
	r := rng()
	node := nodes[r.IntN(len(nodes))]
	acked := readLines(filepath.Join(stateDir, "la-acked"))
	a := call(node, "GET", "/db/backup?fmt=sql", nil, 20*time.Second)
	if a.err != nil || a.status != 200 {
		fmt.Fprintf(os.Stderr, "rqc: backup through %s: %s\n", node, a.text())
		return 0
	}
	re := regexp.MustCompile(`(?m)^INSERT INTO "?la"? VALUES\((\d+),(\d+)\);`)
	have := map[string]bool{}
	for _, m := range re.FindAllStringSubmatch(string(a.body), -1) {
		have[m[1]+" "+m[2]] = true
	}
	var miss []string
	for _, l := range acked {
		if !have[l] {
			miss = append(miss, l)
		}
	}
	if len(miss) > 0 {
		fmt.Printf("the backup through %s lacks the acknowledged appends (key element) %s\n", node, strings.Join(miss[:min(10, len(miss))], ", "))
		fmt.Printf("a backup lacks %d of %d appends acknowledged before it\n", len(miss), len(acked))
		return 100
	}
	console(fmt.Sprintf("rqc: backup through %s holds the %d acknowledged appends (%d bytes)", node, len(acked), len(a.body)))
	return 0
}

// laFinal reads every list from the leader at level strong and records one read of all keys
// (the final read-back of the check list_append_la).
func laFinal() int {
	l := waitLeader(time.Now().Add(150 * time.Second))
	if l == "" {
		fmt.Println("no leader answered a strong read within 150 s")
		return 2
	}
	var inv, out []string
	for k := 0; k < laKeys; k++ {
		inv = append(inv, fmt.Sprintf("r:x%d", k))
	}
	const proc = 100001
	hrec("la", proc, "invoke", append([]string{"txn"}, inv...)...)
	la, _, err := tables(l, "strong", 20*time.Second)
	if err != nil {
		hrec("la", proc, "info", append([]string{"txn"}, inv...)...)
		fmt.Printf("the final read failed: %v\n", err)
		return 2
	}
	for k := 0; k < laKeys; k++ {
		out = append(out, fmt.Sprintf("r:x%d:%s", k, fmtList(la[k])))
	}
	hrec("la", proc, "ok", append([]string{"txn"}, out...)...)
	return 0
}

// ---- probes and availability ----

func probe() int {
	end := time.Now().Add(12 * time.Second)
	for time.Now().Before(end) {
		t := uptimeMs()
		alive, leaders := 0, 0
		var st []string
		for _, n := range nodes {
			s := state(n)
			if s != "-" {
				alive++
			}
			if s == "Leader" {
				leaders++
			}
			st = append(st, n+"="+s)
		}
		os.MkdirAll(stateDir, 0o755)
		appendLine(probeFile, fmt.Sprintf("%d %d %d %s", t, alive, leaders, strings.Join(st, ",")))
		time.Sleep(time.Second)
	}
	return 0
}

// checkAvailable: while two or more nodes answered, the cluster was never without a leader for
// more than 30 s. rqlite's election timeout is 1 s.
func checkAvailable() int {
	type pr struct {
		t             int64
		alive, leader int
		st            string
	}
	var ps []pr
	for _, l := range readLines(probeFile) {
		f := strings.Fields(l)
		if len(f) < 4 {
			continue
		}
		t, _ := strconv.ParseInt(f[0], 10, 64)
		ps = append(ps, pr{t, atoiDef(f[1], 0), atoiDef(f[2], 0), f[3]})
	}
	sort.Slice(ps, func(i, j int) bool { return ps[i].t < ps[j].t })
	var worst, from, to int64
	var at string
	start := int64(-1)
	for _, p := range ps {
		if p.alive >= 2 && p.leader == 0 {
			if start < 0 {
				start = p.t
			}
			if d := p.t - start; d > worst {
				worst, from, to, at = d, start, p.t, p.st
			}
		} else {
			start = -1
		}
	}
	fmt.Printf("%d probes of the leader\n", len(ps))
	if worst > 30000 {
		fmt.Printf("from %.1f s to %.1f s of uptime no node was the leader while two or more answered (%s)\n", float64(from)/1000, float64(to)/1000, at)
		fmt.Printf("no leader for %d s while two or more nodes answered\n", worst/1000)
		return 1
	}
	return 0
}

// ---- the read-back of the history w ----

func opsByKey() (map[string][]op, map[string]string) {
	by := map[string][]op{}
	oc := map[string]string{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) != 2 {
			continue
		}
		o, ok := parseID(f[0])
		if !ok {
			continue
		}
		if _, seen := oc[f[0]]; !seen {
			by[o.key] = append(by[o.key], o)
		}
		oc[f[0]] = f[1]
	}
	for k := range by {
		sort.SliceStable(by[k], func(i, j int) bool { return by[k][i].seq < by[k][j].seq })
	}
	return by, oc
}

func readBack() int {
	const proc = 100000
	by, oc := opsByKey()
	l := waitLeader(time.Now().Add(150 * time.Second))
	if l == "" {
		fmt.Println("no read-back: no leader answered a strong read within 150 s")
		return 0
	}
	_, have, err := tables(l, "strong", 20*time.Second)
	if err != nil {
		fmt.Printf("no read-back: %v\n", err)
		return 0
	}
	var keys []string
	for k := range by {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var ids, items []string
	odd := 0
	for _, k := range keys {
		ops := by[k]
		v, ok := have[k]
		if !ok {
			v = "-"
		}
		lastAck := -1
		for i, o := range ops {
			if oc[o.id()] == "ack" {
				lastAck = i
			}
		}
		cand := map[string]bool{}
		if lastAck >= 0 {
			cand[ops[lastAck].val] = true
		} else {
			cand["-"] = true
		}
		for i, o := range ops {
			if i > lastAck && oc[o.id()] == "unknown" {
				cand[o.val] = true
			}
		}
		for i, o := range ops {
			it := o.id()
			switch oc[o.id()] {
			case "ack":
				it += ";s=ok"
				if i == lastAck {
					if v == o.val {
						it += ";f=1"
					} else if !cand[v] {
						it += ";f=0;e=" + enc("the row holds "+v)
					}
				}
			case "failed":
				it += ";s=fail;e=refused"
				if v == o.val && o.val != "-" && !cand[v] {
					it += ";f=1"
				}
			default:
				it += ";s=info"
			}
			ids, items = append(ids, o.id()), append(items, it)
		}
		if !cand[v] {
			var cs []string
			for x := range cand {
				cs = append(cs, x)
			}
			sort.Strings(cs)
			if odd++; odd <= 10 {
				fmt.Printf("%s holds %s, which no acknowledged or unknown write left there (expected %s)\n", k, v, strings.Join(cs, " or "))
			}
		}
	}
	for k, v := range have {
		if _, ok := by[k]; !ok {
			if odd++; odd <= 10 {
				fmt.Printf("%s holds %s, and no write sent %s\n", k, v, k)
			}
		}
	}
	for i := 0; i < len(items); i += 20 {
		j := min(i+20, len(items))
		hrec("w", proc, "invoke", append([]string{"status", "w"}, ids[i:j]...)...)
		hrec("w", proc, "ok", append([]string{"status", "w"}, items[i:j]...)...)
	}
	fmt.Printf("read back %d writes of %d rows from the leader %s\n", len(items), len(have), l)
	return 0
}

// ---- the checks ----

// barrier writes a row through the leader, so that a node that holds it has applied every
// earlier write.
func barrier(l string) (string, error) {
	id := fmt.Sprintf("b%d", uptimeMs())
	a, rs := sql(l, "/db/execute", "timeout=10s", [][]any{{"INSERT OR REPLACE INTO sv(id, node) VALUES(?, ?)", id, "barrier"}}, 15*time.Second)
	if oc, why := outcome(a, rs); oc != "ack" {
		return "", fmt.Errorf("the barrier write through %s: %s", l, why)
	}
	return id, nil
}

func hasRow(node, id string) bool {
	_, rs := sql(node, "/db/query", "level=none", [][]any{{"SELECT count(*) FROM sv WHERE id = ?", id}}, 5*time.Second)
	if rs.Error != "" || len(rs.Results) != 1 || len(rs.Results[0].Values) != 1 {
		return false
	}
	f, _ := rs.Results[0].Values[0][0].(float64)
	return f == 1
}

// checkConverge: within the check's deadline every node holds the leader's lists and rows.
func checkConverge() int {
	l := waitLeader(time.Now().Add(20 * time.Second))
	if l == "" {
		fmt.Println("no leader")
		return 2
	}
	id, err := barrier(l)
	if err != nil {
		fmt.Println(err)
		return 2
	}
	wantLa, wantKv, err := tables(l, "strong", 20*time.Second)
	if err != nil {
		fmt.Println(err)
		return 2
	}
	bad := 0
	m, err := members(l)
	if err != nil {
		fmt.Println(err)
		return 2
	}
	for _, n := range nodes {
		if r, ok := m[n]; !ok || !r {
			fmt.Printf("the leader %s does not list %s as a reachable voter (listed %v, reachable %v)\n", l, n, ok, r)
			bad++
		}
	}
	for _, n := range nodes {
		if n == l {
			continue
		}
		if !hasRow(n, id) {
			fmt.Printf("%s has not applied the barrier write\n", n)
			bad++
			continue
		}
		la, kv, err := tables(n, "none", 10*time.Second)
		if err != nil {
			fmt.Println(err)
			bad++
			continue
		}
		for k := 0; k < laKeys; k++ {
			if !isPrefix(wantLa[k], la[k]) || len(la[k]) != len(wantLa[k]) {
				if bad++; bad <= 8 {
					fmt.Printf("%s holds x%d as %s, and the leader %s as %s\n", n, k, fmtList(la[k]), l, fmtList(wantLa[k]))
				}
			}
		}
		var ks []string
		for k := range wantKv {
			ks = append(ks, k)
		}
		for k := range kv {
			if _, ok := wantKv[k]; !ok {
				ks = append(ks, k)
			}
		}
		sort.Strings(ks)
		for _, k := range ks {
			if kv[k] != wantKv[k] {
				if bad++; bad <= 8 {
					fmt.Printf("%s holds %s = %q, and the leader %s %q\n", n, k, kv[k], l, wantKv[k])
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("the nodes differ from the leader in %d places\n", bad)
		return 1
	}
	fmt.Printf("every node holds the leader %s's %d rows and lists\n", l, len(wantKv))
	return 0
}

// ---- node removal and rejoin ----

// members is the leader's view of the voters, from /nodes: id -> reachable.
func members(l string) (map[string]bool, error) {
	a := call(l, "GET", "/nodes?ver=2", nil, 10*time.Second)
	if a.err != nil || a.status != 200 {
		return nil, fmt.Errorf("/nodes of %s: %s", l, a.text())
	}
	var d struct {
		Nodes []struct {
			ID        string `json:"id"`
			Reachable bool   `json:"reachable"`
			Voter     bool   `json:"voter"`
		} `json:"nodes"`
	}
	if err := json.Unmarshal(a.body, &d); err != nil {
		return nil, fmt.Errorf("/nodes of %s: %v", l, err)
	}
	m := map[string]bool{}
	for _, n := range d.Nodes {
		if n.Voter {
			m[n.ID] = n.Reachable
		}
	}
	return m, nil
}

// killNode sends SIGKILL to the rqlited process of a node; its restart policy starts it again.
func killNode(n string) bool { return signalNode(n, syscall.SIGKILL) }

// queuedThenStop sends 2 or 3 queued writes without wait (?queue) to a node that the seed
// picks, each a new row, and then asks the node to shut down gracefully (SIGTERM), as an
// operator's restart or an upgrade does; the restart policy starts it again. rqlite answers a
// queued write when it is queued and commits it within -write-queue-timeout (50 ms); the queued
// writes trade durability against a crash, and this asks whether a graceful stop keeps them.
// Each write is a send of the history w of the kind s, acknowledged by rqlite's 200. At most once
// per run (serial_driver_qstop), and only with RQC_QSTOP=yes (the build parameter qstop).
func queuedThenStop() int {
	r := rng()
	if os.Getenv("RQC_QSTOP") != "yes" || r.IntN(3) != 0 || !lock("qstop-done") {
		return 0
	}
	c := cmdID()
	node := nodes[r.IntN(len(nodes))]
	n := 2 + r.IntN(2)
	for i := 0; i < n; i++ {
		key := fmt.Sprintf("k%dq%d", c, i)
		if !lock(key) {
			continue
		}
		o := op{"s", key, 1, fmt.Sprintf("w%d", c)}
		begin([]op{o})
		a, rs := sql(node, "/db/execute", "queue", [][]any{stmt(op{"i", key, 1, o.val})}, 10*time.Second)
		oc, why := outcome(a, rs)
		finish([]op{o}, oc, why+" (queued through "+node+")", nil)
		unlock(key)
	}
	ok := signalNode(node, syscall.SIGTERM)
	console(fmt.Sprintf("rqc: SIGTERM to %s after %d queued writes: %v", node, n, ok))
	event("rq.queued_then_stopped")
	return 0
}

func signalNode(n string, sig syscall.Signal) bool {
	ents, _ := os.ReadDir("/proc")
	for _, e := range ents {
		pid, err := strconv.Atoi(e.Name())
		if err != nil {
			continue
		}
		b, err := os.ReadFile(filepath.Join("/proc", e.Name(), "cmdline"))
		if err != nil {
			continue
		}
		args := strings.Split(string(b), "\x00")
		if len(args) == 0 || !strings.HasSuffix(args[0], "rqlited") {
			continue
		}
		for i := 0; i+1 < len(args); i++ {
			if args[i] == "-node-id" && args[i+1] == n {
				return syscall.Kill(pid, sig) == nil
			}
		}
	}
	return false
}

// removeRejoin removes a follower with DELETE /remove, as the clustering guide does for a node
// that goes away, then ends its process: the node starts again with the same -join and
// -bootstrap-expect flags and joins the cluster again. At most once per run (serial_driver_remove).
// The removal must take effect within 20 s (exit 100 is a violation); the check converge asks
// that the node is a reachable voter again after the heal. The driver does not wait for the
// rejoin itself: a round of the composer waits for its commands, so a wait here would stretch
// the steps and every pause in them.
func removeRejoin() int {
	r := rng()
	if r.IntN(3) != 0 || !lock("remove-done") {
		return 0
	}
	l := leader()
	if l == "" {
		return 0
	}
	var fs []string
	for _, n := range nodes {
		if n != l {
			fs = append(fs, n)
		}
	}
	f := fs[r.IntN(len(fs))]
	via := nodes[r.IntN(len(nodes))]
	body, _ := json.Marshal(map[string]string{"id": f})
	a := call(via, "DELETE", "/remove", body, 15*time.Second)
	console(fmt.Sprintf("rqc: remove %s through %s: %s", f, via, a.text()))
	if a.err == nil && a.status >= 400 && a.status < 500 {
		return 0 // refused: nothing was removed
	}
	if a.err != nil || a.status != 200 {
		// The removal's outcome is unknown: it may commit later. End the node's process
		// anyway, so that it starts again and joins again if it was removed.
		if killNode(f) {
			console("rqc: ended " + f + " after a removal of unknown outcome; it starts again with -join")
		}
		return 0
	}
	event("rq.removed")
	until := time.Now().Add(20 * time.Second)
	for {
		if m, err := members(waitLeader(time.Now().Add(10 * time.Second))); err == nil {
			if _, ok := m[f]; !ok {
				break
			}
		}
		if time.Now().After(until) {
			fmt.Printf("%s is still a voter 20 s after its removal was acknowledged\n", f)
			return 100
		}
		time.Sleep(time.Second)
	}
	if !killNode(f) {
		console("rqc: no process of " + f + " to end")
		return 0
	}
	console("rqc: ended " + f + " after its removal; it starts again with -join")
	event("rq.restarted_after_removal")
	return 0
}

// checkServes: a write through each node is acknowledged and then readable on every node.
func checkServes() int {
	c := uptimeMs()
	bad := 0
	for _, n := range nodes {
		id := fmt.Sprintf("s%d%s", c, n)
		a, rs := sql(n, "/db/execute", "timeout=10s", [][]any{{"INSERT OR REPLACE INTO sv(id, node) VALUES(?, ?)", id, n}}, 15*time.Second)
		if oc, why := outcome(a, rs); oc != "ack" {
			fmt.Printf("a write through %s: %s\n", n, why)
			bad++
			continue
		}
		until := time.Now().Add(15 * time.Second)
		for _, m := range nodes {
			for !hasRow(m, id) {
				if time.Now().After(until) {
					fmt.Printf("%s does not hold the row %s that %s acknowledged\n", m, id, n)
					bad++
					break
				}
				time.Sleep(500 * time.Millisecond)
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d of the writes through each node or their reads failed\n", bad)
		return 1
	}
	return 0
}

func cmdReady(addr string) int {
	cl := &http.Client{Timeout: 3 * time.Second}
	res, err := cl.Get("http://" + addr + ":" + port + "/status")
	if err != nil {
		return 1
	}
	res.Body.Close()
	return 0
}

func cmdInit() int {
	l := waitLeader(time.Now().Add(170 * time.Second))
	if l == "" {
		fmt.Println("no leader within 170 s")
		return 1
	}
	until := time.Now().Add(60 * time.Second)
	for {
		a := call(l, "GET", "/nodes", nil, 5*time.Second)
		if a.err == nil && a.status == 200 && strings.Count(string(a.body), `"reachable":true`) >= 3 {
			break
		}
		if time.Now().After(until) {
			fmt.Printf("not three reachable nodes: %s\n", a.text())
			return 1
		}
		time.Sleep(time.Second)
	}
	a, rs := sql(l, "/db/execute", "timeout=10s", [][]any{
		{"CREATE TABLE IF NOT EXISTS kv (id TEXT PRIMARY KEY, v TEXT)"},
		{"CREATE TABLE IF NOT EXISTS la (k INTEGER, e INTEGER)"},
		{"CREATE TABLE IF NOT EXISTS sv (id TEXT PRIMARY KEY, node TEXT)"},
	}, 15*time.Second)
	if oc, why := outcome(a, rs); oc != "ack" {
		fmt.Printf("create tables: %s\n", why)
		return 1
	}
	fmt.Printf("leader %s, tables created\n", l)
	return 0
}

func cmdPrimary() int {
	until := time.Now().Add(15 * time.Second)
	for {
		if l := leader(); l != "" {
			fmt.Println(l)
			return 0
		}
		if time.Now().After(until) {
			return 1
		}
		time.Sleep(time.Second)
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: rqc ready|init|primary|op|txn|read|backup|probe|readback|lafinal|check|raw ...")
		os.Exit(2)
	}
	os.MkdirAll(stateDir, 0o755)
	a := os.Args[2:]
	rc := 0
	switch os.Args[1] {
	case "ready":
		rc = cmdReady(a[0])
	case "init":
		rc = cmdInit()
	case "primary":
		rc = cmdPrimary()
	case "op":
		rc = runOp(a[0])
	case "txn":
		rc = runTxn()
	case "read":
		rc = readNodes()
	case "backup":
		rc = backup()
	case "probe":
		rc = probe()
	case "readback":
		rc = readBack()
	case "remove":
		rc = removeRejoin()
	case "qstop":
		rc = queuedThenStop()
	case "lafinal":
		rc = laFinal()
	case "check":
		switch a[0] {
		case "converge":
			rc = checkConverge()
		case "serves":
			rc = checkServes()
		case "available":
			rc = checkAvailable()
		default:
			rc = 2
		}
	case "raw":
		var body []byte
		if len(a) > 3 {
			body = []byte(a[3])
		}
		r := call(a[0], a[1], a[2], body, 30*time.Second)
		fmt.Println(r.text())
		if r.err == nil {
			fmt.Println(string(r.body))
		}
	default:
		rc = 2
	}
	os.Exit(rc)
}
