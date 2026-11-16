// sdbc is the test client of the SurrealDB example: the workload's operations, the read-backs of
// the history checks, and the final checks. It speaks SurrealQL to the server's HTTP endpoint
// /sql and uses Go's standard library and hydo's history recorder only.
//
//	sdbc ready            exit 0 when the server answers a query
//	sdbc init             the namespace, the database, the tables and their first rows (first_init)
//	sdbc op <kind>        one client operation; the composer's HYDO_* variables drive it
//	sdbc readback <hist>  read every key back and record the reads (the history checks' final read)
//	sdbc check <rule>     one final or always rule over the client's records and the server's data
//	sdbc q '<surql>'      run one query and print the answer (for a look by hand)
//
// The server is SDB_URL (default http://127.0.0.1:8000), namespace and database "hydo".
//
// Outcomes. A request without an answer (a timeout, a refused or broken connection) is unknown.
// An answer whose statements all say OK is acknowledged. An error that SurrealDB says leaves the
// commit's outcome unknown ("The transaction's commit outcome is unknown") is unknown. Every
// other error is failed: SurrealDB's documentation says that such a transaction applied nothing
// ("A transaction always commits or cancels as a whole").
//
// The tables, each with a changefeed:
//
//	la:k0..la:k7   {v: [elements]}  list-append transactions (history "la")
//	reg:k0..reg:k4 {v: value}       registers: read, write, compare-and-set (history "reg")
//	acct:0..acct:7 {bal: n}         bank accounts, 100 each; transfers keep the total at 800
//	blob:<id>      {n, pad}         one record per operation, its pad 1 to 20 KiB (values above
//	                                4 KiB go to SurrealKV's value log)
//	ev:<id>, mark:<id>              an event; its ASYNC event upserts mark:<id> (n counts the runs)
//	u:<id>         {email, grp}     users; a UNIQUE index on email and a plain index on grp
//	person:0..5, follows:e<id>      a graph: RELATE person:a->follows:e<id>->person:b
//	lq:l<id>_<k>                    records a live query watches (opLive, over the WebSocket /rpc)
//
// $HYDO_ACKED gets b<id>:<n>:<t0>:<t1> for every acknowledged blob, where t0 and t1 are the guest's
// monotonic clock in ns before the request and after the answer.
package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydohist"
)

const (
	nLA   = 8
	nReg  = 5
	nAcct   = 8
	start   = 100
	nPerson = 6  // person:0..5, the graph's vertices
	nEmail  = 24 // the pool of e-mail addresses of the unique index
	nGrp    = 4  // the values of the plain index
)

var (
	base   = env("SDB_URL", "http://127.0.0.1:8000")
	client = &http.Client{Timeout: time.Duration(atoiDef(os.Getenv("SDB_TIMEOUT_S"), 10)) * time.Second}
)

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func atoiDef(s string, d int) int {
	if n, err := strconv.Atoi(s); err == nil {
		return n
	}
	return d
}

// mono is the guest's CLOCK_MONOTONIC in ns: deterministic under hydo, and not moved by clock steps.
func mono() int64 {
	var ts syscall.Timespec
	if err := clockGettime(&ts); err != nil {
		return -1
	}
	return ts.Nano()
}

// ---- the HTTP endpoint ----

type stmt struct {
	Status string          `json:"status"`
	Result json.RawMessage `json:"result"`
}

// errNoAnswer is a request without an answer: its outcome is unknown.
var errNoAnswer = errors.New("no answer")

// query sends one request and returns each statement's answer. An error wrapping errNoAnswer
// means the outcome is unknown.
func query(q string) ([]stmt, error) {
	req, err := http.NewRequest("POST", base+"/sql", strings.NewReader(q))
	if err != nil {
		return nil, err
	}
	req.SetBasicAuth("root", "root")
	req.Header.Set("Accept", "application/json")
	req.Header.Set("surreal-ns", "hydo")
	req.Header.Set("surreal-db", "hydo")
	resp, err := client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", errNoAnswer, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("%w: reading the answer: %v", errNoAnswer, err)
	}
	if resp.StatusCode != 200 {
		// The server refused the request as a whole, or failed while it ran: unknown, since a
		// 5xx can come after a commit.
		return nil, fmt.Errorf("%w: HTTP %d: %s", errNoAnswer, resp.StatusCode, trunc(string(body), 300))
	}
	var out []stmt
	if err := json.Unmarshal(body, &out); err != nil {
		return nil, fmt.Errorf("%w: answer is not JSON: %s", errNoAnswer, trunc(string(body), 300))
	}
	return out, nil
}

func trunc(s string, n int) string {
	if len(s) > n {
		return s[:n] + "..."
	}
	return s
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

func (o outcome) String() string { return [...]string{"ack", "failed", "unknown"}[o] }

// judge decides a request's outcome from its answer, and gives the first error's text.
func judge(res []stmt, err error, want int) (outcome, string) {
	if err != nil {
		return unknown, err.Error()
	}
	if len(res) != want {
		return unknown, fmt.Sprintf("%d answers for %d statements", len(res), want)
	}
	first := ""
	for _, s := range res {
		if s.Status != "OK" {
			var msg string
			if json.Unmarshal(s.Result, &msg) != nil {
				msg = string(s.Result)
			}
			if strings.Contains(msg, "outcome is unknown") {
				return unknown, msg
			}
			if first == "" || strings.Contains(first, "not executed") || strings.Contains(first, "Cannot COMMIT") {
				first = msg
			}
		}
	}
	if first != "" {
		return failed, first
	}
	return ack, ""
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0644)
	if err != nil {
		return
	}
	f.WriteString(line + "\n")
	f.Close()
}

func record(id string, o outcome) { appendLine(os.Getenv("HYDO_OUTCOMES"), id+" "+o.String()) }

// ---- init ----

// feeds says whether the tables have changefeeds and ev its ASYNC event (SDB_FEEDS, default yes).
// Both mint HLC stamps on every write, which a backward clock step stalls (RESULTS.md).
func feeds() bool { return os.Getenv("SDB_FEEDS") != "no" }

func cmdInit() int {
	var b strings.Builder
	b.WriteString("DEFINE NAMESPACE IF NOT EXISTS hydo; USE NS hydo; DEFINE DATABASE IF NOT EXISTS hydo; USE NS hydo DB hydo;")
	feed := " CHANGEFEED 3650d"
	event := " DEFINE EVENT IF NOT EXISTS mark ON TABLE ev ASYNC WHEN $event = \"CREATE\" THEN { UPSERT type::record(\"mark\", record::id($after.id)) SET n += 1 };"
	if !feeds() {
		feed, event = "", " DEFINE TABLE IF NOT EXISTS ev2 SCHEMALESS;" // the same number of statements
	}
	for _, t := range []string{"la", "reg", "acct", "blob"} {
		fmt.Fprintf(&b, " DEFINE TABLE IF NOT EXISTS %s SCHEMALESS%s;", t, feed)
	}
	// the second group: an async event, a unique and a plain index, and a graph
	b.WriteString(" DEFINE TABLE IF NOT EXISTS ev SCHEMALESS; DEFINE TABLE IF NOT EXISTS mark SCHEMALESS;" + event +
		" DEFINE TABLE IF NOT EXISTS u SCHEMALESS; DEFINE INDEX IF NOT EXISTS u_email ON u FIELDS email UNIQUE; DEFINE INDEX IF NOT EXISTS u_grp ON u FIELDS grp;" +
		" DEFINE TABLE IF NOT EXISTS person SCHEMALESS; DEFINE TABLE IF NOT EXISTS follows TYPE RELATION;" +
		" DEFINE TABLE IF NOT EXISTS lq SCHEMALESS;")
	b.WriteString(" BEGIN;")
	for i := 0; i < nLA; i++ {
		fmt.Fprintf(&b, " UPSERT la:k%d SET v = [] RETURN NONE;", i)
	}
	for i := 0; i < nAcct; i++ {
		fmt.Fprintf(&b, " UPSERT acct:%d SET bal = %d RETURN NONE;", i, start)
	}
	for i := 0; i < nPerson; i++ {
		fmt.Fprintf(&b, " UPSERT person:%d SET n = %d RETURN NONE;", i, i)
	}
	b.WriteString(" COMMIT;")
	for try := 0; try < 30; try++ {
		res, err := query(b.String())
		if o, msg := judge(res, err, 4+4+9+1+nLA+nAcct+nPerson+1); o == ack {
			fmt.Println("sdbc: init done")
			return 0
		} else {
			fmt.Fprintln(os.Stderr, "sdbc: init:", o, msg)
		}
		time.Sleep(time.Second)
	}
	return 1
}

// ---- operations ----

func cmdID() int { return atoiDef(os.Getenv("HYDO_CMD_ID"), 0) }

func rng() *rand.Rand {
	s := uint64(atoiDef(os.Getenv("HYDO_SEED"), 0))
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

// opLA is one list-append transaction of 1 to 4 micro-ops on la:k0..k7, sent as one request
// (BEGIN ... COMMIT). Elements are unique: <cmd id>*10 + the micro-op's index.
func opLA() int {
	r := rng()
	id := cmdID()
	n := 1 + r.IntN(4)
	type mop struct {
		app  bool
		key  int
		elem int
	}
	ops := make([]mop, n)
	var b strings.Builder
	b.WriteString("BEGIN;")
	inv := make([]string, n)
	for i := range ops {
		ops[i] = mop{app: r.IntN(2) == 0, key: r.IntN(nLA), elem: id*10 + i}
		k := fmt.Sprintf("k%d", ops[i].key)
		if ops[i].app {
			fmt.Fprintf(&b, " UPSERT la:%s SET v = array::append(v ?? [], %d) RETURN NONE;", k, ops[i].elem)
			inv[i] = hydohist.Append(k, strconv.Itoa(ops[i].elem))
		} else {
			fmt.Fprintf(&b, " SELECT VALUE v FROM ONLY la:%s;", k)
			inv[i] = hydohist.Read(k)
		}
	}
	b.WriteString(" COMMIT;")
	h := hydohist.New("la")
	op := h.Invoke(id, "txn", inv...)
	res, err := query(b.String())
	o, msg := judge(res, err, n+2)
	switch o {
	case ack:
		done := make([]string, n)
		for i, m := range ops {
			k := fmt.Sprintf("k%d", m.key)
			if m.app {
				done[i] = inv[i]
				continue
			}
			var l []int64
			if e := json.Unmarshal(res[i+1].Result, &l); e != nil {
				fmt.Fprintf(os.Stderr, "sdbc: la read %s: %s\n", k, res[i+1].Result)
				op.Info()
				record(strconv.Itoa(id), unknown)
				return 1
			}
			done[i] = hydohist.ReadResult(k, ints(l)...)
		}
		op.Ok(done...)
	case failed:
		op.Fail()
	default:
		op.Info()
	}
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "sdbc: la:", o, msg)
		return 1
	}
	return 0
}

func ints(l []int64) []string {
	s := make([]string, len(l))
	for i, v := range l {
		s[i] = strconv.FormatInt(v, 10)
	}
	return s
}

// opReg is one register operation on reg:k0..k4: a read, a write or a compare-and-set, each one
// statement (its own transaction). Values are unique: the command's id.
func opReg() int {
	r := rng()
	id := cmdID()
	k := fmt.Sprintf("k%d", r.IntN(nReg))
	h := hydohist.New("reg")
	var o outcome
	var msg string
	switch r.IntN(3) {
	case 0:
		op := h.Invoke(id, "read", k)
		res, err := query(fmt.Sprintf("SELECT VALUE v FROM ONLY reg:%s;", k))
		o, msg = judge(res, err, 1)
		if o == ack {
			var v *int64
			if e := json.Unmarshal(res[0].Result, &v); e != nil {
				o, msg = unknown, "read: "+string(res[0].Result)
				op.Info()
			} else if v == nil {
				op.Ok(k, "nil")
			} else {
				op.Ok(k, strconv.FormatInt(*v, 10))
			}
		} else if o == failed {
			op.Fail()
		} else {
			op.Info()
		}
	case 1:
		v := strconv.Itoa(id)
		op := h.Invoke(id, "write", k, v)
		res, err := query(fmt.Sprintf("UPSERT reg:%s SET v = %s RETURN NONE;", k, v))
		o, msg = judge(res, err, 1)
		finish(op, o)
	default:
		old := strconv.Itoa(r.IntN(id + 1))
		// compare with a recent value more often than with an old one
		if r.IntN(2) == 0 && id > 3 {
			old = strconv.Itoa(id - 1 - r.IntN(3))
		}
		v := strconv.Itoa(id)
		op := h.Invoke(id, "cas", k, old, v)
		res, err := query(fmt.Sprintf("UPDATE reg:%s SET v = %s WHERE v = %s RETURN VALUE v;", k, v, old))
		o, msg = judge(res, err, 1)
		if o == ack {
			var l []json.RawMessage
			if e := json.Unmarshal(res[0].Result, &l); e != nil {
				o, msg = unknown, "cas: "+string(res[0].Result)
				op.Info()
			} else if len(l) == 0 {
				o = failed // it did not swap
				op.Fail()
			} else {
				op.Ok()
			}
		} else {
			finish(op, o)
		}
	}
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "sdbc: reg:", o, msg)
		return 1
	}
	return 0
}

func finish(op *hydohist.Op, o outcome) {
	switch o {
	case ack:
		op.Ok()
	case failed:
		op.Fail()
	default:
		op.Info()
	}
}

// opBank moves 1 to 30 from one account to another in one transaction, refused when the
// source would go below zero.
func opBank() int {
	r := rng()
	id := cmdID()
	from := r.IntN(nAcct)
	to := (from + 1 + r.IntN(nAcct-1)) % nAcct
	amt := 1 + r.IntN(30)
	q := fmt.Sprintf("BEGIN; LET $b = (SELECT VALUE bal FROM ONLY acct:%d); IF $b < %d { THROW \"insufficient\" }; "+
		"UPDATE acct:%d SET bal -= %d RETURN NONE; UPDATE acct:%d SET bal += %d RETURN NONE; COMMIT;",
		from, amt, from, amt, to, amt)
	res, err := query(q)
	o, msg := judge(res, err, 6)
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "sdbc: bank:", o, msg)
		return 1
	}
	return 0
}

// pad is the blob's content: n bytes that depend only on the id, so a check can recompute it.
func pad(id, n int) string {
	r := rand.New(rand.NewPCG(uint64(id), 7))
	const abc = "abcdefghijklmnopqrstuvwxyz0123456789"
	b := make([]byte, n)
	for i := range b {
		b[i] = abc[r.IntN(len(abc))]
	}
	return string(b)
}

// opBlob creates blob:<id> with a pad of 1 to 20 KiB, half of them above SurrealKV's 4 KiB
// value-log threshold.
func opBlob() int {
	r := rng()
	id := cmdID()
	n := 1 + r.IntN(4096)
	if r.IntN(2) == 0 {
		n = 4097 + r.IntN(16*1024)
	}
	q := fmt.Sprintf("CREATE blob:%d SET n = %d, pad = '%s' RETURN NONE;", id, n, pad(id, n))
	t0 := mono()
	res, err := query(q)
	t1 := mono()
	o, msg := judge(res, err, 1)
	ids := fmt.Sprintf("b%d", id)
	record(ids, o)
	if o == ack {
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("b%d:%d:%d:%d", id, n, t0, t1))
		return 0
	}
	fmt.Fprintln(os.Stderr, "sdbc: blob:", o, msg)
	return 1
}

// opEv creates ev:<id>; its async event marks it (mark:<id>, n counts the runs). $HYDO_ACKED gets
// v<id> for an acknowledged creation.
func opEv() int {
	id := cmdID()
	res, err := query(fmt.Sprintf("CREATE ev:%d SET k = %d RETURN NONE;", id, id))
	o, msg := judge(res, err, 1)
	record(fmt.Sprintf("v%d", id), o)
	if o == ack {
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("v%d", id))
		return 0
	}
	fmt.Fprintln(os.Stderr, "sdbc: ev:", o, msg)
	return 1
}

// opRel adds the edge follows:e<id> between two persons (two in three), or deletes the edge of an
// earlier command. $HYDO_ACKED gets e<id>:<from>:<to> for an acknowledged edge; the outcome of a
// deletion is d<k> in $HYDO_OUTCOMES.
func opRel() int {
	r := rng()
	id := cmdID()
	if r.IntN(3) > 0 || id < 4 {
		a := r.IntN(nPerson)
		b := (a + 1 + r.IntN(nPerson-1)) % nPerson
		res, err := query(fmt.Sprintf("RELATE person:%d->follows:e%d->person:%d RETURN NONE;", a, id, b))
		o, msg := judge(res, err, 1)
		record(fmt.Sprintf("e%d", id), o)
		if o == ack {
			appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("e%d:%d:%d", id, a, b))
			return 0
		}
		fmt.Fprintln(os.Stderr, "sdbc: relate:", o, msg)
		return 1
	}
	k := 1 + r.IntN(id-1)
	res, err := query(fmt.Sprintf("DELETE follows:e%d RETURN NONE;", k))
	o, msg := judge(res, err, 1)
	record(fmt.Sprintf("d%d", k), o)
	if o == ack {
		return 0
	}
	fmt.Fprintln(os.Stderr, "sdbc: unrelate:", o, msg)
	return 1
}

// opUser creates u:<id> with an e-mail address of the pool (a taken one is refused by the unique
// index), or moves an earlier user to another address and group.
func opUser() int {
	r := rng()
	id := cmdID()
	email, grp := r.IntN(nEmail), r.IntN(nGrp)
	var q string
	if r.IntN(5) < 3 || id < 4 {
		q = fmt.Sprintf("CREATE u:%d SET email = 'm%d', grp = %d RETURN NONE;", id, email, grp)
	} else {
		q = fmt.Sprintf("UPDATE u:%d SET email = 'm%d', grp = %d RETURN NONE;", 1+r.IntN(id-1), email, grp)
	}
	res, err := query(q)
	o, msg := judge(res, err, 1)
	record(fmt.Sprintf("u%d", id), o)
	if o == ack {
		return 0
	}
	fmt.Fprintln(os.Stderr, "sdbc: user:", o, msg)
	return 1
}

// opLive registers LIVE SELECT on table lq over the WebSocket RPC endpoint, then creates
// lq:l<id>_0 .. lq:l<id>_5 one after another over HTTP, and waits for their notifications. Each
// acknowledged creation must be notified, the creations in the order of their commits ("The
// notifications for the commits of one client arrive in the order of those commits"), and no
// creation that failed ("a live event will not take place if a transaction is rolled back").
// A broken connection or a server that does not answer is no verdict (exit 1); a broken promise
// on a connection that stayed open exits 100.
func opLive() int {
	id := cmdID()
	ws, err := wsDial()
	if err != nil {
		fmt.Fprintln(os.Stderr, "sdbc: live: connect:", err)
		return 1
	}
	defer ws.Close()
	lq, err := ws.call("query", []any{"LIVE SELECT * FROM lq"})
	if err != nil {
		fmt.Fprintln(os.Stderr, "sdbc: live: register:", err)
		return 1
	}
	var reg []stmt
	var uuid string
	if json.Unmarshal(lq, &reg) != nil || len(reg) != 1 || reg[0].Status != "OK" || json.Unmarshal(reg[0].Result, &uuid) != nil {
		fmt.Fprintln(os.Stderr, "sdbc: live: register answered", trunc(string(lq), 200))
		return 1
	}
	const n = 6
	outcome := make([]outcome, n)
	for i := 0; i < n; i++ {
		res, err := query(fmt.Sprintf("CREATE lq:l%d_%d SET n = %d RETURN NONE;", id, i, i))
		outcome[i], _ = judge(res, err, 1)
		record(fmt.Sprintf("l%d_%d", id, i), outcome[i])
	}
	// wait for the notifications of this command's records, up to 30 s after the last write
	mine := fmt.Sprintf("lq:l%d_", id)
	var seen []int
	deadline := time.Now().Add(30 * time.Second)
	want := 0
	for _, o := range outcome {
		if o == ack {
			want++
		}
	}
	for len(seen) < n && time.Now().Before(deadline) {
		msg, err := ws.next(time.Until(deadline))
		if err != nil {
			if errors.Is(err, errTimeout) {
				break
			}
			fmt.Fprintln(os.Stderr, "sdbc: live: connection:", err)
			return 1
		}
		var note struct {
			Result struct {
				Action string `json:"action"`
				ID     string `json:"id"`
				Result struct {
					ID string `json:"id"`
				} `json:"result"`
			} `json:"result"`
		}
		if json.Unmarshal(msg, &note) != nil || note.Result.Action != "CREATE" || note.Result.ID != uuid {
			continue
		}
		if r := note.Result.Result.ID; strings.HasPrefix(r, mine) {
			seen = append(seen, atoiDef(strings.TrimPrefix(r, mine), -1))
		}
		got := 0
		for _, k := range seen {
			if k >= 0 && k < n && outcome[k] == ack {
				got++
			}
		}
		if got == want && want == countKnown(outcome) {
			break
		}
	}
	ws.call("kill", []any{uuid})
	bad := []string{}
	have := map[int]bool{}
	for j, k := range seen {
		if k < 0 || k >= n {
			bad = append(bad, fmt.Sprintf("a notification of lq:l%d_%d, which this command never created", id, k))
			continue
		}
		if have[k] {
			bad = append(bad, fmt.Sprintf("lq:l%d_%d was notified twice", id, k))
		}
		have[k] = true
		if outcome[k] == failed {
			bad = append(bad, fmt.Sprintf("lq:l%d_%d was notified, and its creation failed", id, k))
		}
		if j > 0 && seen[j-1] > k {
			bad = append(bad, fmt.Sprintf("lq:l%d_%d was notified after lq:l%d_%d, which committed later", id, k, id, seen[j-1]))
		}
	}
	for k, o := range outcome {
		if o == ack && !have[k] {
			bad = append(bad, fmt.Sprintf("lq:l%d_%d was acknowledged and never notified within 30 s", id, k))
		}
	}
	if len(bad) > 0 {
		for _, b := range bad {
			fmt.Println(b)
		}
		fmt.Printf("live query %s: %d of %d acknowledged creations notified, %d problems\n", uuid, len(have), want, len(bad))
		return 100
	}
	return 0
}

// countKnown is the number of acknowledged outcomes, the ones a notification must exist for.
func countKnown(o []outcome) int {
	n := 0
	for _, x := range o {
		if x == ack {
			n++
		}
	}
	return n
}

// ---- read-backs ----

func readback(hist string) int {
	h := hydohist.New(hist)
	proc := 1000000
	var keys []string
	var q strings.Builder
	switch hist {
	case "la":
		for i := 0; i < nLA; i++ {
			keys = append(keys, fmt.Sprintf("k%d", i))
			fmt.Fprintf(&q, "SELECT VALUE v FROM ONLY la:k%d;", i)
		}
	case "reg":
		for i := 0; i < nReg; i++ {
			keys = append(keys, fmt.Sprintf("k%d", i))
			fmt.Fprintf(&q, "SELECT VALUE v FROM ONLY reg:k%d;", i)
		}
	default:
		fmt.Fprintln(os.Stderr, "sdbc readback: la or reg")
		return 2
	}
	var res []stmt
	var err error
	for try := 0; try < 60; try++ {
		res, err = query(q.String())
		if o, _ := judge(res, err, len(keys)); o == ack {
			break
		}
		res = nil
		time.Sleep(time.Second)
	}
	if res == nil {
		fmt.Println("the server did not answer the read-back:", err)
		return 2
	}
	for i, k := range keys {
		switch hist {
		case "la":
			var l []int64
			if json.Unmarshal(res[i].Result, &l) != nil {
				fmt.Printf("la:%s holds %s, not a list\n", k, res[i].Result)
				return 1
			}
			op := h.Invoke(proc+i, "txn", hydohist.Read(k))
			op.Ok(hydohist.ReadResult(k, ints(l)...))
		case "reg":
			var v *int64
			if json.Unmarshal(res[i].Result, &v) != nil {
				fmt.Printf("reg:%s holds %s, not a number\n", k, res[i].Result)
				return 1
			}
			op := h.Invoke(proc+i, "read", k)
			if v == nil {
				op.Ok(k, "nil")
			} else {
				op.Ok(k, strconv.FormatInt(*v, 10))
			}
		}
	}
	return 0
}

// ---- checks ----

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

// outcomes is the last outcome of each id in $HYDO_OUTCOMES.
func outcomes() map[string]string {
	m := map[string]string{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		if f := strings.Fields(l); len(f) == 2 {
			m[f[0]] = f[1]
		}
	}
	return m
}

// one runs a read-only query with retries and returns its single statement's result.
func one(q string, n int) ([]stmt, error) {
	var err error
	for try := 0; try < 20; try++ {
		var res []stmt
		res, err = query(q)
		if o, msg := judge(res, err, n); o == ack {
			return res, nil
		} else if err == nil {
			err = errors.New(msg)
		}
		time.Sleep(time.Second)
	}
	return nil, err
}

// checkBank: the accounts hold 800 in all and none is below zero.
func checkBank() int {
	res, err := one("SELECT id, bal FROM acct;", 1)
	if err != nil {
		fmt.Println("cannot read the accounts:", err)
		return 2
	}
	var accts []struct {
		ID  string `json:"id"`
		Bal int64  `json:"bal"`
	}
	if e := json.Unmarshal(res[0].Result, &accts); e != nil {
		fmt.Println("accounts:", trunc(string(res[0].Result), 300))
		return 1
	}
	var sum int64
	neg := 0
	for _, a := range accts {
		sum += a.Bal
		if a.Bal < 0 {
			neg++
			fmt.Printf("%s holds %d\n", a.ID, a.Bal)
		}
	}
	if len(accts) != nAcct || sum != nAcct*start || neg > 0 {
		fmt.Printf("%d accounts hold %d in all (want %d accounts and %d), %d below zero\n", len(accts), sum, nAcct, nAcct*start, neg)
		return 1
	}
	fmt.Printf("%d accounts hold %d\n", len(accts), sum)
	return 0
}

type blobRow struct {
	ID  string `json:"id"`
	N   int    `json:"n"`
	Pad string `json:"pad"`
}

func blobID(s string) int {
	return atoiDef(strings.TrimPrefix(s, "blob:"), -1)
}

// checkBlobs: every acknowledged blob is there with its whole pad; no blob whose creation
// failed is there.
func checkBlobs() int {
	res, err := one("SELECT id, n, pad FROM blob;", 1)
	if err != nil {
		fmt.Println("cannot read the blobs:", err)
		return 2
	}
	var rows []blobRow
	if e := json.Unmarshal(res[0].Result, &rows); e != nil {
		fmt.Println("blobs:", trunc(string(res[0].Result), 300))
		return 1
	}
	have := map[int]blobRow{}
	for _, r := range rows {
		have[blobID(r.ID)] = r
	}
	lost, wrong, phantom := 0, 0, 0
	acked := 0
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if !strings.HasPrefix(l, "b") {
			continue
		}
		f := strings.Split(l[1:], ":")
		id, n := atoiDef(f[0], -1), atoiDef(f[1], -1)
		acked++
		r, ok := have[id]
		switch {
		case !ok:
			lost++
			fmt.Printf("lost: blob:%d (%d bytes) was acknowledged\n", id, n)
		case r.N != n || r.Pad != pad(id, n):
			wrong++
			fmt.Printf("wrong: blob:%d holds n=%d and %d bytes, acknowledged with %d\n", id, r.N, len(r.Pad), n)
		}
	}
	for id, o := range outcomes() {
		if !strings.HasPrefix(id, "b") || o != "failed" {
			continue
		}
		if _, ok := have[atoiDef(id[1:], -1)]; ok {
			phantom++
			fmt.Printf("applied: blob:%s exists, and its creation was answered with an error\n", id[1:])
		}
	}
	if lost+wrong+phantom > 0 {
		fmt.Printf("of %d acknowledged blobs %d lost and %d wrong; %d failed creations applied\n", acked, lost, wrong, phantom)
		return 1
	}
	fmt.Printf("%d acknowledged blobs, all there (%d in the table)\n", acked, len(rows))
	return 0
}

// checkFeed: the blob table's changefeed holds one creation for every blob in the table, none
// for a blob that is not there, and its versionstamps follow the order in which the client saw
// the creations acknowledged: a creation acknowledged before another was sent has the lower
// versionstamp ("Versionstamps monotonically increase").
func checkFeed() int {
	if !feeds() {
		fmt.Println("the changefeeds are off (SDB_FEEDS=no)")
		return 0
	}
	res, err := one("SHOW CHANGES FOR TABLE blob SINCE 0 LIMIT 1000000;", 1)
	if err != nil {
		fmt.Println("cannot read the changefeed:", err)
		return 2
	}
	var sets []struct {
		Versionstamp uint64 `json:"versionstamp"`
		Changes      []map[string]json.RawMessage
	}
	if e := json.Unmarshal(res[0].Result, &sets); e != nil {
		fmt.Println("changefeed:", trunc(string(res[0].Result), 300))
		return 1
	}
	stamp := map[int]uint64{}
	for _, s := range sets {
		for _, c := range s.Changes {
			for kind, v := range c {
				if kind != "update" && kind != "create" {
					continue
				}
				var row struct {
					ID string `json:"id"`
				}
				if json.Unmarshal(v, &row) == nil && strings.HasPrefix(row.ID, "blob:") {
					id := blobID(row.ID)
					if _, dup := stamp[id]; !dup {
						stamp[id] = s.Versionstamp
					}
				}
			}
		}
	}
	tab, err := one("SELECT VALUE id FROM blob;", 1)
	if err != nil {
		fmt.Println("cannot read the blobs:", err)
		return 2
	}
	var ids []string
	json.Unmarshal(tab[0].Result, &ids)
	have := map[int]bool{}
	missing, extra := 0, 0
	for _, s := range ids {
		id := blobID(s)
		have[id] = true
		if _, ok := stamp[id]; !ok {
			missing++
			fmt.Printf("blob:%d is in the table and not in the changefeed\n", id)
		}
	}
	for id := range stamp {
		if !have[id] {
			extra++
			fmt.Printf("blob:%d is in the changefeed and not in the table\n", id)
		}
	}
	// the acknowledged creations in the order the client saw them
	type ackd struct {
		id     int
		t0, t1 int64
	}
	var acks []ackd
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if !strings.HasPrefix(l, "b") {
			continue
		}
		f := strings.Split(l[1:], ":")
		if len(f) == 4 {
			t0, _ := strconv.ParseInt(f[2], 10, 64)
			t1, _ := strconv.ParseInt(f[3], 10, 64)
			acks = append(acks, ackd{atoiDef(f[0], -1), t0, t1})
		}
	}
	sort.Slice(acks, func(i, j int) bool { return acks[i].t1 < acks[j].t1 })
	back := 0
	// a: acknowledged before b was sent (a.t1 < b.t0), yet b's stamp is not above a's
	for j, b := range acks {
		sb, okb := stamp[b.id]
		if !okb {
			continue
		}
		for _, a := range acks[:j] {
			if a.t1 >= b.t0 {
				continue
			}
			if sa, oka := stamp[a.id]; oka && sb <= sa {
				back++
				if back <= 5 {
					fmt.Printf("blob:%d (versionstamp %d) was sent after blob:%d (versionstamp %d) was acknowledged\n", b.id, sb, a.id, sa)
				}
				break
			}
		}
	}
	if missing+extra+back > 0 {
		fmt.Printf("changefeed of %d blobs: %d missing, %d extra, %d creations stamped below an earlier acknowledged one\n", len(ids), missing, extra, back)
		return 1
	}
	fmt.Printf("changefeed of %d blobs complete and in order\n", len(ids))
	return 0
}

// strs reads a statement's result as a list of strings (record ids).
func strs(s stmt) ([]string, error) {
	var l []string
	if err := json.Unmarshal(s.Result, &l); err != nil {
		return nil, fmt.Errorf("not a list of ids: %s", trunc(string(s.Result), 200))
	}
	sort.Strings(l)
	return l, nil
}

// checkMarks: every acknowledged ev has been marked by its async event ("An event runs at least
// once"), and no ev whose creation failed was marked ("If the transaction fails, the event is
// never queued"). Exit 1 until then: an eventually_ check.
func checkMarks() int {
	if !feeds() {
		fmt.Println("the ASYNC event is off (SDB_FEEDS=no)")
		return 0
	}
	res, err := one("SELECT VALUE record::id(id) FROM ev; SELECT VALUE record::id(id) FROM mark;", 2)
	if err != nil {
		fmt.Println("cannot read the events:", err)
		return 2
	}
	var evs, marks []int
	json.Unmarshal(res[0].Result, &evs)
	json.Unmarshal(res[1].Result, &marks)
	hasEv, hasMark := map[int]bool{}, map[int]bool{}
	for _, v := range evs {
		hasEv[v] = true
	}
	for _, v := range marks {
		hasMark[v] = true
	}
	unmarked, lost, stray := 0, 0, 0
	acked := 0
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if !strings.HasPrefix(l, "v") {
			continue
		}
		id := atoiDef(l[1:], -1)
		acked++
		if !hasEv[id] {
			lost++
			fmt.Printf("lost: ev:%d was acknowledged\n", id)
		} else if !hasMark[id] {
			unmarked++
			fmt.Printf("ev:%d has no mark yet\n", id)
		}
	}
	for id := range hasMark {
		if !hasEv[id] {
			stray++
			fmt.Printf("mark:%d has no ev:%d\n", id, id)
		}
	}
	if lost+unmarked+stray > 0 {
		fmt.Printf("of %d acknowledged events %d lost and %d not marked; %d marks without their event\n", acked, lost, unmarked, stray)
		return 1
	}
	fmt.Printf("%d acknowledged events, all marked (%d events, %d marks)\n", acked, len(evs), len(marks))
	return 0
}

// checkGraph: each person's edges read through the graph (->follows->, <-follows<-) are the edges
// of the follows table, and every acknowledged edge that nothing tried to delete is there.
func checkGraph() int {
	var q strings.Builder
	for i := 0; i < nPerson; i++ {
		fmt.Fprintf(&q, "SELECT VALUE ->follows->person FROM ONLY person:%d; SELECT VALUE out FROM follows WHERE in = person:%d;", i, i)
		fmt.Fprintf(&q, "SELECT VALUE <-follows<-person FROM ONLY person:%d; SELECT VALUE in FROM follows WHERE out = person:%d;", i, i)
	}
	q.WriteString("SELECT VALUE record::id(id) FROM follows;")
	res, err := one(q.String(), 4*nPerson+1)
	if err != nil {
		fmt.Println("cannot read the graph:", err)
		return 2
	}
	bad := 0
	for i := 0; i < nPerson; i++ {
		for d, dir := range []string{"->follows->", "<-follows<-"} {
			g, e1 := strs(res[4*i+2*d])
			t, e2 := strs(res[4*i+2*d+1])
			if e1 != nil || e2 != nil {
				fmt.Println("person", i, dir, e1, e2)
				return 1
			}
			if strings.Join(g, ",") != strings.Join(t, ",") {
				bad++
				fmt.Printf("person:%d %s gives [%s], the follows table [%s]\n", i, dir, strings.Join(g, ","), strings.Join(t, ","))
			}
		}
	}
	var ids []string
	json.Unmarshal(res[4*nPerson].Result, &ids)
	have := map[string]bool{}
	for _, s := range ids {
		have[s] = true
	}
	tried := map[string]bool{}
	for id, o := range outcomes() {
		if strings.HasPrefix(id, "d") && o != "failed" {
			tried["e"+id[1:]] = true
		}
	}
	lost, acked := 0, 0
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		if !strings.HasPrefix(l, "e") {
			continue
		}
		e := strings.SplitN(l, ":", 2)[0]
		acked++
		if !tried[e] && !have[e] {
			lost++
			fmt.Printf("lost: follows:%s (%s) was acknowledged and never deleted\n", e, l)
		}
	}
	if bad+lost > 0 {
		fmt.Printf("%d graph reads differ from the follows table; %d of %d acknowledged edges lost\n", bad, lost, acked)
		return 1
	}
	fmt.Printf("graph of %d edges agrees with the follows table (%d acknowledged)\n", len(ids), acked)
	return 0
}

// checkIndex: no two users share an e-mail address (the unique index), and a lookup through each
// index finds the users that a scan without the index finds.
func checkIndex() int {
	var q strings.Builder
	for j := 0; j < nEmail; j++ {
		fmt.Fprintf(&q, "SELECT VALUE id FROM u WHERE email = 'm%d'; SELECT VALUE id FROM u WITH NOINDEX WHERE email = 'm%d';", j, j)
	}
	for g := 0; g < nGrp; g++ {
		fmt.Fprintf(&q, "SELECT VALUE id FROM u WHERE grp = %d; SELECT VALUE id FROM u WITH NOINDEX WHERE grp = %d;", g, g)
	}
	n := 2 * (nEmail + nGrp)
	res, err := one(q.String(), n)
	if err != nil {
		fmt.Println("cannot read the users:", err)
		return 2
	}
	dup, diff := 0, 0
	for k := 0; k < nEmail+nGrp; k++ {
		ix, e1 := strs(res[2*k])
		sc, e2 := strs(res[2*k+1])
		if e1 != nil || e2 != nil {
			fmt.Println("users:", e1, e2)
			return 1
		}
		what := fmt.Sprintf("email = 'm%d'", k)
		if k >= nEmail {
			what = fmt.Sprintf("grp = %d", k-nEmail)
		}
		if strings.Join(ix, ",") != strings.Join(sc, ",") {
			diff++
			fmt.Printf("%s: the index finds [%s], a scan [%s]\n", what, strings.Join(ix, ","), strings.Join(sc, ","))
		}
		if k < nEmail && len(sc) > 1 {
			dup++
			fmt.Printf("%s is held by %d users: [%s]\n", what, len(sc), strings.Join(sc, ","))
		}
	}
	if dup+diff > 0 {
		fmt.Printf("%d addresses held twice; %d index lookups differ from a scan\n", dup, diff)
		return 1
	}
	fmt.Println("the indexes agree with the table, every address held once")
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: sdbc ready|init|op <kind>|readback <hist>|check <rule>|q <surql>")
		os.Exit(2)
	}
	arg := ""
	if len(os.Args) > 2 {
		arg = os.Args[2]
	}
	switch os.Args[1] {
	case "ready":
		res, err := query("RETURN 1;")
		if o, _ := judge(res, err, 1); o != ack {
			os.Exit(1)
		}
	case "init":
		os.Exit(cmdInit())
	case "op":
		switch arg {
		case "la":
			os.Exit(opLA())
		case "reg":
			os.Exit(opReg())
		case "bank":
			os.Exit(opBank())
		case "blob":
			os.Exit(opBlob())
		case "ev":
			os.Exit(opEv())
		case "rel":
			os.Exit(opRel())
		case "user":
			os.Exit(opUser())
		case "live":
			os.Exit(opLive())
		}
		fmt.Fprintln(os.Stderr, "sdbc op: la, reg, bank, blob, ev, rel, user or live")
		os.Exit(2)
	case "readback":
		os.Exit(readback(arg))
	case "check":
		switch arg {
		case "bank":
			os.Exit(checkBank())
		case "blobs":
			os.Exit(checkBlobs())
		case "feed":
			os.Exit(checkFeed())
		case "marks":
			os.Exit(checkMarks())
		case "graph":
			os.Exit(checkGraph())
		case "index":
			os.Exit(checkIndex())
		}
		fmt.Fprintln(os.Stderr, "sdbc check: bank, blobs, feed, marks, graph or index")
		os.Exit(2)
	case "q":
		res, err := query(arg)
		if err != nil {
			fmt.Println(err)
			os.Exit(2)
		}
		b, _ := json.Marshal(res)
		fmt.Println(string(b))
	default:
		os.Exit(2)
	}
}
