// crc is the test client of the CockroachDB example: the cluster's setup, the workload's writes,
// transfers and transactions, the reads at past timestamps, the read-backs and the final checks.
// It speaks CockroachDB's SQL with pgx and sends each statement once, without retries.
//
//	crc ready <addr>      the nodes' RPC port at <addr> accepts a connection (a readiness check)
//	crc init              wait until every node serves SQL, create the tables, wait until every
//	                      range has three replicas (first_init); with
//	                      CRC_PERSIST set, also server.clock.persist_upper_bound_interval
//	crc primary           print the node that holds the lease of the table la (the group's primary:)
//	crc op <kind>         one write of the history w; the composer's HYDO_* variables drive it
//	crc txn               one transaction of the history la (list appends and reads)
//	crc transfer          one transfer between two accounts of the table acct
//	crc read              one past timestamp read on every node: equal answers, the bank's total,
//	                      lists that are prefixes of the current ones (anytime_read)
//	crc follower          a read at follower_read_timestamp() through a node (anytime_follower)
//	crc readback          the final: of the messages check: each write's effect, read back
//	crc lafinal           the final read of every list of the history la
//	crc check <rule>      one check: bank, replicas, serves, clockstops
//	crc sql <node> <stmt> one statement, the rows printed (for a look by hand)
//
// The nodes are roach1, roach2 and roach3 (10.89.0.11 to .13), with SQL on port 26257. A
// statement goes to a node that the seed picks; CockroachDB sends it on to the leaseholder.
//
// The history w (a messages check): writes of the table kv(id STRING PRIMARY KEY, v STRING), one
// id each: <letter>.<key>.<seq>.<value>. A key is a register: its value is the tag w<cmd> of the
// write that last set it, or "-" when the row does not exist.
//
//	insert  i  INSERT of a new key k<cmd>, one statement (an implicit transaction)
//	upsert  r  UPSERT of a known key, deleted ones too
//	update  u  UPDATE of an existing key
//	delete  d  DELETE of an existing key
//	tx      t  2 or 3 of the writes above on other keys in one explicit transaction: one send of
//	           all of their ids, each with t before its own letter (ti, tr, tu, td)
//
// One command at a time works on a key (a lock file under /tmp/crc/lock). A key's last known
// value and seq are in /tmp/crc/keys/<key>, for the keys whose every write got a certain answer;
// a key whose write got no certain answer is never written again.
//
// Outcomes ($HYDO_ACKED, $HYDO_OUTCOMES "<id> ack|failed|unknown"): a commit without an error
// acknowledges. An error before the commit was sent, a connection that never opened, a retry
// error (40001), a unique violation and the other errors of a statement fail: CockroachDB applied
// nothing. "result is ambiguous" (40003), every other error of the commit, a timeout and a lost
// connection give an unknown outcome.
//
// The history la (a list-append check at serializability): the table la(k INT PRIMARY KEY,
// v STRING), a list per key x0 to x7 as comma-separated elements. A transaction of 1 to 3 appends
// and reads runs at CockroachDB's default isolation, SERIALIZABLE.
//
// The bank (table acct): five accounts of 100 each; a transfer moves 1 to 5 from one to another
// in one transaction, so every read of all five sums to 500.
package main

import (
	"context"
	"errors"
	"fmt"
	"math/rand/v2"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

const (
	sqlPort  = "26257"
	rpcPort  = "26357"
	stateDir = "/tmp/crc"
	laKeys   = 8
	accounts = 5
	total    = 500
)

var nodes = []string{"roach1", "roach2", "roach3"}

var addrs = map[string]string{"roach1": "10.89.0.11", "roach2": "10.89.0.12", "roach3": "10.89.0.13"}

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
// (A=crdb.write_acked+<ms>:cut, hydo explore --reactive-at crdb.write_acked@20).
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

// ---- SQL ----

func connect(node string, timeout time.Duration) (*pgx.Conn, error) {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	url := fmt.Sprintf("postgres://root@%s:%s/defaultdb?sslmode=disable&application_name=crc", addrs[node], sqlPort)
	return pgx.Connect(ctx, url)
}

func pgCode(err error) string {
	var pe *pgconn.PgError
	if errors.As(err, &pe) {
		return pe.Code
	}
	return ""
}

// errText is an error in one line.
func errText(err error) string {
	if err == nil {
		return ""
	}
	s := strings.ReplaceAll(err.Error(), "\n", " ")
	if len(s) > 300 {
		s = s[:300]
	}
	return s
}

// commitOutcome classifies an error of the statement that commits: a retry error (40001), a
// unique violation and the other errors of a statement's own classes fail; "result is
// ambiguous" (40003) and every error without a SQL state are unknown.
func commitOutcome(err error) string {
	if err == nil {
		return "ack"
	}
	c := pgCode(err)
	switch {
	case c == "":
		return "unknown"
	case c == "40003":
		return "unknown"
	case c == "40001", strings.HasPrefix(c, "23"), strings.HasPrefix(c, "22"), strings.HasPrefix(c, "42"):
		return "failed"
	}
	return "unknown"
}

// ---- the history w: writes of kv ----

type op struct {
	kind string // i r u d, or t + one of them
	key  string
	seq  int
	val  string
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
	os.WriteFile(filepath.Join(stateDir, "keys", key), []byte(fmt.Sprintf("%d %s\n", s.seq, s.val)), 0o644)
}

func retire(key string) { os.Remove(filepath.Join(stateDir, "keys", key)) }

// pickKey takes a known key that no other command works on: an existing one (value not "-")
// when exists is set.
func pickKey(r *rand.Rand, exists bool, not map[string]bool) (string, keyState) {
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var names []string
	for _, e := range ents {
		names = append(names, e.Name())
	}
	sort.Strings(names)
	r.Shuffle(len(names), func(i, j int) { names[i], names[j] = names[j], names[i] })
	for _, k := range names {
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
		event("crdb.write_acked")
	}
	if oc != "ack" {
		fmt.Fprintf(os.Stderr, "crc: %s: %s: %s\n", strings.Join(ids, " "), oc, why)
	}
}

// stmt is the SQL of one write.
func stmt(o op) (string, []any) {
	switch strings.TrimPrefix(o.kind, "t") {
	case "i":
		return "INSERT INTO kv(id, v) VALUES ($1, $2)", []any{o.key, o.val}
	case "r":
		return "UPSERT INTO kv(id, v) VALUES ($1, $2)", []any{o.key, o.val}
	case "u":
		return "UPDATE kv SET v = $1 WHERE id = $2", []any{o.val, o.key}
	}
	return "DELETE FROM kv WHERE id = $1", []any{o.key}
}

var kindLetter = map[string]string{"insert": "i", "upsert": "r", "update": "u", "delete": "d"}

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
		case "insert":
			key := fmt.Sprintf("k%d", c)
			if exclude[key] {
				key = fmt.Sprintf("k%dx%d", c, len(exclude))
			}
			if !lock(key) {
				return op{}, false
			}
			locked = append(locked, key)
			return op{"i", key, 1, tag}, true
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
	default:
		o, ok := one(kind, nil)
		if !ok {
			return 0
		}
		ops = append(ops, o)
	}
	begin(ops)
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		finish(ops, "failed", "connect: "+errText(err)+" (through "+node+")", before)
		return 0
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	oc, why := "ack", ""
	if len(ops) == 1 {
		q, a := stmt(ops[0])
		_, err = conn.Exec(ctx, q, a...)
		oc, why = commitOutcome(err), errText(err)
	} else {
		oc, why = explicitTx(ctx, conn, func(tx pgx.Tx) error {
			for _, o := range ops {
				q, a := stmt(o)
				if _, err := tx.Exec(ctx, q, a...); err != nil {
					return err
				}
			}
			return nil
		})
	}
	finish(ops, oc, why+" (through "+node+")", before)
	return 0
}

// explicitTx runs body in BEGIN ... COMMIT: an error before the commit fails (nothing committed),
// an error of the commit is classified by commitOutcome.
func explicitTx(ctx context.Context, conn *pgx.Conn, body func(pgx.Tx) error) (string, string) {
	tx, err := conn.Begin(ctx)
	if err != nil {
		return "failed", "begin: " + errText(err)
	}
	if err := body(tx); err != nil {
		tx.Rollback(context.Background())
		return "failed", errText(err)
	}
	err = tx.Commit(ctx)
	return commitOutcome(err), errText(err)
}

// ---- the history la: list appends and reads in transactions ----

func parseList(s string) []int64 {
	var out []int64
	for _, e := range strings.Split(s, ",") {
		if e == "" {
			continue
		}
		if v, err := strconv.ParseInt(e, 10, 64); err == nil {
			out = append(out, v)
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
	var inv []string
	for _, m := range ms {
		if m.app {
			inv = append(inv, fmt.Sprintf("a:x%d:%d", m.k, m.e))
		} else {
			inv = append(inv, fmt.Sprintf("r:x%d", m.k))
		}
	}
	hrec("la", c, "invoke", append([]string{"txn"}, inv...)...)
	oc, why := "failed", ""
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		why = "connect: " + errText(err)
	} else {
		defer conn.Close(context.Background())
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		defer cancel()
		oc, why = explicitTx(ctx, conn, func(tx pgx.Tx) error {
			for i := range ms {
				m := &ms[i]
				if m.app {
					if _, err := tx.Exec(ctx, "INSERT INTO la(k, v) VALUES ($1, $2) ON CONFLICT (k) DO UPDATE SET v = la.v || ',' || excluded.v", m.k, strconv.FormatInt(m.e, 10)); err != nil {
						return err
					}
					continue
				}
				var v *string
				err := tx.QueryRow(ctx, "SELECT v FROM la WHERE k = $1", m.k).Scan(&v)
				if err != nil && !errors.Is(err, pgx.ErrNoRows) {
					return err
				}
				if v != nil {
					m.read = parseList(*v)
				}
			}
			return nil
		})
	}
	switch oc {
	case "ack":
		var out []string
		for _, m := range ms {
			if m.app {
				out = append(out, fmt.Sprintf("a:x%d:%d", m.k, m.e))
				appendLine(filepath.Join(stateDir, "la-acked"), fmt.Sprintf("%d %d", m.k, m.e))
			} else {
				out = append(out, fmt.Sprintf("r:x%d:%s", m.k, fmtList(m.read)))
			}
		}
		hrec("la", c, "ok", append([]string{"txn"}, out...)...)
		event("crdb.txn_acked")
	case "failed":
		hrec("la", c, "fail", append([]string{"txn"}, inv...)...)
		fmt.Fprintf(os.Stderr, "crc: txn failed through %s: %s\n", node, why)
	default:
		hrec("la", c, "info", append([]string{"txn"}, inv...)...)
		fmt.Fprintf(os.Stderr, "crc: txn unknown through %s: %s\n", node, why)
	}
	return 0
}

// ---- the bank ----

func transfer() int {
	r := rng()
	node := nodes[r.IntN(len(nodes))]
	from := r.IntN(accounts)
	to := (from + 1 + r.IntN(accounts-1)) % accounts
	amt := 1 + r.IntN(5)
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		return 0
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	oc, why := explicitTx(ctx, conn, func(tx pgx.Tx) error {
		var a, b int
		if err := tx.QueryRow(ctx, "SELECT bal FROM acct WHERE id = $1", from).Scan(&a); err != nil {
			return err
		}
		if err := tx.QueryRow(ctx, "SELECT bal FROM acct WHERE id = $1", to).Scan(&b); err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, "UPDATE acct SET bal = $1 WHERE id = $2", a-amt, from); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, "UPDATE acct SET bal = $1 WHERE id = $2", b+amt, to)
		return err
	})
	if oc != "ack" {
		fmt.Fprintf(os.Stderr, "crc: transfer %d->%d of %d through %s: %s: %s\n", from, to, amt, node, oc, why)
	}
	return 0
}

// bankAt reads every account in one transaction, at a past timestamp when asOf is set.
func bankAt(ctx context.Context, conn *pgx.Conn, asOf string) (map[int]int, error) {
	q := "SELECT id, bal FROM acct"
	if asOf != "" {
		q += " AS OF SYSTEM TIME " + asOf
	}
	rows, err := conn.Query(ctx, q+" ORDER BY id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[int]int{}
	for rows.Next() {
		var id, bal int
		if err := rows.Scan(&id, &bal); err != nil {
			return nil, err
		}
		out[id] = bal
	}
	return out, rows.Err()
}

func sum(m map[int]int) int {
	s := 0
	for _, v := range m {
		s += v
	}
	return s
}

func fmtBank(m map[int]int) string {
	var s []string
	for i := 0; i < accounts; i++ {
		s = append(s, strconv.Itoa(m[i]))
	}
	return "[" + strings.Join(s, " ") + "]"
}

// listsAt reads every list of la, at a past timestamp when asOf is set.
func listsAt(ctx context.Context, conn *pgx.Conn, asOf string) (map[int][]int64, error) {
	q := "SELECT k, v FROM la"
	if asOf != "" {
		q += " AS OF SYSTEM TIME " + asOf
	}
	rows, err := conn.Query(ctx, q+" ORDER BY k")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[int][]int64{}
	for rows.Next() {
		var k int
		var v string
		if err := rows.Scan(&k, &v); err != nil {
			return nil, err
		}
		out[k] = parseList(v)
	}
	return out, rows.Err()
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

// snapshot is what one node read at the past timestamp.
type snapshot struct {
	bank  map[int]int
	lists map[int][]int64
}

func (s snapshot) String() string {
	var l []string
	for k := 0; k < laKeys; k++ {
		l = append(l, fmt.Sprintf("x%d=%s", k, fmtList(s.lists[k])))
	}
	return "bank " + fmtBank(s.bank) + " " + strings.Join(l, " ")
}

// readAt reads the bank and the lists at one past timestamp through node, in one read-only
// transaction.
func readAt(node, ts string) (snapshot, error) {
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		return snapshot{}, err
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	tx, err := conn.BeginTx(ctx, pgx.TxOptions{AccessMode: pgx.ReadOnly})
	if err != nil {
		return snapshot{}, err
	}
	defer tx.Rollback(context.Background())
	if _, err := tx.Exec(ctx, "SET TRANSACTION AS OF SYSTEM TIME "+ts); err != nil {
		return snapshot{}, err
	}
	var s snapshot
	s.bank = map[int]int{}
	rows, err := tx.Query(ctx, "SELECT id, bal FROM acct ORDER BY id")
	if err != nil {
		return snapshot{}, err
	}
	for rows.Next() {
		var id, bal int
		if err := rows.Scan(&id, &bal); err != nil {
			rows.Close()
			return snapshot{}, err
		}
		s.bank[id] = bal
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return snapshot{}, err
	}
	s.lists = map[int][]int64{}
	rows, err = tx.Query(ctx, "SELECT k, v FROM la ORDER BY k")
	if err != nil {
		return snapshot{}, err
	}
	for rows.Next() {
		var k int
		var v string
		if err := rows.Scan(&k, &v); err != nil {
			rows.Close()
			return snapshot{}, err
		}
		s.lists[k] = parseList(v)
	}
	rows.Close()
	if err := rows.Err(); err != nil {
		return snapshot{}, err
	}
	return s, tx.Commit(ctx)
}

// readNodes takes the cluster's HLC timestamp from one node, goes back 4 s (past the closed
// timestamp target of 3 s, so that every replica may serve it), reads the bank and every list
// at that timestamp through each node, and reads the lists again at the current time. Every
// node must give the same answer, the bank must sum to 500, and each list must be a prefix of
// its current list.
// (anytime_read; exit 100 is a violation)
func readNodes() int {
	r := rng()
	first := nodes[r.IntN(len(nodes))]
	conn, err := connect(first, 5*time.Second)
	if err != nil {
		return 0
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	var hlc string
	err = conn.QueryRow(ctx, "SELECT cluster_logical_timestamp()::STRING").Scan(&hlc)
	cancel()
	conn.Close(context.Background())
	if err != nil {
		return 0
	}
	wall, err := strconv.ParseInt(strings.SplitN(hlc, ".", 2)[0], 10, 64)
	if err != nil {
		return 0
	}
	ts := strconv.FormatInt(wall-4_000_000_000, 10)
	got := map[string]snapshot{}
	var order []string
	for _, n := range nodes {
		s, err := readAt(n, ts)
		if err != nil {
			fmt.Fprintf(os.Stderr, "crc: read at %s through %s: %s\n", ts, n, errText(err))
			continue
		}
		got[n] = s
		order = append(order, n)
	}
	bad := 0
	for _, n := range order {
		s := got[n]
		if t := sum(s.bank); t != total {
			bad++
			fmt.Printf("%s read the bank at %s as %s, which sums to %d, not %d\n", n, ts, fmtBank(s.bank), t, total)
		}
		if n != order[0] && s.String() != got[order[0]].String() {
			bad++
			fmt.Printf("%s and %s read different data at one timestamp %s: %s against %s\n", n, order[0], ts, s, got[order[0]])
		}
	}
	// the current lists, through the first node that answered
	if len(order) > 0 {
		if cc, err := connect(order[0], 5*time.Second); err == nil {
			ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			now, err := listsAt(ctx, cc, "")
			cancel()
			cc.Close(context.Background())
			if err == nil {
				for _, n := range order {
					for k := 0; k < laKeys; k++ {
						if !isPrefix(got[n].lists[k], now[k]) {
							bad++
							fmt.Printf("%s read x%d at %s as %s, which is not a prefix of the later %s\n", n, k, ts, fmtList(got[n].lists[k]), fmtList(now[k]))
						}
					}
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("the reads at one past timestamp disagree (%d differences)\n", bad)
		return 100
	}
	console(fmt.Sprintf("crc: %d nodes read one snapshot at %s", len(order), ts))
	return 0
}

// follower reads the bank at follower_read_timestamp() through a node that the seed picks: it
// must sum to 500 (anytime_follower; exit 100 is a violation).
func follower() int {
	r := rng()
	node := nodes[r.IntN(len(nodes))]
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		return 0
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	b, err := bankAt(ctx, conn, "follower_read_timestamp()")
	if err != nil {
		fmt.Fprintf(os.Stderr, "crc: follower read through %s: %s\n", node, errText(err))
		return 0
	}
	if len(b) == accounts && sum(b) != total {
		fmt.Printf("a follower read through %s gave the bank %s, which sums to %d, not %d\n", node, fmtBank(b), sum(b), total)
		return 100
	}
	return 0
}

// ---- the read-backs and the final checks ----

// anyNode runs f on the first node, from a seed's order, that connects and answers, trying
// until the deadline.
func anyNode(deadline time.Time, f func(n string, conn *pgx.Conn) error) (string, error) {
	var last error
	for {
		for _, n := range nodes {
			conn, err := connect(n, 5*time.Second)
			if err != nil {
				last = err
				continue
			}
			err = f(n, conn)
			conn.Close(context.Background())
			if err == nil {
				return n, nil
			}
			last = err
		}
		if time.Now().After(deadline) {
			return "", last
		}
		time.Sleep(2 * time.Second)
	}
}

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
	have := map[string]string{}
	node, err := anyNode(time.Now().Add(150*time.Second), func(n string, conn *pgx.Conn) error {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		defer cancel()
		rows, err := conn.Query(ctx, "SELECT id, v FROM kv")
		if err != nil {
			return err
		}
		defer rows.Close()
		h := map[string]string{}
		for rows.Next() {
			var id, v string
			if err := rows.Scan(&id, &v); err != nil {
				return err
			}
			h[id] = v
		}
		if err := rows.Err(); err != nil {
			return err
		}
		have = h
		return nil
	})
	if err != nil {
		fmt.Printf("no read-back: no node answered within 150 s: %s\n", errText(err))
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
	fmt.Printf("read back %d writes of %d rows through %s\n", len(items), len(have), node)
	return 0
}

// laFinal reads every list through any node and records one read of all keys (the final
// read-back of the check la).
func laFinal() int {
	const proc = 100001
	var inv []string
	for k := 0; k < laKeys; k++ {
		inv = append(inv, fmt.Sprintf("r:x%d", k))
	}
	hrec("la", proc, "invoke", append([]string{"txn"}, inv...)...)
	var la map[int][]int64
	_, err := anyNode(time.Now().Add(150*time.Second), func(n string, conn *pgx.Conn) error {
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		defer cancel()
		l, err := listsAt(ctx, conn, "")
		if err == nil {
			la = l
		}
		return err
	})
	if err != nil {
		hrec("la", proc, "info", append([]string{"txn"}, inv...)...)
		fmt.Printf("the final read failed: %s\n", errText(err))
		return 2
	}
	var out []string
	for k := 0; k < laKeys; k++ {
		out = append(out, fmt.Sprintf("r:x%d:%s", k, fmtList(la[k])))
	}
	hrec("la", proc, "ok", append([]string{"txn"}, out...)...)
	return 0
}

// checkBank: every node that answers reads the bank as summing to 500 (a final check).
func checkBank() int {
	bad, seen := 0, 0
	for _, n := range nodes {
		conn, err := connect(n, 5*time.Second)
		if err != nil {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		b, err := bankAt(ctx, conn, "")
		cancel()
		conn.Close(context.Background())
		if err != nil {
			fmt.Printf("%s: %s\n", n, errText(err))
			continue
		}
		seen++
		if sum(b) != total || len(b) != accounts {
			bad++
			fmt.Printf("%s reads the bank as %s, which sums to %d, not %d\n", n, fmtBank(b), sum(b), total)
		}
	}
	if seen == 0 {
		fmt.Println("no node read the bank")
		return 2
	}
	if bad > 0 {
		fmt.Println("the bank does not sum to 500")
		return 1
	}
	fmt.Printf("%d nodes read the bank as 500\n", seen)
	return 0
}

// checkReplicas runs CockroachDB's own consistency check, which compares every replica of every
// range by a checksum: no range may be inconsistent (a final check). Since v26 the session needs
// allow_unsafe_internals for crdb_internal.
func checkReplicas() int {
	type row struct {
		rng            int64
		status, detail string
	}
	var rows []row
	node, err := anyNode(time.Now().Add(10*time.Second), func(n string, conn *pgx.Conn) error {
		ctx, cancel := context.WithTimeout(context.Background(), 40*time.Second)
		defer cancel()
		if _, err := conn.Exec(ctx, "SET allow_unsafe_internals = true"); err != nil {
			return err
		}
		rs, err := conn.Query(ctx, "SELECT range_id, status, detail FROM crdb_internal.check_consistency(false, '', '') ORDER BY range_id")
		if err != nil {
			return err
		}
		defer rs.Close()
		var out []row
		for rs.Next() {
			var x row
			if err := rs.Scan(&x.rng, &x.status, &x.detail); err != nil {
				return err
			}
			out = append(out, x)
		}
		if err := rs.Err(); err != nil {
			return err
		}
		rows = out
		return nil
	})
	if err != nil {
		fmt.Printf("the consistency check did not run: %s\n", errText(err))
		return 2
	}
	counts := map[string]int{}
	bad := 0
	for _, x := range rows {
		counts[x.status]++
		if strings.Contains(x.status, "INCONSISTENT") || strings.Contains(x.status, "MISSING") {
			if bad++; bad <= 5 {
				fmt.Printf("range r%d: %s: %s\n", x.rng, x.status, strings.ReplaceAll(x.detail, "\n", " "))
			}
		}
	}
	var cs []string
	for s, n := range counts {
		cs = append(cs, fmt.Sprintf("%s %d", s, n))
	}
	sort.Strings(cs)
	if bad > 0 {
		fmt.Printf("%d ranges have replicas that differ (through %s): %s\n", bad, node, strings.Join(cs, ", "))
		return 1
	}
	fmt.Printf("%d ranges checked through %s: %s\n", len(rows), node, strings.Join(cs, ", "))
	return 0
}

// checkServes: a write and a read through each of at least two nodes succeed (an eventually_
// check after the heal: a cluster of three serves with one node down).
func checkServes() int {
	ok := 0
	var why []string
	for _, n := range nodes {
		conn, err := connect(n, 5*time.Second)
		if err != nil {
			why = append(why, n+": "+errText(err))
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		_, err = conn.Exec(ctx, "UPSERT INTO sv(id, node) VALUES ($1, $1)", n)
		if err == nil {
			var x string
			err = conn.QueryRow(ctx, "SELECT node FROM sv WHERE id = $1", n).Scan(&x)
		}
		cancel()
		conn.Close(context.Background())
		if err != nil {
			why = append(why, n+": "+errText(err))
			continue
		}
		ok++
	}
	if ok >= 2 {
		return 0
	}
	fmt.Printf("only %d nodes served a write and a read: %s\n", ok, strings.Join(why, "; "))
	return 1
}

// checkClockStops reads every node's log files on its own file system and counts the nodes that
// stopped themselves with "clock synchronization error". A node does so when its clock is more
// than 80 % of --max-offset away from at least half of the nodes it knows. One node's clock that
// is off must stop that node alone: at most one of the three nodes may have stopped so (a final
// check).
func checkClockStops() int {
	var stopped []string
	for i, n := range nodes {
		files, _ := filepath.Glob(fmt.Sprintf("/data/vol/roach%d/logs/cockroach.*.log", i+1))
		count, example := 0, ""
		for _, f := range files {
			for _, l := range readLines(f) {
				if strings.HasPrefix(l, "F") && strings.Contains(l, "clock synchronization error") {
					count++
					if j := strings.Index(l, "clock synchronization error"); j >= 0 && example == "" {
						example = l[j:]
					}
				}
			}
		}
		if count > 0 {
			stopped = append(stopped, n)
			fmt.Printf("%s stopped itself %d times: %s\n", n, count, example)
		}
	}
	if len(stopped) > 1 {
		fmt.Printf("%d of 3 nodes stopped themselves for clock synchronization (%s)\n", len(stopped), strings.Join(stopped, ", "))
		return 1
	}
	fmt.Printf("%d of 3 nodes stopped themselves for clock synchronization\n", len(stopped))
	return 0
}

// ---- setup ----

func cmdReady(addr string) int {
	c, err := net.DialTimeout("tcp", addr+":"+rpcPort, 3*time.Second)
	if err != nil {
		return 1
	}
	c.Close()
	return 0
}

func cmdInit() int {
	until := time.Now().Add(170 * time.Second)
	for _, n := range nodes {
		for {
			conn, err := connect(n, 5*time.Second)
			if err == nil {
				ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
				var one int
				err = conn.QueryRow(ctx, "SELECT 1").Scan(&one)
				cancel()
				conn.Close(context.Background())
				if err == nil {
					break
				}
			}
			if time.Now().After(until) {
				fmt.Printf("%s does not serve SQL: %s\n", n, errText(err))
				return 1
			}
			time.Sleep(2 * time.Second)
		}
	}
	conn, err := connect("roach1", 5*time.Second)
	if err != nil {
		fmt.Println(errText(err))
		return 1
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()
	stmts := []string{
		"CREATE TABLE IF NOT EXISTS kv (id STRING PRIMARY KEY, v STRING NOT NULL)",
		"CREATE TABLE IF NOT EXISTS la (k INT PRIMARY KEY, v STRING NOT NULL)",
		"CREATE TABLE IF NOT EXISTS acct (id INT PRIMARY KEY, bal INT NOT NULL)",
		"CREATE TABLE IF NOT EXISTS sv (id STRING PRIMARY KEY, node STRING)",
		"UPSERT INTO acct(id, bal) VALUES (0, 100), (1, 100), (2, 100), (3, 100), (4, 100)",
	}
	if v := os.Getenv("CRC_PERSIST"); v != "" && v != "0" {
		stmts = append(stmts, "SET CLUSTER SETTING server.clock.persist_upper_bound_interval = '"+v+"'")
	}
	for _, s := range stmts {
		if _, err := conn.Exec(ctx, s); err != nil {
			fmt.Printf("%s: %s\n", s, errText(err))
			return 1
		}
	}
	// A new cluster replicates its ranges to the three nodes over tens of seconds. Until every
	// range has three replicas, the loss of one node can leave a range without a quorum, so the
	// workload starts only after that.
	if _, err := conn.Exec(ctx, "SET allow_unsafe_internals = true"); err != nil {
		fmt.Println(errText(err))
		return 1
	}
	wait := time.Now().Add(150 * time.Second)
	for {
		c2, cancel2 := context.WithTimeout(context.Background(), 10*time.Second)
		var under, all int
		err := conn.QueryRow(c2, "SELECT count(*) FILTER (WHERE array_length(replicas, 1) < 3), count(*) FROM crdb_internal.ranges_no_leases").Scan(&under, &all)
		cancel2()
		if err == nil && under == 0 {
			fmt.Printf("three nodes serve SQL, tables created, %d ranges with three replicas each\n", all)
			return 0
		}
		if time.Now().After(wait) {
			fmt.Printf("%d of %d ranges have fewer than three replicas after 150 s (%s)\n", under, all, errText(err))
			return 1
		}
		time.Sleep(2 * time.Second)
	}
}

// cmdPrimary prints the service of the node that holds the lease of the table la's range.
func cmdPrimary() int {
	_, err := anyNode(time.Now().Add(15*time.Second), func(n string, conn *pgx.Conn) error {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if _, err := conn.Exec(ctx, "SET allow_unsafe_internals = true"); err != nil {
			return err
		}
		var addr string
		err := conn.QueryRow(ctx, `SELECT g.address FROM [SHOW RANGES FROM TABLE la WITH DETAILS] AS r
			JOIN crdb_internal.gossip_nodes AS g ON g.node_id = r.lease_holder LIMIT 1`).Scan(&addr)
		if err != nil {
			return err
		}
		host := strings.SplitN(addr, ":", 2)[0]
		for s, a := range addrs {
			if a == host {
				fmt.Println(s)
				return nil
			}
		}
		return fmt.Errorf("no service at %s", addr)
	})
	if err != nil {
		return 1
	}
	return 0
}

func cmdSQL(node, q string) int {
	conn, err := connect(node, 5*time.Second)
	if err != nil {
		fmt.Println(errText(err))
		return 1
	}
	defer conn.Close(context.Background())
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	rows, err := conn.Query(ctx, q)
	if err != nil {
		fmt.Println(errText(err))
		return 1
	}
	defer rows.Close()
	for rows.Next() {
		v, err := rows.Values()
		if err != nil {
			fmt.Println(errText(err))
			return 1
		}
		var s []string
		for _, x := range v {
			s = append(s, fmt.Sprint(x))
		}
		fmt.Println(strings.Join(s, "\t"))
	}
	if err := rows.Err(); err != nil {
		fmt.Println(errText(err))
		return 1
	}
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: crc ready|init|primary|op|txn|transfer|read|follower|readback|lafinal|check|sql ...")
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
	case "transfer":
		rc = transfer()
	case "read":
		rc = readNodes()
	case "follower":
		rc = follower()
	case "readback":
		rc = readBack()
	case "lafinal":
		rc = laFinal()
	case "check":
		switch a[0] {
		case "bank":
			rc = checkBank()
		case "replicas":
			rc = checkReplicas()
		case "serves":
			rc = checkServes()
		case "clockstops":
			rc = checkClockStops()
		default:
			rc = 2
		}
	case "sql":
		rc = cmdSQL(a[0], a[1])
	default:
		rc = 2
	}
	os.Exit(rc)
}
