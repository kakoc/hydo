// chc is the test client of the ClickHouse example: the schema, the workload's inserts and
// reads, the drain of the messages check, and the final checks. It speaks ClickHouse's HTTP
// interface and uses only Go's standard library.
//
//	chc schema         create the four replicated tables on both replicas (first_schema)
//	chc op <kind>      one insert: dedup | async | quorum | qseq (the composer's HYDO_* variables)
//	chc read           read t_qpar or t_qseq on one replica and judge the acknowledged blocks
//	chc drain          the final: command of the messages check: every block of every table
//	chc check <rule>   converge | blocks: a final rule over both replicas
//	chc ready <host>   exit 0 when the server answers SELECT 1
//	chc dump <host>    print one replica's blocks (for a look by hand)
//
// Every insert writes one block: rows (blk, i, n, v) with i = 0..n-1, where blk is the
// command's id. A block's rows belong to no other command. The four tables:
//
//	t_dedup  synchronous inserts (async_insert = 0) that rely on the block's hash, or name it
//	         with insert_deduplication_token
//	t_async  async_insert = 1 with wait_for_async_insert = 1 (both on by default since 26.2)
//	t_qpar   insert_quorum = 2 with insert_quorum_parallel = 1 (the default), async or not
//	t_qseq   insert_quorum = 2 with insert_quorum_parallel = 0 and async_insert = 0, read with
//	         select_sequential_consistency = 1
//
// An insert is tried up to four times with the same rows, on either replica, until one try is
// acknowledged. After an acknowledgement the client sends the block once more in one insert of
// three (a client that lost the answer). Every try writes HYDO-HIST records of the history `ch`
// (hydo guide messages): the channel is the table, the message is b<blk>. An acknowledged block
// writes <table>~<blk>~<n> to $HYDO_ACKED; every block writes <table>.<blk> ack|failed|unknown
// to $HYDO_OUTCOMES.
package main

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

var replicas = []string{"ch1", "ch2"}

var tables = []string{"t_dedup", "t_async", "t_qpar", "t_qseq"}

const hist = "ch"

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func die(code int, format string, a ...any) {
	fmt.Printf(format+"\n", a...)
	os.Exit(code)
}

// ---- the clock that a clock step does not move ----

func uptime() float64 {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return 0
	}
	f := strings.Fields(string(b))
	if len(f) == 0 {
		return 0
	}
	v, _ := strconv.ParseFloat(f[0], 64)
	return v
}

// ---- the history recorder (the wire format of hydo_hist.sh) ----

var histOut *os.File

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

// rec writes one record: HYDO-HIST ch <process> <type> <time> <args>
func rec(proc int, typ string, args ...string) {
	if histOut == nil {
		return
	}
	histOut.WriteString(fmt.Sprintf("HYDO-HIST %s %d %s %s %s\n", hist, proc, typ, uptimeNs(), strings.Join(args, " ")))
}

// note prints a line to the command's log and to the console, so that a run's one.log shows
// why an insert was not acknowledged.
func note(format string, a ...any) {
	line := fmt.Sprintf(format, a...)
	if len(line) > 240 {
		line = line[:240]
	}
	fmt.Println(line)
	if histOut != nil {
		histOut.WriteString("chc: " + strings.ReplaceAll(line, "\n", " ") + "\n")
	}
}

// ---- records for the checks ----

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		die(3, "append %s: %v", path, err)
	}
	defer f.Close()
	if _, err := f.WriteString(line + "\n"); err != nil {
		os.Exit(3)
	}
}

func readLines(path string) []string {
	if path == "" {
		return nil
	}
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

type ackedBlock struct {
	table string
	blk   uint64
	n     int
}

// acked reads $HYDO_ACKED: one <table>~<blk>~<n> per acknowledged block.
func acked() []ackedBlock {
	var out []ackedBlock
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, "~")
		if len(f) != 3 {
			continue
		}
		b, err1 := strconv.ParseUint(f[1], 10, 64)
		n, err2 := strconv.Atoi(f[2])
		if err1 == nil && err2 == nil {
			out = append(out, ackedBlock{f[0], b, n})
		}
	}
	return out
}

// failedBlocks reads $HYDO_OUTCOMES: the blocks whose every try was refused before it wrote.
func failedBlocks() map[string]bool {
	out := map[string]bool{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) == 2 && f[1] == "failed" {
			out[f[0]] = true
		}
	}
	return out
}

// ---- ClickHouse over HTTP ----

// chError is an answer of the server with an exception: its code and its first line.
type chError struct {
	code int
	msg  string
}

func (e *chError) Error() string { return fmt.Sprintf("Code %d: %s", e.code, e.msg) }

// sent tells whether a request reached the server: a refused connection never did.
type notSent struct{ err error }

func (e *notSent) Error() string { return "not sent: " + e.err.Error() }

func do(host, query string, settings map[string]string, body string, timeout time.Duration) (string, error) {
	q := url.Values{}
	q.Set("query", query)
	for k, v := range settings {
		q.Set(k, v)
	}
	u := "http://" + host + ":8123/?" + q.Encode()
	var rd io.Reader
	method := "GET"
	if body != "" {
		rd = strings.NewReader(body)
		method = "POST"
	} else if !strings.HasPrefix(strings.ToUpper(query), "SELECT") {
		method = "POST"
	}
	req, err := http.NewRequest(method, u, rd)
	if err != nil {
		return "", err
	}
	c := http.Client{
		Timeout: timeout,
		Transport: &http.Transport{
			DialContext:       (&net.Dialer{Timeout: 2 * time.Second}).DialContext,
			DisableKeepAlives: true,
		},
	}
	resp, err := c.Do(req)
	if err != nil {
		var op *net.OpError
		if errors.As(err, &op) && op.Op == "dial" {
			return "", &notSent{err}
		}
		return "", err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", err
	}
	if code := resp.Header.Get("X-ClickHouse-Exception-Code"); code != "" || resp.StatusCode != 200 {
		n, _ := strconv.Atoi(code)
		msg := strings.SplitN(strings.TrimSpace(string(b)), "\n", 2)[0]
		if len(msg) > 300 {
			msg = msg[:300]
		}
		return "", &chError{n, msg}
	}
	// An exception after the first bytes of an answer arrives in the body.
	if i := strings.Index(string(b), "DB::Exception"); i >= 0 {
		return "", &chError{-1, strings.TrimSpace(string(b))}
	}
	return string(b), nil
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

// refusedBefore lists the error codes that the server raises before it writes any part.
var refusedBefore = map[int]bool{
	60:  true, // UNKNOWN_TABLE
	81:  true, // UNKNOWN_DATABASE
	164: true, // READONLY
	202: true, // TOO_MANY_SIMULTANEOUS_QUERIES
	242: true, // TABLE_IS_READ_ONLY
	252: true, // TOO_MANY_PARTS
	285: true, // TOO_FEW_LIVE_REPLICAS
	286: true, // UNSATISFIED_QUORUM_FOR_PREVIOUS_WRITE
}

// classify turns an insert's error into an outcome. Only a request that never reached the
// server, or an exception raised before the write, is a failure for certain; everything else
// (a timeout, a lost connection, an unknown status, a quorum timeout) is unknown.
func classify(err error) outcome {
	if err == nil {
		return ack
	}
	var ns *notSent
	if errors.As(err, &ns) {
		return failed
	}
	var ce *chError
	if errors.As(err, &ce) && refusedBefore[ce.code] {
		return failed
	}
	return unknown
}

// ---- the schema ----

func confValue(key, def string) string {
	for _, l := range readLines("/etc/chc.conf") {
		if k, v, ok := strings.Cut(l, "="); ok && strings.TrimSpace(k) == key {
			return strings.TrimSpace(v)
		}
	}
	return def
}

// ddl creates one table with the fsync settings that /etc/chc.conf names (the build parameter
// fsync): 0 keeps MergeTree's defaults, which sync nothing; insert syncs a part and its
// directory after an insert (fsync_after_insert, fsync_part_directory); 1 also syncs a part after
// a merge and after a fetch from the other replica (min_rows_to_fsync_after_merge,
// min_compressed_bytes_to_fsync_after_merge, min_compressed_bytes_to_fsync_after_fetch).
func ddl(t string) string {
	ins, more := "0", "0"
	switch confValue("fsync", "0") {
	case "insert":
		ins = "1"
	case "1":
		ins, more = "1", "1"
	}
	return fmt.Sprintf("CREATE TABLE IF NOT EXISTS default.%s (blk UInt64, i UInt32, n UInt32, v String) "+
		"ENGINE = ReplicatedMergeTree('/clickhouse/tables/1/%s', '{replica}') ORDER BY (blk, i) "+
		"SETTINGS fsync_after_insert = %s, fsync_part_directory = %s, "+
		"min_rows_to_fsync_after_merge = %s, min_compressed_bytes_to_fsync_after_merge = %s, "+
		"min_compressed_bytes_to_fsync_after_fetch = %s", t, t, ins, ins, more, more, more)
}

func schema() {
	end := uptime() + 150
	for _, h := range replicas {
		for _, t := range tables {
			for {
				_, err := do(h, ddl(t), nil, "", 30*time.Second)
				if err == nil {
					break
				}
				if uptime() > end {
					die(1, "schema: %s on %s: %v", t, h, err)
				}
				time.Sleep(time.Second)
			}
		}
	}
	fmt.Println("schema: four tables on both replicas, fsync =", confValue("fsync", "0"))
}

// ---- inserts ----

type kind struct {
	table    string
	settings map[string]string
	token    bool // insert_deduplication_token names the block (t_dedup, one insert of two)
}

func kindOf(name string, r *rand.Rand) kind {
	base := map[string]string{"insert_keeper_max_retries": "20"}
	switch name {
	case "dedup":
		base["async_insert"] = "0" // on by default since 26.2: this table takes synchronous inserts
		return kind{table: "t_dedup", settings: base, token: r.IntN(2) == 0}
	case "async":
		base["async_insert"] = "1"
		base["wait_for_async_insert"] = "1"
		base["async_insert_busy_timeout_ms"] = "200"
		base["async_insert_use_adaptive_busy_timeout"] = "0"
		return kind{table: "t_async", settings: base}
	case "quorum":
		base["insert_quorum"] = "2"
		base["insert_quorum_parallel"] = "1"
		base["insert_quorum_timeout"] = "10000"
		base["async_insert"] = strconv.Itoa(r.IntN(2)) // either path: a quorum allows async inserts when parallel
		return kind{table: "t_qpar", settings: base}
	case "qseq":
		base["insert_quorum"] = "2"
		base["insert_quorum_parallel"] = "0"
		base["insert_quorum_timeout"] = "10000"
		base["async_insert"] = "0" // a sequential quorum refuses async inserts (UNSUPPORTED_PARAMETER)
		return kind{table: "t_qseq", settings: base}
	}
	die(2, "unknown kind %q (dedup | async | quorum | qseq)", name)
	return kind{}
}

const letters = "abcdefghijklmnopqrstuvwxyz0123456789"

func blockRows(blk uint64, n int, r *rand.Rand) string {
	var b strings.Builder
	for i := 0; i < n; i++ {
		l := r.IntN(64)
		v := make([]byte, l)
		for j := range v {
			v[j] = letters[r.IntN(len(letters))]
		}
		fmt.Fprintf(&b, "%d\t%d\t%d\t%s\n", blk, i, n, v)
	}
	return b.String()
}

func opInsert(name string, id int, r *rand.Rand) {
	k := kindOf(name, r)
	blk := uint64(id)
	n := 1 + r.IntN(30)
	rows := blockRows(blk, n, r)
	m := fmt.Sprintf("b%d", blk)
	settings := map[string]string{}
	for a, b := range k.settings {
		settings[a] = b
	}
	if k.token {
		settings["insert_deduplication_token"] = m
	}
	query := fmt.Sprintf("INSERT INTO default.%s (blk, i, n, v) FORMAT TSV", k.table)
	host := replicas[r.IntN(len(replicas))]
	try := func(h string) outcome {
		rec(id, "invoke", "send", k.table, m)
		_, err := do(h, query, settings, rows, 12*time.Second)
		o := classify(err)
		switch o {
		case ack:
			rec(id, "ok", "send", k.table, m)
		case failed:
			rec(id, "fail", "send", k.table, m)
		default:
			rec(id, "info", "send", k.table, m)
		}
		if err != nil {
			note("%s %s on %s: %v", k.table, m, h, err)
		}
		return o
	}
	got := failed
	anyUnknown := false
	start := uptime()
	for attempt := 0; attempt < 4 && uptime()-start < 30; attempt++ {
		if attempt > 0 && r.IntN(2) == 0 {
			host = other(host)
		}
		o := try(host)
		if o == ack {
			got = ack
			break
		}
		if o == unknown {
			anyUnknown = true
		}
		time.Sleep(time.Duration(300+r.IntN(700)) * time.Millisecond)
	}
	if got != ack && anyUnknown {
		got = unknown
	}
	id2 := fmt.Sprintf("%s.%d", k.table, blk)
	switch got {
	case ack:
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("%s~%d~%d", k.table, blk, n))
		appendLine(os.Getenv("HYDO_OUTCOMES"), id2+" ack")
	case failed:
		appendLine(os.Getenv("HYDO_OUTCOMES"), id2+" failed")
	default:
		appendLine(os.Getenv("HYDO_OUTCOMES"), id2+" unknown")
	}
	if got != ack {
		fmt.Printf("%s %s not acknowledged (%s)\n", k.table, m, map[outcome]string{failed: "failed", unknown: "unknown"}[got])
		os.Exit(1)
	}
	// A quorum of 2 on 2 replicas: the other replica holds the whole block now.
	if k.table == "t_qpar" || k.table == "t_qseq" {
		h := other(host)
		out, err := do(h, fmt.Sprintf("SELECT count(), uniqExact(i) FROM default.%s WHERE blk = %d FORMAT TSV", k.table, blk), nil, "", 5*time.Second)
		if err == nil {
			f := strings.Fields(out)
			if len(f) == 2 && (f[0] != strconv.Itoa(n) || f[1] != strconv.Itoa(n)) {
				die(100, "VIOLATION: %s %s was acknowledged with insert_quorum = 2 by %s, but %s holds %s rows of %d (%s distinct)", k.table, m, host, h, f[0], n, f[1])
			}
		}
	}
	// A client that lost the answer sends the acknowledged block again.
	if r.IntN(3) == 0 {
		h := replicas[r.IntN(len(replicas))]
		try(h)
	}
}

func other(h string) string {
	if h == replicas[0] {
		return replicas[1]
	}
	return replicas[0]
}

// ---- reads ----

type blockCount struct {
	count, distinct, n int
}

func readBlocks(host, table string, settings map[string]string, timeout time.Duration) (map[uint64]blockCount, error) {
	out, err := do(host, fmt.Sprintf("SELECT blk, count(), uniqExact(i), max(n) FROM default.%s GROUP BY blk ORDER BY blk FORMAT TSV", table), settings, "", timeout)
	if err != nil {
		return nil, err
	}
	m := map[uint64]blockCount{}
	sc := bufio.NewScanner(strings.NewReader(out))
	for sc.Scan() {
		f := strings.Fields(sc.Text())
		if len(f) != 4 {
			continue
		}
		b, _ := strconv.ParseUint(f[0], 10, 64)
		c, _ := strconv.Atoi(f[1])
		d, _ := strconv.Atoi(f[2])
		n, _ := strconv.Atoi(f[3])
		m[b] = blockCount{c, d, n}
	}
	return m, nil
}

// opRead reads t_qseq with select_sequential_consistency = 1, or t_qpar without it, on one
// replica. Every block acknowledged before the read starts must be there, whole and once.
func opRead(r *rand.Rand) {
	table := "t_qseq"
	settings := map[string]string{"select_sequential_consistency": "1"}
	if r.IntN(2) == 0 {
		table = "t_qpar"
		settings = nil
	}
	host := replicas[r.IntN(len(replicas))]
	before := acked()
	got, err := readBlocks(host, table, settings, 8*time.Second)
	if err != nil {
		fmt.Printf("read %s on %s: %v\n", table, host, err)
		return
	}
	for _, a := range before {
		if a.table != table {
			continue
		}
		c, ok := got[a.blk]
		switch {
		case !ok:
			die(100, "VIOLATION: %s b%d was acknowledged with insert_quorum = 2 before the read, but a read on %s (%v) has no row of it", table, a.blk, host, settings)
		case c.distinct < a.n:
			die(100, "VIOLATION: %s b%d was acknowledged with insert_quorum = 2 before the read, but a read on %s (%v) has %d of its %d rows", table, a.blk, host, settings, c.distinct, a.n)
		case c.count > c.distinct:
			die(100, "VIOLATION: a read on %s (%v) has %s b%d %d times", host, settings, table, a.blk, c.count/c.distinct)
		}
	}
	fmt.Printf("read %s on %s: %d blocks, every acknowledged one whole\n", table, host, len(got))
}

// ---- the drain and the final checks ----

// waitReady waits until the server answers, up to the uptime end.
func waitReady(host string, end float64) bool {
	for uptime() < end {
		if _, err := do(host, "SELECT 1", nil, "", 3*time.Second); err == nil {
			return true
		}
		time.Sleep(time.Second)
	}
	return false
}

// syncReplica waits until the replica has fetched every part in the replication log of each
// table, at most until the uptime end. A part that no replica holds any more keeps a fetch in
// the queue for good, so the wait ends without success then.
func syncReplica(host string, end float64) {
	for _, t := range tables {
		for uptime() < end {
			left := end - uptime()
			if left > 60 {
				left = 60
			}
			_, err := do(host, "SYSTEM SYNC REPLICA default."+t, map[string]string{"receive_timeout": strconv.Itoa(int(left) + 1)}, "", time.Duration(left+3)*time.Second)
			if err == nil {
				break
			}
			fmt.Printf("sync %s on %s: %v\n", t, host, err)
			time.Sleep(time.Second)
		}
	}
}

func drain() {
	end := uptime() + 420
	for gi, g := range replicas {
		if !waitReady(g, end) {
			die(1, "drain: %s does not answer", g)
		}
		syncReplica(g, uptime()+120)
		for ti, t := range tables {
			proc := 1000000 + 10*gi + ti
			rec(proc, "invoke", "sub", t, g, "from=start")
			rec(proc, "ok", "sub", t, g, "from=start")
			var got map[uint64]blockCount
			for {
				rec(proc, "invoke", "recv", t, g)
				var err error
				got, err = readBlocks(g, t, nil, 30*time.Second)
				if err == nil {
					break
				}
				rec(proc, "info", "recv", t, g)
				fmt.Printf("drain: read %s on %s: %v\n", t, g, err)
				if uptime() > end {
					die(1, "drain: %s on %s does not answer", t, g)
				}
				time.Sleep(time.Second)
			}
			var blks []uint64
			for b := range got {
				blks = append(blks, b)
			}
			sort.Slice(blks, func(i, j int) bool { return blks[i] < blks[j] })
			var items []string
			for _, b := range blks {
				c := got[b]
				copies := 1
				if c.distinct > 0 && c.count/c.distinct > 1 {
					copies = c.count / c.distinct
				}
				for k := 1; k <= copies; k++ {
					items = append(items, fmt.Sprintf("b%d;o=%s.%d", b, g, k))
				}
			}
			if len(items) == 0 {
				rec(proc, "ok", "recv", t, g)
			}
			for len(items) > 0 {
				n := min(40, len(items))
				rec(proc, "ok", append([]string{"recv", t, g}, items[:n]...)...)
				items = items[n:]
				if len(items) > 0 {
					rec(proc, "invoke", "recv", t, g)
				}
			}
			rec(proc, "ok", "drained", t, g)
			fmt.Printf("drain: %s on %s: %d blocks\n", t, g, len(blks))
		}
	}
}

// checkConverge: after the heal each replica holds the same blocks of every table, each with
// the same number of rows.
func checkConverge() {
	end := uptime() + 90
	for _, h := range replicas {
		if !waitReady(h, end) {
			die(2, "%s does not answer", h)
		}
		syncReplica(h, uptime()+40)
	}
	bad := 0
	for _, t := range tables {
		a, err1 := readBlocks(replicas[0], t, nil, 30*time.Second)
		b, err2 := readBlocks(replicas[1], t, nil, 30*time.Second)
		if err1 != nil || err2 != nil {
			die(2, "read %s: %v %v", t, err1, err2)
		}
		keys := map[uint64]bool{}
		for k := range a {
			keys[k] = true
		}
		for k := range b {
			keys[k] = true
		}
		var ks []uint64
		for k := range keys {
			ks = append(ks, k)
		}
		sort.Slice(ks, func(i, j int) bool { return ks[i] < ks[j] })
		for _, k := range ks {
			if a[k] != b[k] {
				bad++
				if bad <= 5 {
					fmt.Printf("%s b%d: %s has %d rows (%d distinct), %s has %d rows (%d distinct)\n", t, k, replicas[0], a[k].count, a[k].distinct, replicas[1], b[k].count, b[k].distinct)
				}
			}
		}
	}
	if bad > 0 {
		die(1, "the replicas differ in %d blocks after the heal", bad)
	}
	fmt.Println("both replicas hold the same blocks")
}

// checkBlocks: on each replica every block is whole (all its rows, each once or each the
// same number of times), and no block whose every try was refused is there.
func checkBlocks() {
	end := uptime() + 90
	fl := failedBlocks()
	partial, refused := 0, 0
	for _, h := range replicas {
		if !waitReady(h, end) {
			die(2, "%s does not answer", h)
		}
		syncReplica(h, uptime()+40)
		for _, t := range tables {
			got, err := readBlocks(h, t, nil, 30*time.Second)
			if err != nil {
				die(2, "read %s on %s: %v", t, h, err)
			}
			for b, c := range got {
				if c.distinct != c.n || c.count%c.n != 0 {
					partial++
					fmt.Printf("%s b%d on %s is partial: %d rows, %d distinct of %d\n", t, b, h, c.count, c.distinct, c.n)
				}
				if fl[fmt.Sprintf("%s.%d", t, b)] {
					refused++
					fmt.Printf("%s b%d is on %s, although every try of it was refused before the write\n", t, b, h)
				}
			}
		}
	}
	if partial+refused > 0 {
		die(1, "%d partial blocks and %d refused blocks stored on the replicas", partial, refused)
	}
	fmt.Println("every block whole on both replicas, no refused block written")
}

func dump(host string) {
	for _, t := range tables {
		got, err := readBlocks(host, t, nil, 30*time.Second)
		if err != nil {
			fmt.Printf("%s: %v\n", t, err)
			continue
		}
		var ks []uint64
		for k := range got {
			ks = append(ks, k)
		}
		sort.Slice(ks, func(i, j int) bool { return ks[i] < ks[j] })
		fmt.Printf("%s: %d blocks\n", t, len(ks))
		for _, k := range ks {
			fmt.Printf("  b%d %+v\n", k, got[k])
		}
	}
}

func main() {
	if os.Getenv("HYDO_HIST_OUT") != "off" {
		path := env("HYDO_HIST_OUT", "/dev/console")
		if f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644); err == nil {
			histOut = f
		}
	}
	if len(os.Args) < 2 {
		die(2, "usage: chc schema | op <kind> | read | drain | check converge|blocks | ready <host> | dump <host>")
	}
	id, _ := strconv.Atoi(os.Getenv("HYDO_CMD_ID"))
	seed, _ := strconv.ParseUint(os.Getenv("HYDO_SEED"), 10, 64)
	r := rand.New(rand.NewPCG(seed, uint64(id)))
	switch os.Args[1] {
	case "schema":
		schema()
	case "op":
		if len(os.Args) < 3 {
			die(2, "op needs a kind")
		}
		opInsert(os.Args[2], id, r)
	case "read":
		opRead(r)
	case "drain":
		drain()
	case "check":
		if len(os.Args) < 3 {
			die(2, "check needs a rule")
		}
		switch os.Args[2] {
		case "converge":
			checkConverge()
		case "blocks":
			checkBlocks()
		default:
			die(2, "unknown rule %q", os.Args[2])
		}
	case "ready":
		if len(os.Args) < 3 {
			die(2, "ready needs a host")
		}
		if _, err := do(os.Args[2], "SELECT 1", nil, "", 3*time.Second); err != nil {
			die(1, "%v", err)
		}
	case "dump":
		dump(os.Args[2])
	default:
		die(2, "unknown command %q", os.Args[1])
	}
}
