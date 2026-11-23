// gtc is the test client of the GreptimeDB example: the workload's operations, the wait for the
// cluster, the drain of the messages check and the final checks. It speaks GreptimeDB's HTTP SQL
// API (POST /v1/sql) to the two frontends and uses only Go's standard library.
//
//	gtc settle [s]     wait for both datanodes and both frontends, create the two tables, and
//	                   wait until every region answers
//	gtc write          one write: 3 rows into hydo_rows and the same 3 rows into hydo_log
//	gtc read           read both tables during the faults; exit 100 when an acknowledged row is
//	                   missing, returned twice or has another value, while the frontend answered
//	gtc admin          one flush, compaction or region migration (ADMIN functions)
//	gtc ddl            one CREATE, ALTER or DROP of a table d<entry>
//	gtc drain          the messages check's final read of both tables
//	gtc check query    count(*) and sum(v) of each table agree with the acknowledged rows
//	gtc check ddl      every acknowledged DDL took effect, and every table d<n> answers
//	gtc check leaders  every region of the two tables has one leader on a live datanode
//	gtc dump           print every row and the regions' peers (for a look by hand)
//
// Each write has an entry number w (HYDO_CMD_ID) and writes the rows (w, k, t0, v) for k = 0..2
// at the client's wall clock t0. The tables are partitioned on k into 3 regions each, so a write
// reaches every region. hydo_rows keeps GreptimeDB's default deduplication (the last row of a
// key and timestamp wins); hydo_log is an append-mode table, which keeps every row it took. A
// row's value is a function of its key (valueOf), so every reader can tell a wrong value from a
// right one.
//
// Records: the history `gt` of the messages check (hydo guide messages), one record before the
// request and one after the reply, written to /dev/console, on the channels r (hydo_rows) and l
// (hydo_log); for an acknowledged insert the token <ch>~<entry>~<t0>~<uptime ms>~<boot> in
// $HYDO_ACKED and its outcome in $HYDO_OUTCOMES; for an acknowledged DDL statement the token
// d~<what>~<table>[~<column>], and u~<what>~<table>[~<column>] for one without an answer, whose
// outcome is unknown. Before each insert the client writes the console event gt.write
// (an assertion record, the event of A= reactions), and before each DDL statement gt.ddl.
// Deadlines use /proc/uptime, which a clock step (T=) does not move.
package main

import (
	"encoding/json"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

const (
	hist   = "gt"
	nKeys  = 3
	tRows  = "hydo_rows"
	tLog   = "hydo_log"
	readMs = 0 // the read judges every row acknowledged before it started
)

var (
	frontends = strings.Split(env("GTC_FRONTENDS", "http://frontend0:4000,http://frontend1:4000"), ",")
	datanodes = strings.Split(env("GTC_DATANODES", "http://datanode0:5000,http://datanode1:5000"), ",")
	histOut   *os.File
	tables    = []struct{ name, ch string }{{tRows, "r"}, {tLog, "l"}}
)

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

// ---- time and records ----

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

func bootID() string {
	b, _ := os.ReadFile("/proc/sys/kernel/random/boot_id")
	s := strings.TrimSpace(string(b))
	if len(s) > 8 {
		s = s[:8]
	}
	if s == "" {
		s = "-"
	}
	return s
}

// rec writes one record: HYDO-HIST gt <process> <type> <uptime ns> <args>
func rec(proc int64, typ string, args ...string) {
	if histOut == nil {
		return
	}
	line := fmt.Sprintf("HYDO-HIST %s %d %s %d %s\n", hist, proc, typ, uptimeMs()*1000000, strings.Join(args, " "))
	histOut.WriteString(line) // one write: one record
}

// event writes an SDK assertion record to the console: the event of a reaction (A=<id>:...).
// The file is opened for each record, so that a kill-now relay armed after the command started
// sees it.
func event(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	fmt.Fprintf(f, "HYDO-ASSERT %s\n", b)
}

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

func cmdID() int64 {
	id, err := strconv.ParseInt(os.Getenv("HYDO_CMD_ID"), 10, 64)
	if err != nil {
		fmt.Println("HYDO_CMD_ID is not a number")
		os.Exit(2)
	}
	return id
}

func rng() *rand.Rand {
	s, _ := strconv.ParseUint(os.Getenv("HYDO_SEED"), 10, 64)
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

// ---- SQL over HTTP ----

var httpc = &http.Client{Timeout: 30 * time.Second}

type sqlAnswer struct {
	Output []struct {
		AffectedRows *int64 `json:"affectedrows"`
		Records      *struct {
			Rows [][]any `json:"rows"`
		} `json:"records"`
	} `json:"output"`
	Code  int    `json:"code"`
	Error string `json:"error"`
}

// sqlErr is an answer that is not a success: status 0 means no answer at all
type sqlErr struct {
	status int
	msg    string
}

func (e *sqlErr) Error() string {
	if e.status == 0 {
		return "no answer: " + e.msg
	}
	return fmt.Sprintf("status %d: %s", e.status, e.msg)
}

// sql runs one statement on a frontend and returns the rows of its first output
func sql(fe, stmt string) ([][]any, *int64, error) {
	form := url.Values{"sql": {stmt}}
	resp, err := httpc.PostForm(fe+"/v1/sql?db=public", form)
	if err != nil {
		return nil, nil, &sqlErr{0, err.Error()}
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, nil, &sqlErr{0, err.Error()}
	}
	var a sqlAnswer
	if err := json.Unmarshal(b, &a); err != nil {
		return nil, nil, &sqlErr{resp.StatusCode, fmt.Sprintf("%.300s", b)}
	}
	if resp.StatusCode != 200 || a.Error != "" {
		return nil, nil, &sqlErr{resp.StatusCode, fmt.Sprintf("code %d %.300s", a.Code, a.Error)}
	}
	if len(a.Output) == 0 {
		return nil, nil, nil
	}
	o := a.Output[0]
	if o.Records != nil {
		return o.Records.Rows, nil, nil
	}
	return nil, o.AffectedRows, nil
}

func num(v any) int64 {
	switch x := v.(type) {
	case float64:
		return int64(x)
	case string:
		n, _ := strconv.ParseInt(x, 10, 64)
		return n
	}
	return -1
}

func str(v any) string {
	if s, ok := v.(string); ok {
		return s
	}
	return fmt.Sprint(v)
}

func get(u string) (int, error) {
	resp, err := httpc.Get(u)
	if err != nil {
		return 0, err
	}
	io.Copy(io.Discard, resp.Body)
	resp.Body.Close()
	return resp.StatusCode, nil
}

// ---- rows ----

type key struct {
	w  int64
	k  int64
	ts int64
}

func (r key) item(ch string) string { return fmt.Sprintf("%s%d.%d", ch, r.w, r.k) }
func (r key) sysID() string         { return fmt.Sprintf("%d.%d", r.w, r.k) }

// valueOf is the value the client writes for a row
func valueOf(r key) int64 {
	h := uint64(r.w)*1000003 + uint64(r.k)*7919 + uint64(r.ts)
	h ^= h >> 17
	h *= 0x9e3779b97f4a7c15
	h ^= h >> 29
	return int64(h % 1000000000)
}

type ackTok struct {
	ch   string
	w    int64
	t0   int64
	up   int64
	boot string
}

func acked() []ackTok {
	var out []ackTok
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, "~")
		if len(f) != 5 || (f[0] != "r" && f[0] != "l") {
			continue
		}
		w, e1 := strconv.ParseInt(f[1], 10, 64)
		t0, e2 := strconv.ParseInt(f[2], 10, 64)
		up, e3 := strconv.ParseInt(f[3], 10, 64)
		if e1 != nil || e2 != nil || e3 != nil {
			continue
		}
		out = append(out, ackTok{f[0], w, t0, up, f[4]})
	}
	return out
}

func (a ackTok) rows() []key {
	var out []key
	for k := int64(0); k < nKeys; k++ {
		out = append(out, key{a.w, k, a.t0})
	}
	return out
}

// selectAll reads every row of a table: key -> (value, how many times)
type got struct {
	v int64
	n int
}

func selectAll(fe, table string) (map[key]*got, error) {
	rows, _, err := sql(fe, fmt.Sprintf("SELECT w, k, CAST(ts AS BIGINT), v FROM %s", table))
	if err != nil {
		return nil, err
	}
	out := map[key]*got{}
	for _, r := range rows {
		if len(r) != 4 {
			return nil, fmt.Errorf("a row of %d columns", len(r))
		}
		k := key{num(r[0]), num(r[1]), num(r[2])}
		if g, ok := out[k]; ok {
			g.n++
		} else {
			out[k] = &got{num(r[3]), 1}
		}
	}
	return out, nil
}

func feFor(id int64) string { return frontends[int(id)%len(frontends)] }

// anyFrontend returns the first frontend that answers a query
func anyFrontend(start int) (string, error) {
	var last error
	for i := range frontends {
		fe := frontends[(start+i)%len(frontends)]
		if _, _, err := sql(fe, "SELECT 1"); err == nil {
			return fe, nil
		} else {
			last = err
		}
	}
	return "", last
}

// ---- the cluster ----

const ddlRows = `CREATE TABLE IF NOT EXISTS %s (
  w BIGINT, k BIGINT, ts TIMESTAMP(3) TIME INDEX, v BIGINT, PRIMARY KEY (w, k)
) PARTITION ON COLUMNS (k) (k < 1, k >= 1 AND k < 2, k >= 2)%s`

func settle(limit int64) {
	end := uptimeMs() + limit*1000
	step := ""
	for uptimeMs() < end {
		step = "datanodes"
		rows, _, err := sql(frontends[0], "SELECT peer_id FROM information_schema.cluster_info WHERE peer_type = 'DATANODE'")
		if err != nil || len(rows) < len(datanodes) {
			time.Sleep(time.Second)
			continue
		}
		step = "frontends"
		ok := true
		for _, fe := range frontends {
			if _, _, err := sql(fe, "SELECT 1"); err != nil {
				ok = false
			}
		}
		if !ok {
			time.Sleep(time.Second)
			continue
		}
		step = "tables"
		if _, _, err := sql(frontends[0], fmt.Sprintf(ddlRows, tRows, "")); err != nil {
			fmt.Println("settle: create", tRows, err)
			time.Sleep(time.Second)
			continue
		}
		if _, _, err := sql(frontends[0], fmt.Sprintf(ddlRows, tLog, " WITH ('append_mode' = 'true')")); err != nil {
			fmt.Println("settle: create", tLog, err)
			time.Sleep(time.Second)
			continue
		}
		step = "regions"
		for _, fe := range frontends {
			for _, t := range tables {
				if _, err := selectAll(fe, t.name); err != nil {
					ok = false
				}
			}
		}
		if !ok {
			time.Sleep(time.Second)
			continue
		}
		peers, _ := regionPeers(frontends[0])
		fmt.Printf("settle: %d datanodes, %d frontends, both tables answer; regions: %s\n", len(rows), len(frontends), peers)
		return
	}
	fmt.Printf("settle: the cluster is not ready after %d s (waiting for the %s)\n", limit, step)
	os.Exit(1)
}

type region struct {
	table  string
	id     int64
	peer   int64
	leader bool
	status string
}

func regions(fe string) ([]region, error) {
	rows, _, err := sql(fe, "SELECT table_name, region_id, peer_id, is_leader, status FROM information_schema.region_peers WHERE table_name IN ('hydo_rows', 'hydo_log') ORDER BY region_id, peer_id")
	if err != nil {
		return nil, err
	}
	var out []region
	for _, r := range rows {
		if len(r) != 5 {
			continue
		}
		out = append(out, region{str(r[0]), num(r[1]), num(r[2]), str(r[3]) == "Yes" || str(r[3]) == "true", str(r[4])})
	}
	return out, nil
}

func regionPeers(fe string) (string, error) {
	rs, err := regions(fe)
	if err != nil {
		return "", err
	}
	var b []string
	for _, r := range rs {
		l := "f"
		if r.leader {
			l = "L"
		}
		b = append(b, fmt.Sprintf("%d@%d%s/%s", r.id, r.peer, l, r.status))
	}
	return strings.Join(b, " "), nil
}

// ---- operations ----

func write() {
	id := cmdID()
	fe := feFor(id)
	t0 := time.Now().UnixMilli()
	for _, t := range tables {
		var vals, items []string
		for k := int64(0); k < nKeys; k++ {
			r := key{id, k, t0}
			vals = append(vals, fmt.Sprintf("(%d, %d, %d, %d)", r.w, r.k, r.ts, valueOf(r)))
			items = append(items, r.item(t.ch))
		}
		stmt := fmt.Sprintf("INSERT INTO %s (w, k, ts, v) VALUES %s", t.name, strings.Join(vals, ", "))
		event("gt.write", "an insert is sent to a frontend")
		rec(id, "invoke", append([]string{"send", t.ch}, items...)...)
		_, n, err := sql(fe, stmt)
		switch {
		case err == nil && n != nil && *n == nKeys:
			rec(id, "ok", append([]string{"send", t.ch}, items...)...)
			appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("%s~%d~%d~%d~%s", t.ch, id, t0, uptimeMs(), bootID()))
			appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d%s ack", id, t.ch))
			fmt.Printf("write %d: %s took %d rows at %d through %s\n", id, t.name, nKeys, t0, fe)
		default:
			// no answer, a status that is not 200, or another row count: the rows may or may not
			// be stored (the frontend may have written some regions)
			rec(id, "info", append([]string{"send", t.ch}, items...)...)
			appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d%s unknown", id, t.ch))
			if err == nil {
				err = fmt.Errorf("affected rows %v", n)
			}
			fmt.Printf("write %d: %s through %s: %v\n", id, t.name, fe, err)
		}
	}
}

func judge(toks []ackTok, have map[key]*got) (lost, twice, wrong []key) {
	for _, a := range toks {
		for _, r := range a.rows() {
			g, ok := have[r]
			switch {
			case !ok:
				lost = append(lost, r)
			case g.v != valueOf(r):
				wrong = append(wrong, r)
			case g.n > 1:
				twice = append(twice, r)
			}
		}
	}
	return
}

func names(ks []key, max int) string {
	var b []string
	for i, r := range ks {
		if i == max {
			b = append(b, fmt.Sprintf("and %d more", len(ks)-max))
			break
		}
		b = append(b, r.sysID())
	}
	return strings.Join(b, " ")
}

// read: during the faults, read both tables through one frontend. An answer must hold every row
// acknowledged before the read started, once and with its value. A read that fails is no
// violation: a region may be moving.
func read() {
	id := cmdID()
	start, boot := uptimeMs(), bootID()
	fe := feFor(id)
	bad := 0
	for _, t := range tables {
		var toks []ackTok
		for _, a := range acked() {
			if a.ch == t.ch && a.boot == boot && a.up <= start-readMs {
				toks = append(toks, a)
			}
		}
		if len(toks) == 0 {
			continue
		}
		have, err := selectAll(fe, t.name)
		if err != nil {
			fmt.Printf("read %s through %s: %v\n", t.name, fe, err)
			continue
		}
		lost, twice, wrong := judge(toks, have)
		if len(lost)+len(twice)+len(wrong) == 0 {
			fmt.Printf("read %s: %d acknowledged writes, every row returned once\n", t.name, len(toks))
			continue
		}
		peers, _ := regionPeers(fe)
		fmt.Printf("%s through %s: lost: %s\nreturned twice: %s\nwrong value: %s\nregions: %s\n", t.name, fe, names(lost, 20), names(twice, 20), names(wrong, 20), peers)
		bad += len(lost) + len(twice) + len(wrong)
	}
	if bad > 0 {
		fmt.Printf("VIOLATION: %d acknowledged rows are not returned as written by a read that succeeded\n", bad)
		os.Exit(100)
	}
}

// admin: one ADMIN function, drawn from the command's seed: a flush or a compaction of one table,
// or the migration of one region of hydo_rows or hydo_log to the other datanode
func admin() {
	id := cmdID()
	r := rng()
	fe := feFor(id)
	t := tables[r.IntN(len(tables))].name
	var stmt string
	switch r.IntN(3) {
	case 0:
		stmt = fmt.Sprintf("ADMIN flush_table('%s')", t)
	case 1:
		stmt = fmt.Sprintf("ADMIN compact_table('%s')", t)
	default:
		rs, err := regions(fe)
		if err != nil {
			fmt.Println("admin: region_peers:", err)
			os.Exit(1)
		}
		var leaders []region
		for _, x := range rs {
			if x.leader {
				leaders = append(leaders, x)
			}
		}
		if len(leaders) == 0 {
			fmt.Println("admin: no leader in region_peers")
			os.Exit(1)
		}
		x := leaders[r.IntN(len(leaders))]
		to := int64(1) - x.peer
		stmt = fmt.Sprintf("ADMIN migrate_region(%d, %d, %d, 30)", x.id, x.peer, to)
	}
	event("gt.admin", "an ADMIN function is sent")
	rows, _, err := sql(fe, stmt)
	if err != nil {
		fmt.Printf("admin %d: %s: %v\n", id, stmt, err)
		os.Exit(1)
	}
	fmt.Printf("admin %d: %s: %v\n", id, stmt, rows)
}

type ddlState struct {
	created map[string]bool     // acknowledged CREATE
	dropped map[string]bool     // acknowledged DROP
	columns map[string][]string // acknowledged ADD COLUMN
	maybe   map[string]bool     // a DROP without an answer: the table may be gone
}

func ddlAcked() ddlState {
	s := ddlState{map[string]bool{}, map[string]bool{}, map[string][]string{}, map[string]bool{}}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, "~")
		if len(f) >= 3 && f[0] == "u" && f[1] == "drop" {
			s.maybe[f[2]] = true
			continue
		}
		if len(f) < 3 || f[0] != "d" {
			continue
		}
		switch f[1] {
		case "create":
			s.created[f[2]] = true
		case "drop":
			s.dropped[f[2]] = true
		case "alter":
			if len(f) == 4 {
				s.columns[f[2]] = append(s.columns[f[2]], f[3])
			}
		}
	}
	return s
}

func (s ddlState) live() []string {
	var out []string
	for t := range s.created {
		if !s.dropped[t] {
			out = append(out, t)
		}
	}
	sort.Strings(out)
	return out
}

// ddl: one CREATE, ALTER ... ADD COLUMN or DROP statement. A new table d<entry> has two
// partitions, so its regions can be on both datanodes.
func ddl() {
	id := cmdID()
	r := rng()
	fe := feFor(id)
	s := ddlAcked()
	live := s.live()
	var stmt, tok string
	switch op := r.IntN(4); {
	case op <= 1 || len(live) == 0:
		t := fmt.Sprintf("d%d", id)
		stmt = fmt.Sprintf("CREATE TABLE %s (h STRING, ts TIMESTAMP(3) TIME INDEX, x BIGINT, PRIMARY KEY (h)) PARTITION ON COLUMNS (h) (h < 'm', h >= 'm')", t)
		tok = "d~create~" + t
	case op == 2:
		t := live[r.IntN(len(live))]
		c := fmt.Sprintf("c%d", id)
		stmt = fmt.Sprintf("ALTER TABLE %s ADD COLUMN %s BIGINT", t, c)
		tok = "d~alter~" + t + "~" + c
	default:
		t := live[r.IntN(len(live))]
		stmt = fmt.Sprintf("DROP TABLE %s", t)
		tok = "d~drop~" + t
	}
	event("gt.ddl", "a DDL statement is sent")
	_, _, err := sql(fe, stmt)
	if err != nil {
		// no answer or an error: the statement may still take effect (a procedure of the metasrv
		// goes on after the frontend gave up), so the check allows both outcomes
		appendLine(os.Getenv("HYDO_ACKED"), "u"+strings.TrimPrefix(tok, "d"))
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%dd unknown", id))
		fmt.Printf("ddl %d: %s: %v\n", id, stmt, err)
		os.Exit(1)
	}
	appendLine(os.Getenv("HYDO_ACKED"), tok)
	appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%dd ack", id))
	fmt.Printf("ddl %d: %s: done\n", id, stmt)
}

// drain: the final read of the messages check. It waits up to 150 s until a frontend reads both
// tables, then reads until two answers 3 s apart agree, and records every row it read.
func drain() {
	const proc = 900000
	start := uptimeMs()
	end := start + 200000
	var last map[string]map[key]*got
	same := 0
	for uptimeMs() < end {
		fe, err := anyFrontend(0)
		if err != nil {
			fmt.Println("drain: no frontend answers:", err)
			time.Sleep(2 * time.Second)
			continue
		}
		cur := map[string]map[key]*got{}
		ok := true
		for _, t := range tables {
			rec(proc, "invoke", "recv", t.ch, "-")
			have, err := selectAll(fe, t.name)
			if err != nil {
				rec(proc, "info", "recv", t.ch, "-")
				fmt.Printf("drain: %s through %s: %v\n", t.name, fe, err)
				ok = false
				break
			}
			rec(proc, "ok", "recv", t.ch, "-")
			cur[t.name] = have
		}
		if !ok {
			same = 0
			time.Sleep(2 * time.Second)
			continue
		}
		if last != nil && len(cur[tRows]) == len(last[tRows]) && len(cur[tLog]) == len(last[tLog]) {
			same++
		} else {
			same = 0
		}
		last = cur
		if same < 1 {
			time.Sleep(3 * time.Second)
			continue
		}
		for _, t := range tables {
			have := cur[t.name]
			keys := make([]key, 0, len(have))
			for r := range have {
				keys = append(keys, r)
			}
			sort.Slice(keys, func(i, j int) bool {
				a, b := keys[i], keys[j]
				if a.w != b.w {
					return a.w < b.w
				}
				if a.k != b.k {
					return a.k < b.k
				}
				return a.ts < b.ts
			})
			var its []string
			for _, r := range keys {
				g := have[r]
				if g.v != valueOf(r) {
					its = append(its, fmt.Sprintf("x%s;o=%s", r.item(t.ch), r.sysID()))
					fmt.Printf("drain: %s %s has the value %d, not %d\n", t.name, r.sysID(), g.v, valueOf(r))
					continue
				}
				its = append(its, r.item(t.ch)+";o="+r.sysID())
				for n := 2; n <= g.n; n++ {
					its = append(its, fmt.Sprintf("%s;o=%s#%d", r.item(t.ch), r.sysID(), n))
					fmt.Printf("drain: %s %s returned %d times\n", t.name, r.sysID(), g.n)
				}
			}
			rec(proc, "invoke", "recv", t.ch, "-")
			for len(its) > 0 {
				n := len(its)
				if n > 40 {
					n = 40
				}
				rec(proc, "ok", append([]string{"recv", t.ch, "-"}, its[:n]...)...)
				its = its[n:]
				if len(its) > 0 {
					rec(proc, "invoke", "recv", t.ch, "-")
				}
			}
			rec(proc, "ok", "drained", t.ch, "-")
			fmt.Printf("drain: %s: %d rows read back\n", t.name, len(have))
		}
		return
	}
	fmt.Println("drain: no stable answer of a frontend within 200 s")
	os.Exit(1)
}

// checkQuery: count(*) and sum(v) of each table, from each frontend, against the acknowledged
// rows and the rows a full read returns. Exit 2 when no frontend answers.
func checkQuery() {
	fe, err := anyFrontend(0)
	if err != nil {
		fmt.Println("no frontend answers:", err)
		os.Exit(2)
	}
	var bad []string
	for _, t := range tables {
		have, err := selectAll(fe, t.name)
		if err != nil {
			fmt.Println("read:", err)
			os.Exit(2)
		}
		var n, sum int64
		for r, g := range have {
			n += int64(g.n)
			sum += int64(g.n) * g.v
			_ = r
		}
		rows, _, err := sql(fe, fmt.Sprintf("SELECT count(*), sum(v) FROM %s", t.name))
		if err != nil || len(rows) != 1 || len(rows[0]) != 2 {
			fmt.Println("aggregate:", err)
			os.Exit(2)
		}
		c, s := num(rows[0][0]), num(rows[0][1])
		if c != n || (n > 0 && s != sum) {
			bad = append(bad, fmt.Sprintf("%s: count(*) %d sum %d, a full read %d rows sum %d", t.name, c, s, n, sum))
		}
		// each region alone, by its partition column
		for k := int64(0); k < nKeys; k++ {
			rows, _, err := sql(fe, fmt.Sprintf("SELECT count(*) FROM %s WHERE k = %d", t.name, k))
			if err != nil || len(rows) != 1 {
				fmt.Println("count of a region:", err)
				os.Exit(2)
			}
			var want int64
			for r, g := range have {
				if r.k == k {
					want += int64(g.n)
				}
			}
			if got := num(rows[0][0]); got != want {
				bad = append(bad, fmt.Sprintf("%s k=%d: count(*) %d, a full read %d", t.name, k, got, want))
			}
		}
	}
	if len(bad) == 0 {
		fmt.Println("each table's count(*) and sum(v) agree with a full read, also per region")
		return
	}
	for _, b := range bad {
		fmt.Println(b)
	}
	fmt.Printf("%d aggregates disagree with a full read of the same table\n", len(bad))
	os.Exit(1)
}

// checkDDL: every acknowledged CREATE that no DROP followed (acknowledged or without an answer) is
// a table that answers,
// with every acknowledged column; every acknowledged DROP is gone; and every table d<n> that
// exists, also one whose statement had no answer, answers a query on all its columns.
func checkDDL() {
	fe, err := anyFrontend(0)
	if err != nil {
		fmt.Println("no frontend answers:", err)
		os.Exit(2)
	}
	s := ddlAcked()
	rows, _, err := sql(fe, "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE 'd%'")
	if err != nil {
		fmt.Println("information_schema.tables:", err)
		os.Exit(2)
	}
	exists := map[string]bool{}
	for _, r := range rows {
		exists[str(r[0])] = true
	}
	var bad []string
	for _, t := range s.live() {
		if !exists[t] && !s.maybe[t] {
			bad = append(bad, fmt.Sprintf("%s: created (acknowledged) and not dropped, but missing", t))
		}
	}
	for t := range s.dropped {
		if exists[t] {
			bad = append(bad, fmt.Sprintf("%s: dropped (acknowledged), but still listed", t))
		}
	}
	names := make([]string, 0, len(exists))
	for t := range exists {
		names = append(names, t)
	}
	sort.Strings(names)
	for _, t := range names {
		cols, _, err := sql(fe, fmt.Sprintf("SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '%s'", t))
		if err != nil {
			bad = append(bad, fmt.Sprintf("%s: its columns cannot be read: %v", t, err))
			continue
		}
		have := map[string]bool{}
		var list []string
		for _, c := range cols {
			have[str(c[0])] = true
			list = append(list, str(c[0]))
		}
		for _, c := range s.columns[t] {
			if !have[c] {
				bad = append(bad, fmt.Sprintf("%s: column %s added (acknowledged), but missing", t, c))
			}
		}
		sort.Strings(list)
		if _, _, err := sql(fe, fmt.Sprintf("SELECT %s FROM %s", strings.Join(list, ", "), t)); err != nil {
			bad = append(bad, fmt.Sprintf("%s: listed with the columns %s, but a query fails: %v", t, strings.Join(list, ","), err))
		}
	}
	if len(bad) == 0 {
		fmt.Printf("%d tables d<n> exist and answer; every acknowledged DDL statement took effect\n", len(exists))
		return
	}
	for i, b := range bad {
		if i == 20 {
			fmt.Printf("and %d more\n", len(bad)-20)
			break
		}
		fmt.Println(b)
	}
	fmt.Printf("%d tables differ from the acknowledged DDL statements or do not answer\n", len(bad))
	os.Exit(1)
}

// checkLeaders: every region of the two tables has exactly one leader, on a datanode that the
// metasrv lists as alive
func checkLeaders() {
	fe, err := anyFrontend(0)
	if err != nil {
		fmt.Println("no frontend answers:", err)
		os.Exit(2)
	}
	rs, err := regions(fe)
	if err != nil {
		fmt.Println("region_peers:", err)
		os.Exit(2)
	}
	leaders := map[int64]int{}
	for _, r := range rs {
		if r.leader {
			leaders[r.id]++
		} else if _, ok := leaders[r.id]; !ok {
			leaders[r.id] = 0
		}
	}
	var bad []string
	for id, n := range leaders {
		if n != 1 {
			bad = append(bad, fmt.Sprintf("region %d has %d leaders", id, n))
		}
	}
	if len(leaders) != 2*nKeys {
		bad = append(bad, fmt.Sprintf("%d regions listed, not %d", len(leaders), 2*nKeys))
	}
	peers, _ := regionPeers(fe)
	if len(bad) == 0 {
		fmt.Printf("every region has one leader: %s\n", peers)
		return
	}
	fmt.Println(strings.Join(bad, "\n"))
	fmt.Println("regions:", peers)
	fmt.Printf("%d regions without exactly one leader\n", len(bad))
	os.Exit(1)
}

func dump() {
	fe, err := anyFrontend(0)
	if err != nil {
		fmt.Println(err)
		os.Exit(2)
	}
	for _, t := range tables {
		have, err := selectAll(fe, t.name)
		if err != nil {
			fmt.Println(t.name, err)
			continue
		}
		keys := make([]key, 0, len(have))
		for r := range have {
			keys = append(keys, r)
		}
		sort.Slice(keys, func(i, j int) bool { return keys[i].sysID() < keys[j].sysID() })
		for _, r := range keys {
			fmt.Printf("%s %s ts=%d v=%d x%d (want %d)\n", t.name, r.sysID(), r.ts, have[r].v, have[r].n, valueOf(r))
		}
		fmt.Printf("%s: %d rows\n", t.name, len(keys))
	}
	peers, _ := regionPeers(fe)
	fmt.Println("regions:", peers)
	for _, d := range datanodes {
		c, err := get(d + "/health")
		fmt.Println(d, c, err)
	}
}

func main() {
	if p := env("HYDO_HIST_OUT", "/dev/console"); p != "off" {
		if f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644); err == nil {
			histOut = f
		}
	}
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: gtc settle|write|read|admin|ddl|drain|check query|check ddl|check leaders|dump")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "settle":
		s := int64(150)
		if len(os.Args) > 2 {
			s, _ = strconv.ParseInt(os.Args[2], 10, 64)
		}
		settle(s)
	case "write":
		write()
	case "read":
		read()
	case "admin":
		admin()
	case "ddl":
		ddl()
	case "drain":
		drain()
	case "check":
		what := ""
		if len(os.Args) > 2 {
			what = os.Args[2]
		}
		switch what {
		case "query":
			checkQuery()
		case "ddl":
			checkDDL()
		case "leaders":
			checkLeaders()
		default:
			fmt.Fprintln(os.Stderr, "usage: gtc check query|ddl|leaders")
			os.Exit(2)
		}
	case "dump":
		dump()
	default:
		fmt.Fprintln(os.Stderr, "unknown command", os.Args[1])
		os.Exit(2)
	}
}
