// vmc is the test client of the VictoriaMetrics example: the workload's operations, the wait for
// the cluster, the drain of the messages check and one query check. It speaks HTTP to vminsert
// and vmselect and uses only Go's standard library.
//
//	vmc settle [s]     wait until vminsert reaches both vmstorage nodes and vmselect answers
//	vmc write          one import of 2 new series with 3 samples each (the composer's HYDO_*)
//	vmc read           read back the samples acknowledged 30 s ago or earlier; exit 100 when one
//	                   is missing, returned twice or has another value, while vmselect answered
//	vmc drain          the messages check's final read: every sample through /api/v1/export
//	vmc check query    every acknowledged series counts 3 samples and the right sum in a query
//	vmc dump           print every sample of the test, one per line (for a look by hand)
//
// A write's series are hydo_s{w="<entry>",k="<0..1>",t="<t0>"}, each with 3 samples at the
// client's wall clock t0 .. t0+2 ms. A sample's value is a function of its series and its timestamp
// (valueOf), so every reader can tell a wrong value from a right one. Each series belongs to one
// composer entry, so no other write adds samples to it.
//
// Records: the history `vm` of the messages check (hydo guide messages), one record before the
// request and one after the reply, written to /dev/console; and, for an acknowledged write, the
// token w~<entry>~<t0>~<uptime ms>~<boot> in $HYDO_ACKED and its outcome in $HYDO_OUTCOMES.
// Deadlines use /proc/uptime, which a clock step (T=) does not move.
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

const (
	hist      = "vm"
	channel   = "s"
	nSeries   = 2
	nSamples  = 3
	metric    = "hydo_s"
	readAgeMs = 30000 // the read judges samples acknowledged this long ago or earlier
)

var (
	vminsert = env("VMC_INSERT", "http://vminsert:8480")
	vmselect = env("VMC_SELECT", "http://vmselect:8481")
	storages = strings.Split(env("VMC_STORAGES", "http://vmstorage1:8482,http://vmstorage2:8482"), ",")
	histOut  *os.File
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

// rec writes one record: HYDO-HIST vm <process> <type> <uptime ns> <f> <args>
func rec(proc int64, typ string, args ...string) {
	if histOut == nil {
		return
	}
	line := fmt.Sprintf("HYDO-HIST %s %d %s %d %s\n", hist, proc, typ, uptimeMs()*1000000, strings.Join(args, " "))
	histOut.WriteString(line) // one write: one record
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

// ---- samples ----

// a sample's key: the write's entry, the series k, the timestamp in ms, and the write's first
// timestamp t0 (the series' label t)
type key struct {
	w  int64
	k  int
	ts int64
	t0 int64
}

// j numbers the samples of a write: k*nSamples + (ts - t0). The history names a sample
// m<entry>.<j>, which keeps the records short: hvx's console holds 64 KiB.
func (s key) j() int64      { return int64(s.k)*nSamples + s.ts - s.t0 }
func (s key) item() string  { return fmt.Sprintf("m%d.%d", s.w, s.j()) }
func (s key) sysID() string { return fmt.Sprintf("%d.%d", s.w, s.j()) }

// valueOf is the value the client writes for a sample: an integer below 1e9, which
// VictoriaMetrics stores exactly.
func valueOf(s key) float64 {
	h := uint64(s.w)*1000003 + uint64(s.k)*7919 + uint64(s.ts)
	h ^= h >> 17
	h *= 0x9e3779b97f4a7c15
	h ^= h >> 29
	return float64(h % 1000000000)
}

type ackTok struct {
	w    int64
	t0   int64
	up   int64
	boot string
}

func acked() []ackTok {
	var out []ackTok
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, "~")
		if len(f) != 5 || f[0] != "w" {
			continue
		}
		w, e1 := strconv.ParseInt(f[1], 10, 64)
		t0, e2 := strconv.ParseInt(f[2], 10, 64)
		up, e3 := strconv.ParseInt(f[3], 10, 64)
		if e1 != nil || e2 != nil || e3 != nil {
			continue
		}
		out = append(out, ackTok{w, t0, up, f[4]})
	}
	return out
}

func (a ackTok) samples() []key {
	var out []key
	for k := 0; k < nSeries; k++ {
		for i := 0; i < nSamples; i++ {
			out = append(out, key{a.w, k, a.t0 + int64(i), a.t0})
		}
	}
	return out
}

// ---- HTTP ----

var httpc = &http.Client{Timeout: 15 * time.Second}

func get(u string) (int, []byte, error) {
	resp, err := httpc.Get(u)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	return resp.StatusCode, b, err
}

type exported struct {
	Metric     map[string]string `json:"metric"`
	Values     []float64         `json:"values"`
	Timestamps []int64           `json:"timestamps"`
}

// a returned sample: its key, its value, and how many times the answer had it
type got struct {
	v float64
	n int
}

// export reads every sample of the test through vmselect's /api/v1/export
func export() (map[key]*got, error) {
	q := url.Values{}
	q.Set("match[]", `{__name__="`+metric+`"}`)
	q.Set("start", "0")
	q.Set("end", "4102444800") // 2100-01-01: after any clock step of the tests
	code, body, err := get(vmselect + "/select/0/prometheus/api/v1/export?" + q.Encode())
	if err != nil {
		return nil, err
	}
	if code != 200 {
		return nil, fmt.Errorf("export: status %d: %.200s", code, body)
	}
	out := map[key]*got{}
	sc := bufio.NewScanner(bytes.NewReader(body))
	sc.Buffer(make([]byte, 1<<20), 64<<20)
	for sc.Scan() {
		line := sc.Bytes()
		if len(bytes.TrimSpace(line)) == 0 {
			continue
		}
		var e exported
		if err := json.Unmarshal(line, &e); err != nil {
			return nil, fmt.Errorf("export: %v: %.200s", err, line)
		}
		w, e1 := strconv.ParseInt(e.Metric["w"], 10, 64)
		k, e2 := strconv.Atoi(e.Metric["k"])
		t0, e3 := strconv.ParseInt(e.Metric["t"], 10, 64)
		if e1 != nil || e2 != nil || e3 != nil || len(e.Values) != len(e.Timestamps) {
			return nil, fmt.Errorf("export: a series that no write made: %v", e.Metric)
		}
		for i, ts := range e.Timestamps {
			s := key{w, k, ts, t0}
			if g, ok := out[s]; ok {
				g.n++
			} else {
				out[s] = &got{e.Values[i], 1}
			}
		}
	}
	return out, sc.Err()
}

func healthy(base string) bool {
	code, _, err := get(base + "/health")
	return err == nil && code == 200
}

// reachableAny reads vminsert's view of the vmstorage nodes: how many it can send to now
func reachableAny() int {
	code, body, err := get(vminsert + "/metrics")
	if err != nil || code != 200 {
		return -1
	}
	seen := map[string]bool{}
	for _, l := range strings.Split(string(body), "\n") {
		if strings.HasPrefix(l, "vm_rpc_vmstorage_is_reachable{") && strings.HasSuffix(l, " 1") {
			if i := strings.Index(l, `addr="`); i >= 0 {
				a := l[i+6:]
				seen[a[:strings.IndexByte(a, '"')]] = true
			}
		}
	}
	return len(seen)
}

// ---- operations ----

func settle(limit int64) {
	end := uptimeMs() + limit*1000
	for uptimeMs() < end {
		if reachableAny() == len(storages) && healthy(vmselect) {
			if _, err := export(); err == nil {
				fmt.Printf("settle: vminsert reaches %d vmstorage nodes, vmselect answers\n", len(storages))
				return
			}
		}
		time.Sleep(time.Second)
	}
	fmt.Printf("settle: the cluster is not ready after %d s (vminsert reaches %d nodes)\n", limit, reachableAny())
	os.Exit(1)
}

func write() {
	id, err := strconv.ParseInt(os.Getenv("HYDO_CMD_ID"), 10, 64)
	if err != nil {
		fmt.Println("write: HYDO_CMD_ID is not a number")
		os.Exit(2)
	}
	t0 := time.Now().UnixMilli()
	var body bytes.Buffer
	var items []string
	for k := 0; k < nSeries; k++ {
		e := exported{Metric: map[string]string{"__name__": metric, "w": strconv.FormatInt(id, 10), "k": strconv.Itoa(k), "t": strconv.FormatInt(t0, 10)}}
		for i := 0; i < nSamples; i++ {
			s := key{id, k, t0 + int64(i), t0}
			e.Values = append(e.Values, valueOf(s))
			e.Timestamps = append(e.Timestamps, s.ts)
			items = append(items, s.item())
		}
		b, _ := json.Marshal(e)
		body.Write(b)
		body.WriteByte('\n')
	}
	rec(id, "invoke", append([]string{"send", channel}, items...)...)
	req, _ := http.NewRequest("POST", vminsert+"/insert/0/prometheus/api/v1/import", &body)
	resp, err := httpc.Do(req)
	switch {
	case err != nil:
		rec(id, "info", append([]string{"send", channel}, items...)...)
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d unknown", id))
		fmt.Printf("write %d: no answer: %v\n", id, err)
	case resp.StatusCode/100 == 2:
		resp.Body.Close()
		rec(id, "ok", append([]string{"send", channel}, items...)...)
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("w~%d~%d~%d~%s", id, t0, uptimeMs(), bootID()))
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d ack", id))
		fmt.Printf("write %d: %d samples acknowledged (status %d), t0 %d\n", id, len(items), resp.StatusCode, t0)
	default:
		b, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		// vminsert answers 4xx for a request it refused to parse, and 5xx when it could not
		// take the rows; a 5xx may still have taken some of them
		typ, out := "info", "unknown"
		if resp.StatusCode/100 == 4 {
			typ, out = "fail", "failed"
		}
		rec(id, typ, append([]string{"send", channel}, items...)...)
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d %s", id, out))
		fmt.Printf("write %d: status %d: %.200s\n", id, resp.StatusCode, b)
	}
}

// judge compares the samples of the acknowledged writes with what vmselect returned
func judge(toks []ackTok, have map[key]*got) (lost, twice, wrong []key) {
	for _, a := range toks {
		for _, s := range a.samples() {
			g, ok := have[s]
			switch {
			case !ok:
				lost = append(lost, s)
			case g.v != valueOf(s):
				wrong = append(wrong, s)
			case g.n > 1:
				twice = append(twice, s)
			}
		}
	}
	return
}

func names(ks []key, max int) string {
	var b []string
	for i, s := range ks {
		if i == max {
			b = append(b, fmt.Sprintf("and %d more", len(ks)-max))
			break
		}
		b = append(b, s.sysID())
	}
	return strings.Join(b, " ")
}

func read() {
	now, boot := uptimeMs(), bootID()
	var toks []ackTok
	for _, a := range acked() {
		if a.boot == boot && a.up <= now-readAgeMs {
			toks = append(toks, a)
		}
	}
	if len(toks) == 0 {
		fmt.Println("read: no acknowledged write is old enough to judge")
		return
	}
	up := 0
	for _, s := range storages {
		if healthy(s) {
			up++
		}
	}
	have, err := export()
	if err != nil {
		fmt.Printf("read: vmselect did not answer (%d of %d vmstorage nodes healthy): %v\n", up, len(storages), err)
		return
	}
	lost, twice, wrong := judge(toks, have)
	if len(lost)+len(twice)+len(wrong) == 0 {
		fmt.Printf("read: %d writes acknowledged 30 s ago or earlier, every sample returned once (%d of %d vmstorage nodes healthy)\n", len(toks), up, len(storages))
		return
	}
	fmt.Printf("lost: %s\nreturned twice: %s\nwrong value: %s\n", names(lost, 20), names(twice, 20), names(wrong, 20))
	fmt.Printf("%d missing, %d returned twice, %d with another value; %d of %d vmstorage nodes healthy\n",
		len(lost), len(twice), len(wrong), up, len(storages))
	// the last line is the finding's signature once its numbers are blanked: keep it short
	fmt.Printf("VIOLATION: %d of the samples acknowledged 30 s ago are not returned as written\n", len(lost)+len(twice)+len(wrong))
	os.Exit(100)
}

// drain: the final read of the messages check. It waits up to 120 s for both vmstorage nodes,
// then until 15 s after the last acknowledged write, asks each node to make its recent samples
// searchable (/internal/force_flush), and reads until three exports 3 s apart agree. It gives up after 220 s of uptime from its start (below
// workload.timeout).
func drain() {
	const proc = 900000
	start := uptimeMs()
	end := start + 220000
	for uptimeMs() < start+120000 {
		up := 0
		for _, s := range storages {
			if healthy(s) {
				up++
			}
		}
		if up == len(storages) && healthy(vminsert) && healthy(vmselect) {
			break
		}
		time.Sleep(time.Second)
	}
	// vmstorage caches the series that a tag filter matches, and resets that cache only every
	// 10 s after new series became searchable (lib/mergeset, defaultFlushCallbackInterval), also
	// after /internal/force_flush. So the drain reads no earlier than 15 s after the last
	// acknowledged write.
	boot, lastAck := bootID(), int64(0)
	for _, a := range acked() {
		if a.boot == boot && a.up > lastAck {
			lastAck = a.up
		}
	}
	for uptimeMs() < lastAck+15000 && uptimeMs() < end {
		time.Sleep(time.Second)
	}
	var last map[key]*got
	same := 0
	for uptimeMs() < end {
		for _, s := range storages {
			get(s + "/internal/force_flush")
		}
		rec(proc, "invoke", "recv", channel, "-")
		have, err := export()
		if err != nil {
			rec(proc, "info", "recv", channel, "-")
			fmt.Println("drain:", err)
			same = 0
			time.Sleep(2 * time.Second)
			continue
		}
		rec(proc, "ok", "recv", channel, "-")
		if last != nil && len(have) == len(last) {
			same++
		} else {
			same = 0
		}
		last = have
		if same < 2 {
			time.Sleep(3 * time.Second)
			continue
		}
		// the deliveries, in a fixed order: a wrong value is a message that no write sent
		// (no-phantom), and a sample returned twice is a second stored copy (duplicates)
		keys := make([]key, 0, len(have))
		for s := range have {
			keys = append(keys, s)
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
		for _, s := range keys {
			g := have[s]
			if g.v != valueOf(s) {
				its = append(its, fmt.Sprintf("x%s;o=%s", s.sysID(), s.sysID()))
				fmt.Printf("drain: %s has the value %v, not %v\n", s.sysID(), g.v, valueOf(s))
				continue
			}
			its = append(its, s.item()+";o="+s.sysID())
			for n := 2; n <= g.n; n++ {
				its = append(its, fmt.Sprintf("%s;o=%s#%d", s.item(), s.sysID(), n))
			}
		}
		rec(proc, "invoke", "recv", channel, "-")
		for len(its) > 0 {
			n := len(its)
			if n > 30 {
				n = 30
			}
			rec(proc, "ok", append([]string{"recv", channel, "-"}, its[:n]...)...)
			its = its[n:]
			if len(its) > 0 {
				rec(proc, "invoke", "recv", channel, "-")
			}
		}
		rec(proc, "ok", "drained", channel, "-")
		fmt.Printf("drain: %d samples read back\n", len(have))
		return
	}
	fmt.Println("drain: no stable answer of vmselect within 220 s")
	os.Exit(1)
}

// checkQuery: every acknowledged series has 4 samples and their sum in a MetricsQL query over a
// window around the samples (which uses the per-day index for a window of days). Exit 2 when
// vmselect does not answer.
func checkQuery() {
	toks := acked()
	if len(toks) == 0 {
		fmt.Println("no acknowledged write")
		return
	}
	lo, hi := toks[0].t0, toks[0].t0
	for _, a := range toks {
		if a.t0 < lo {
			lo = a.t0
		}
		if a.t0 > hi {
			hi = a.t0
		}
	}
	at := hi/1000 + 60               // seconds
	win := at - (lo/1000 - 3600) + 1 // seconds back from at, past the first sample
	type res struct {
		Status string `json:"status"`
		Data   struct {
			Result []struct {
				Metric map[string]string `json:"metric"`
				Value  []any             `json:"value"`
			} `json:"result"`
		} `json:"data"`
	}
	ask := func(fn string) (map[[2]int64]float64, error) {
		q := url.Values{}
		q.Set("query", fmt.Sprintf(`%s(%s[%ds])`, fn, metric, win))
		q.Set("time", strconv.FormatInt(at, 10))
		q.Set("latency_offset", "1ms") // vmselect hides the last 30 s of samples by default (documented)
		code, body, err := get(vmselect + "/select/0/prometheus/api/v1/query?" + q.Encode())
		if err != nil {
			return nil, err
		}
		if code != 200 {
			return nil, fmt.Errorf("query: status %d: %.200s", code, body)
		}
		var r res
		if err := json.Unmarshal(body, &r); err != nil {
			return nil, err
		}
		out := map[[2]int64]float64{}
		for _, x := range r.Data.Result {
			w, _ := strconv.ParseInt(x.Metric["w"], 10, 64)
			k, _ := strconv.ParseInt(x.Metric["k"], 10, 64)
			if len(x.Value) == 2 {
				s, _ := x.Value[1].(string)
				v, _ := strconv.ParseFloat(s, 64)
				out[[2]int64{w, k}] = v
			}
		}
		return out, nil
	}
	counts, err := ask("count_over_time")
	if err != nil {
		fmt.Println("vmselect did not answer:", err)
		os.Exit(2)
	}
	sums, err := ask("sum_over_time")
	if err != nil {
		fmt.Println("vmselect did not answer:", err)
		os.Exit(2)
	}
	var bad []string
	for _, a := range toks {
		for k := 0; k < nSeries; k++ {
			want := 0.0
			for i := 0; i < nSamples; i++ {
				want += valueOf(key{a.w, k, a.t0 + int64(i), a.t0})
			}
			c, s := counts[[2]int64{a.w, int64(k)}], sums[[2]int64{a.w, int64(k)}]
			if c != nSamples || s != want {
				bad = append(bad, fmt.Sprintf("%d.%d: count %v sum %v (want %d and %v)", a.w, k, c, s, nSamples, want))
			}
		}
	}
	if len(bad) == 0 {
		fmt.Printf("%d acknowledged series: each counts %d samples with the right sum\n", len(toks)*nSeries, nSamples)
		return
	}
	for i, b := range bad {
		if i == 20 {
			fmt.Printf("and %d more\n", len(bad)-20)
			break
		}
		fmt.Println(b)
	}
	fmt.Printf("%d of %d acknowledged series have another count or sum in a query\n", len(bad), len(toks)*nSeries)
	os.Exit(1)
}

func dump() {
	have, err := export()
	if err != nil {
		fmt.Println(err)
		os.Exit(2)
	}
	keys := make([]key, 0, len(have))
	for s := range have {
		keys = append(keys, s)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i].sysID() < keys[j].sysID() })
	for _, s := range keys {
		fmt.Printf("%s %v x%d (want %v)\n", s.sysID(), have[s].v, have[s].n, valueOf(s))
	}
	fmt.Printf("%d samples; vminsert reaches %d vmstorage nodes\n", len(keys), reachableAny())
}

func main() {
	if p := env("HYDO_HIST_OUT", "/dev/console"); p != "off" {
		if f, err := os.OpenFile(p, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644); err == nil {
			histOut = f
		}
	}
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: vmc settle|write|read|drain|check query|dump")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "settle":
		s := int64(100)
		if len(os.Args) > 2 {
			s, _ = strconv.ParseInt(os.Args[2], 10, 64)
		}
		settle(s)
	case "write":
		write()
	case "read":
		read()
	case "drain":
		drain()
	case "check":
		if len(os.Args) > 2 && os.Args[2] == "query" {
			checkQuery()
			return
		}
		fmt.Fprintln(os.Stderr, "usage: vmc check query")
		os.Exit(2)
	case "dump":
		dump()
	default:
		fmt.Fprintln(os.Stderr, "unknown command", os.Args[1])
		os.Exit(2)
	}
}
