// promc is the test client of the Prometheus example: the exporter that the agent scrapes, the
// workload's remote writes, snapshots and reads, and the checks. It speaks HTTP to the Prometheus
// server and uses only Go's standard library (remote write 1.0 is encoded by hand: protobuf and
// snappy's literal blocks).
//
//	promc serve            the exporter on :9100: every scrape gets a new series hydo_scrape{n="<k>"}
//	promc settle [s]       wait until the server, the agent and the agent's forwarding work
//	promc write            one remote write of 2 new series with 3 samples each (the composer's HYDO_*)
//	promc read             read back the samples acknowledged 30 s ago or earlier; exit 100 when one
//	                       is missing or has another value, while the server answered
//	promc blocks           record the blocks that the server lists (for the power-cut rule)
//	promc snapshot         take a snapshot with the head; exit 100 when it lacks an acknowledged sample
//	promc check samples    every acknowledged sample is queryable with its value (final)
//	promc check snapshots  every snapshot taken still holds what it held (final)
//	promc check scrape     the samples scraped in the last 45 s reach the server (eventually)
//	promc dump             print every sample of the test, one per line (for a look by hand)
//
// A write's series are hydo_rw{w="<entry>",k="0"} with 3 samples at the client's wall clock
// t0 .. t0+2 ms, in order, and hydo_rw{w="<entry>",k="1"} with 3 samples 60 s earlier, which the
// server takes as out-of-order samples (its head accepts in-order samples down to 30 s behind its
// newest one, with blocks of one minute). A sample's value is a function of its series and its
// timestamp (valueOf). Each fourth write is sent twice, as a sender's retry after a lost answer.
//
// Records in $HYDO_ACKED, one token per line:
//
//	w~<entry>~<t0>~<uptime ms>~<boot>   an acknowledged write
//	b~<ulid>~<mint>~<maxt>~<ooo>~<uptime ms>~<boot>   a block the server listed
//	s~<name>~<writes acknowledged before it>~<uptime ms>~<boot>   a snapshot the server took
//
// Deadlines use /proc/uptime, which a clock step (T=) does not move.
package main

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	metric      = "hydo_rw"
	scrapeName  = "hydo_scrape"
	nSeries     = 2
	nSamples    = 3
	oooBackMs   = 60000 // series k=1 lies this far behind the write's wall clock
	readAgeMs   = 30000 // the read judges samples acknowledged this long ago or earlier
	retention   = 24 * 3600 * 1000
	oooWindowMs = 10 * 60 * 1000 // the server's out_of_order_time_window
	servedLog   = "/data/exp/served.log"
	snapRoot    = "/prometheus/tsdb/snapshots" // inside the server's root (hydo-oci)
	snapHost    = "/data/vol/prom/tsdb/snapshots"
)

var (
	server = env("PROMC_SERVER", "http://prometheus:9090")
	agent  = env("PROMC_AGENT", "http://agent:9091")
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

// event writes an assertion record of kind sometimes: the events that reactions (A=) wait for
//
//	prom.write_acked     a remote write was acknowledged
//	prom.block_seen      the server listed a block that no earlier record named
//	prom.snapshot_acked  the server answered a snapshot request with its name
func event(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	fmt.Fprintf(f, "HYDO-ASSERT %s\n", b)
}

// say prints a line to the command's output and to the console, where a run's one.log keeps it
// (a command's own output stays in the guest). hvx's console holds 64 KiB, so only the lines that
// explain a failure go there.
func say(format string, a ...any) {
	line := fmt.Sprintf(format, a...)
	fmt.Println(line)
	if f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0); err == nil {
		fmt.Fprintf(f, "promc: %s\n", line)
		f.Close()
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

type key struct {
	w  int64 // the write's entry
	k  int   // the series
	ts int64 // ms
}

func (s key) id() string { return fmt.Sprintf("%d.%d@%d", s.w, s.k, s.ts) }

// valueOf is the value the client writes for a sample: an integer below 1e9, which Prometheus
// stores exactly.
func valueOf(s key) float64 {
	h := uint64(s.w)*1000003 + uint64(s.k)*7919 + uint64(s.ts)
	h ^= h >> 17
	h *= 0x9e3779b97f4a7c15
	h ^= h >> 29
	return float64(h % 1000000000)
}

type ackTok struct {
	w, t0, up int64
	boot      string
	start     string // the server's start time when it acknowledged the write ("-": not known)
	inOrder   bool   // the series k=0 was new enough for the server's in-order head
}

// ooo says whether the server took sample s of the write as an out-of-order sample: the series
// k=1 always, the series k=0 when its time was more than 25 s behind the head's newest sample
// (after a backward clock step)
func (a ackTok) ooo(s key) bool { return s.k == 1 || !a.inOrder }

func (a ackTok) samples() []key {
	var out []key
	for k := 0; k < nSeries; k++ {
		base := a.t0
		if k == 1 {
			base -= oooBackMs
		}
		for i := 0; i < nSamples; i++ {
			out = append(out, key{a.w, k, base + int64(i)})
		}
	}
	return out
}

type blockTok struct {
	ulid       string
	mint, maxt int64
	ooo        bool
	up         int64
	boot       string
	start      string // the server's start time when the block was listed
}

// seenTok is a scrape that a read found in the server: once queryable, it stays queryable
type seenTok struct {
	n, ts, up   int64
	boot, start string
}

type snapTok struct {
	name   string
	nWrite int
	up     int64
	boot   string
	start  string // the server's start time when it answered
}

// tokens reads $HYDO_ACKED: the acknowledged writes in order, the blocks, the snapshots
func tokens() (ws []ackTok, bs []blockTok, ss []snapTok) {
	ws, bs, ss, _ = allTokens()
	return
}

// allTokens reads $HYDO_ACKED with the scrapes that reads found in the server
func allTokens() (ws []ackTok, bs []blockTok, ss []snapTok, xs []seenTok) {
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, "~")
		switch {
		case len(f) == 7 && f[0] == "w":
			w, e1 := strconv.ParseInt(f[1], 10, 64)
			t0, e2 := strconv.ParseInt(f[2], 10, 64)
			up, e3 := strconv.ParseInt(f[3], 10, 64)
			if e1 == nil && e2 == nil && e3 == nil {
				ws = append(ws, ackTok{w, t0, up, f[4], f[5], f[6] == "1"})
			}
		case len(f) == 8 && f[0] == "b":
			mi, e1 := strconv.ParseInt(f[2], 10, 64)
			ma, e2 := strconv.ParseInt(f[3], 10, 64)
			up, e3 := strconv.ParseInt(f[5], 10, 64)
			if e1 == nil && e2 == nil && e3 == nil {
				bs = append(bs, blockTok{f[1], mi, ma, f[4] == "1", up, f[6], f[7]})
			}
		case len(f) == 6 && f[0] == "x":
			n, e1 := strconv.ParseInt(f[1], 10, 64)
			ts, e2 := strconv.ParseInt(f[2], 10, 64)
			up, e3 := strconv.ParseInt(f[3], 10, 64)
			if e1 == nil && e2 == nil && e3 == nil {
				xs = append(xs, seenTok{n, ts, up, f[4], f[5]})
			}
		case len(f) == 6 && f[0] == "s":
			n, e1 := strconv.Atoi(f[2])
			up, e2 := strconv.ParseInt(f[3], 10, 64)
			if e1 == nil && e2 == nil {
				ss = append(ss, snapTok{f[1], n, up, f[4], f[5]})
			}
		}
	}
	return
}

// ---- remote write 1.0, by hand ----

func pbVarint(b *bytes.Buffer, v uint64) {
	for v >= 0x80 {
		b.WriteByte(byte(v) | 0x80)
		v >>= 7
	}
	b.WriteByte(byte(v))
}

func pbBytes(b *bytes.Buffer, field int, p []byte) {
	pbVarint(b, uint64(field<<3|2))
	pbVarint(b, uint64(len(p)))
	b.Write(p)
}

type series struct {
	labels  [][2]string // sorted by name
	samples []key
}

// writeRequest encodes prometheus.WriteRequest{timeseries: [...]}
func writeRequest(ss []series) []byte {
	var req bytes.Buffer
	for _, s := range ss {
		var ts bytes.Buffer
		for _, l := range s.labels {
			var lb bytes.Buffer
			pbBytes(&lb, 1, []byte(l[0]))
			pbBytes(&lb, 2, []byte(l[1]))
			pbBytes(&ts, 1, lb.Bytes())
		}
		for _, k := range s.samples {
			var sb bytes.Buffer
			pbVarint(&sb, 1<<3|1) // double value = 1
			var f [8]byte
			binary.LittleEndian.PutUint64(f[:], math.Float64bits(valueOf(k)))
			sb.Write(f[:])
			pbVarint(&sb, 2<<3|0) // int64 timestamp = 2
			pbVarint(&sb, uint64(k.ts))
			pbBytes(&ts, 2, sb.Bytes())
		}
		pbBytes(&req, 1, ts.Bytes())
	}
	return req.Bytes()
}

// snappyLiteral encodes p as one snappy block of literals only
func snappyLiteral(p []byte) []byte {
	var b bytes.Buffer
	pbVarint(&b, uint64(len(p)))
	for len(p) > 0 {
		n := len(p)
		if n > 65536 {
			n = 65536
		}
		l := n - 1
		switch {
		case l < 60:
			b.WriteByte(byte(l << 2))
		case l < 1<<8:
			b.WriteByte(60 << 2)
			b.WriteByte(byte(l))
		default:
			b.WriteByte(61 << 2)
			b.WriteByte(byte(l))
			b.WriteByte(byte(l >> 8))
		}
		b.Write(p[:n])
		p = p[n:]
	}
	return b.Bytes()
}

var httpc = &http.Client{Timeout: 15 * time.Second}

func post(body []byte) (int, string, error) {
	req, _ := http.NewRequest("POST", server+"/api/v1/write", bytes.NewReader(body))
	req.Header.Set("Content-Type", "application/x-protobuf")
	req.Header.Set("Content-Encoding", "snappy")
	req.Header.Set("X-Prometheus-Remote-Write-Version", "0.1.0")
	resp, err := httpc.Do(req)
	if err != nil {
		return 0, "", err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	return resp.StatusCode, strings.TrimSpace(string(b)), nil
}

func get(u string) (int, []byte, error) {
	resp, err := httpc.Get(u)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	return resp.StatusCode, b, err
}

// ---- reading the server ----

type got struct {
	v float64
	n int
}

type queryResp struct {
	Status string `json:"status"`
	Error  string `json:"error"`
	Data   struct {
		ResultType string `json:"resultType"`
		Result     []struct {
			Metric map[string]string `json:"metric"`
			Values [][2]any          `json:"values"`
		} `json:"result"`
	} `json:"data"`
}

// rangeQuery returns the raw samples of every series that matches sel, from 2000 to 2100
func rangeQuery(sel string) (*queryResp, error) {
	q := url.Values{}
	q.Set("query", sel+"[100y]")
	q.Set("time", "4102444800") // 2100-01-01: after any clock step of the tests
	code, body, err := get(server + "/api/v1/query?" + q.Encode())
	if err != nil {
		return nil, err
	}
	if code != 200 {
		return nil, fmt.Errorf("query: status %d: %.200s", code, body)
	}
	var r queryResp
	if err := json.Unmarshal(body, &r); err != nil {
		return nil, fmt.Errorf("query: %v", err)
	}
	if r.Status != "success" {
		return nil, fmt.Errorf("query: %s", r.Error)
	}
	return &r, nil
}

func tsOf(x any) int64 {
	f, _ := x.(float64)
	return int64(math.Round(f * 1000))
}

func valOf(x any) float64 {
	s, _ := x.(string)
	v, err := strconv.ParseFloat(s, 64)
	if err != nil {
		return math.NaN()
	}
	return v
}

// readSamples reads every sample of the writes
func readSamples() (map[key]*got, error) {
	r, err := rangeQuery(metric)
	if err != nil {
		return nil, err
	}
	out := map[key]*got{}
	for _, s := range r.Data.Result {
		w, e1 := strconv.ParseInt(s.Metric["w"], 10, 64)
		k, e2 := strconv.Atoi(s.Metric["k"])
		if e1 != nil || e2 != nil {
			return nil, fmt.Errorf("query: a series that no write made: %v", s.Metric)
		}
		for _, p := range s.Values {
			kk := key{w, k, tsOf(p[0])}
			if g, ok := out[kk]; ok {
				g.n++
			} else {
				out[kk] = &got{valOf(p[1]), 1}
			}
		}
	}
	return out, nil
}

type tsdbStatus struct {
	Data struct {
		HeadStats struct {
			MinTime int64 `json:"minTime"`
			MaxTime int64 `json:"maxTime"`
		} `json:"headStats"`
	} `json:"data"`
}

// headMax is the newest sample time of the server's head, or 0
func headMax() int64 {
	code, body, err := get(server + "/api/v1/status/tsdb")
	if err != nil || code != 200 {
		return 0
	}
	var s tsdbStatus
	if json.Unmarshal(body, &s) != nil {
		return 0
	}
	return s.Data.HeadStats.MaxTime
}

// serverStart is the server's start time as /api/v1/status/runtimeinfo gives it: another value
// means that the server started again. "-" when it does not answer.
func serverStart() string {
	code, body, err := get(server + "/api/v1/status/runtimeinfo")
	if err != nil || code != 200 {
		return "-"
	}
	var r struct {
		Data struct {
			StartTime string `json:"startTime"`
		} `json:"data"`
	}
	if json.Unmarshal(body, &r) != nil || r.Data.StartTime == "" {
		return "-"
	}
	return r.Data.StartTime
}

// powerLoss says whether the image's explorations draw a power loss of the server alone (L=,
// the build parameter powerloss). A server that started again since a write may then have lost
// what it had not synced, as after a power cut of the whole guest, so the rules ask only for the
// samples of the blocks it had listed.
func powerLoss() bool {
	b, _ := os.ReadFile("/etc/promc/powerloss")
	return strings.TrimSpace(string(b)) == "yes"
}

// durable says whether sample s of write a must have survived. bs are the blocks the server
// listed, boot and start are this boot's and the server's current start.
func durable(a ackTok, s key, bs []blockTok, boot, start string) bool {
	if a.boot == boot && (!powerLoss() || (a.start != "-" && a.start == start)) {
		return true
	}
	for _, b := range bs {
		if inBlock(b, a, s) {
			return true
		}
	}
	return false
}

func ready(base string) bool {
	code, _, err := get(base + "/-/ready")
	return err == nil && code == 200
}

// ---- the exporter ----

func serve() {
	if err := os.MkdirAll("/data/exp", 0o755); err != nil {
		fmt.Println("serve:", err)
		os.Exit(1)
	}
	// the count goes on after a restart, so that a series name is never served twice
	var n int64
	for _, l := range readLines(servedLog) {
		if f := strings.Fields(l); len(f) >= 1 {
			if v, err := strconv.ParseInt(f[0], 10, 64); err == nil && v > n {
				n = v
			}
		}
	}
	n += 1000 // a power cut can lose the log's tail
	var mu sync.Mutex
	logf, err := os.OpenFile(servedLog, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		fmt.Println("serve:", err)
		os.Exit(1)
	}
	boot := bootID()
	http.HandleFunc("/metrics", func(w http.ResponseWriter, r *http.Request) {
		start := uptimeMs()
		mu.Lock()
		n++
		k := n
		mu.Unlock()
		v := valueOf(key{k, 9, 0})
		w.Header().Set("Content-Type", "text/plain; version=0.0.4")
		fmt.Fprintf(w, "# TYPE %s gauge\n%s{n=\"%d\"} %v\n", scrapeName, scrapeName, k, v)
		fmt.Fprintf(w, "# TYPE %s_count gauge\n%s_count %d\n", scrapeName, scrapeName, k)
		if f, ok := w.(http.Flusher); ok {
			f.Flush()
		}
		end := uptimeMs()
		// a scrape that took long may have passed the agent's timeout of 1 s
		if end-start < 500 {
			mu.Lock()
			fmt.Fprintf(logf, "%d %d %s\n", k, end, boot)
			mu.Unlock()
		}
	})
	fmt.Println("serve: the exporter listens on :9100")
	if err := http.ListenAndServe(":9100", nil); err != nil {
		fmt.Println("serve:", err)
		os.Exit(1)
	}
}

// ---- operations ----

func settle(limit int64) {
	end := uptimeMs() + limit*1000
	for uptimeMs() < end {
		if ready(server) && ready(agent) && len(scraped()) > 0 {
			fmt.Println("settle: the server and the agent are ready, and the agent's samples arrive")
			return
		}
		time.Sleep(time.Second)
	}
	fmt.Printf("settle: not ready after %d s (server %v, agent %v)\n", limit, ready(server), ready(agent))
	os.Exit(1)
}

func write() {
	id, err := strconv.ParseInt(os.Getenv("HYDO_CMD_ID"), 10, 64)
	if err != nil {
		fmt.Println("write: HYDO_CMD_ID is not a number")
		os.Exit(2)
	}
	hm := headMax()
	t0 := time.Now().UnixMilli()
	a := ackTok{w: id, t0: t0}
	io := "1"
	if hm != 0 && t0 < hm-25000 {
		io = "0" // the server's head takes it as an out-of-order sample
	}
	var ss []series
	all := a.samples()
	for k := 0; k < nSeries; k++ {
		ss = append(ss, series{
			labels:  [][2]string{{"__name__", metric}, {"k", strconv.Itoa(k)}, {"w", strconv.FormatInt(id, 10)}},
			samples: all[k*nSamples : (k+1)*nSamples],
		})
	}
	body := snappyLiteral(writeRequest(ss))
	// a sender retries a request without an answer or with a 5xx answer, as Prometheus's own
	// remote write does; a 4xx answer is final
	outcome, tries := "unknown", 0
	var last string
tries:
	for tries < 3 {
		tries++
		code, msg, err := post(body)
		switch {
		case err != nil:
			last = err.Error()
		case code/100 == 2:
			outcome = "ack"
			break tries
		case code/100 == 4:
			last = fmt.Sprintf("status %d: %.200s", code, msg)
			if tries == 1 {
				outcome = "failed" // an earlier try without an answer may have been applied
			}
			break tries
		default:
			last = fmt.Sprintf("status %d: %.200s", code, msg)
		}
		time.Sleep(200 * time.Millisecond)
	}
	switch outcome {
	case "ack":
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("w~%d~%d~%d~%s~%s~%s", id, t0, uptimeMs(), bootID(), serverStart(), io))
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d ack", id))
		event("prom.write_acked", fmt.Sprintf("write %d", id))
		fmt.Printf("write %d: %d samples acknowledged after %d tries, t0 %d\n", id, len(all), tries, t0)
		if id%4 == 0 {
			// the retry of a sender whose answer was lost: the same request again
			code, msg, err := post(body)
			if code/100 != 2 {
				say("write %d: the same request again: status %d %v %.200s", id, code, err, msg)
			}
		}
		blocks() // the blocks the server lists now, for the power-cut rule
	case "failed":
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d failed", id))
		say("write %d: refused: %s", id, last)
	default:
		appendLine(os.Getenv("HYDO_OUTCOMES"), fmt.Sprintf("%d unknown", id))
		say("write %d: no acknowledgement after %d tries: %s", id, tries, last)
	}
}

// judge compares the samples of the writes with what the server returned. need says which
// samples must be there; a sample that is there must have its value either way.
func judge(toks []ackTok, have map[key]*got, need func(ackTok, key) bool) (lost, wrong []key, excused int) {
	for _, a := range toks {
		for _, s := range a.samples() {
			g, ok := have[s]
			switch {
			case ok && g.v != valueOf(s):
				wrong = append(wrong, s)
			case !ok && need(a, s):
				lost = append(lost, s)
			case !ok:
				excused++
			}
		}
	}
	return
}

func ids(ks []key, max int) string {
	var b []string
	for i, s := range ks {
		if i == max {
			b = append(b, fmt.Sprintf("and %d more", len(ks)-max))
			break
		}
		b = append(b, s.id())
	}
	return strings.Join(b, " ")
}

// retained says whether retention may have deleted a sample: the server deletes a block when its
// newest sample is a day older than the newest block's, so a sample a day older than the head's
// newest sample may be gone.
func retainedFn() func(key) bool {
	hm := headMax()
	return func(s key) bool { return hm == 0 || s.ts >= hm-retention }
}

func read() {
	now, boot := uptimeMs(), bootID()
	ws, bs, _ := tokens()
	start := serverStart()
	recordSeen(boot, start)
	var toks []ackTok
	for _, a := range ws {
		if a.boot == boot && a.up <= now-readAgeMs {
			toks = append(toks, a)
		}
	}
	if len(toks) == 0 {
		fmt.Println("read: no acknowledged write is old enough to judge")
		return
	}
	have, err := readSamples()
	if err != nil {
		fmt.Printf("read: the server did not answer: %v\n", err)
		return
	}
	keep := retainedFn()
	lost, wrong, excused := judge(toks, have, func(a ackTok, s key) bool { return keep(s) && durable(a, s, bs, boot, start) })
	if len(lost)+len(wrong) == 0 {
		fmt.Printf("read: %d writes acknowledged 30 s ago or earlier, every sample returned (%d past the retention)\n", len(toks), excused)
		return
	}
	say("read: lost: %s; wrong value: %s", ids(lost, 12), ids(wrong, 12))
	say("read: %d missing, %d with another value, of %d writes", len(lost), len(wrong), len(toks))
	fmt.Println("VIOLATION: samples acknowledged 30 s ago are not returned as written")
	os.Exit(100)
}

type blocksResp struct {
	Data struct {
		Blocks []struct {
			ULID       string `json:"ulid"`
			MinTime    int64  `json:"minTime"`
			MaxTime    int64  `json:"maxTime"`
			Compaction struct {
				Level int      `json:"level"`
				Hints []string `json:"hints"`
			} `json:"compaction"`
		} `json:"blocks"`
	} `json:"data"`
}

// blocks records each block the server lists that no earlier record named
func blocks() {
	code, body, err := get(server + "/api/v1/status/tsdb/blocks")
	if err != nil || code != 200 {
		fmt.Printf("blocks: the server did not answer: %v %d\n", err, code)
		return
	}
	var r blocksResp
	if err := json.Unmarshal(body, &r); err != nil {
		fmt.Println("blocks:", err)
		return
	}
	_, seen, _ := tokens()
	known := map[string]bool{}
	for _, b := range seen {
		known[b.ulid] = true
	}
	up, boot, start, n := uptimeMs(), bootID(), serverStart(), 0
	for _, b := range r.Data.Blocks {
		if known[b.ULID] {
			continue
		}
		ooo := "0"
		for _, h := range b.Compaction.Hints {
			if h == "from-out-of-order" {
				ooo = "1"
			}
		}
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("b~%s~%d~%d~%s~%d~%s~%s", b.ULID, b.MinTime, b.MaxTime, ooo, up, boot, start))
		n++
	}
	if n > 0 {
		event("prom.block_seen", fmt.Sprintf("%d new blocks", n))
	}
	if n > 0 {
		say("blocks: %d blocks listed, %d new", len(r.Data.Blocks), n)
	}
}

// dumpDir reads the samples of the writes from a TSDB directory in the server's root with
// promtool (the server's image has it)
func dumpDir(dir string) (map[key]*got, error) {
	// promtool tsdb dump refuses a directory without a wal folder ("read WAL: find last
	// checkpoint"), and a snapshot has none: give it an empty one (prometheus pull request #20416)
	if host := snapHost + strings.TrimPrefix(dir, snapRoot); strings.HasPrefix(dir, snapRoot) {
		if err := os.MkdirAll(host+"/wal", 0o755); err != nil {
			return nil, err
		}
	}
	cmd := exec.Command("/bin/hydo-oci", "prometheus", "promtool", "tsdb", "dump",
		"--sandbox-dir-root=/tmp", `--match={__name__="`+metric+`"}`, dir)
	var errb bytes.Buffer
	cmd.Stderr = &errb
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("promtool tsdb dump %s: %v: %.300s", dir, err, errb.String())
	}
	have := map[key]*got{}
	sc := bufio.NewScanner(bytes.NewReader(out))
	sc.Buffer(make([]byte, 1<<20), 64<<20)
	for sc.Scan() {
		// {__name__="hydo_rw", k="0", w="12"} 123 1794960000123
		line := sc.Text()
		i := strings.LastIndex(line, "} ")
		if i < 0 {
			continue
		}
		f := strings.Fields(line[i+2:])
		if len(f) != 2 {
			continue
		}
		lbl := map[string]string{}
		for _, p := range strings.Split(strings.Trim(line[:i+1], "{}"), ",") {
			kv := strings.SplitN(strings.TrimSpace(p), "=", 2)
			if len(kv) == 2 {
				lbl[kv[0]] = strings.Trim(kv[1], `"`)
			}
		}
		w, e1 := strconv.ParseInt(lbl["w"], 10, 64)
		k, e2 := strconv.Atoi(lbl["k"])
		ts, e3 := strconv.ParseInt(f[1], 10, 64)
		v, e4 := strconv.ParseFloat(f[0], 64)
		if e1 != nil || e2 != nil || e3 != nil || e4 != nil {
			continue
		}
		kk := key{w, k, ts}
		if g, ok := have[kk]; ok {
			g.n++
		} else {
			have[kk] = &got{v, 1}
		}
	}
	return have, sc.Err()
}

// judgeOOOSnapshots says whether a snapshot must hold the out-of-order samples that the server
// still keeps in its out-of-order head (the build parameter ooo_snapshots, RESULTS.md finding 1).
// With skip, a snapshot must hold only the in-order samples and the out-of-order samples of the
// out-of-order blocks that the server had listed before it.
func judgeOOOSnapshots() bool {
	b, _ := os.ReadFile("/etc/promc/ooo_snapshots")
	return strings.TrimSpace(string(b)) != "skip"
}

// inBlock says whether block b holds sample s of write a. An in-order block holds every in-order
// sample of its range, because the head refuses in-order samples older than a cut block. An
// out-of-order block holds the out-of-order samples of its range that the out-of-order head had
// when the server compacted it: those acknowledged well before the block was listed (5 s).
func inBlock(b blockTok, a ackTok, s key) bool {
	if b.boot != a.boot || b.mint > s.ts || s.ts >= b.maxt || b.ooo != a.ooo(s) {
		return false
	}
	// a block listed after the server started again was made from what survived
	if !sameLife(a.start, b.start) {
		return false
	}
	if b.ooo {
		return a.up+5000 < b.up
	}
	return a.up < b.up
}

// sameLife says whether two records saw one run of the server (the same start time); an unknown
// start ("-") counts as another one
func sameLife(a, b string) bool { return a != "-" && a == b }

// snapNeed returns the rule of what a snapshot taken at uptime up in boot must hold
func snapNeed(bs []blockTok, up int64, boot string) func(ackTok, key) bool {
	strict := judgeOOOSnapshots()
	return func(a ackTok, s key) bool {
		if !a.ooo(s) || strict {
			return true
		}
		for _, b := range bs {
			if b.boot == boot && b.up < up && inBlock(b, a, s) {
				return true
			}
		}
		return false
	}
}

// snapshot asks for a snapshot with the head and checks that it holds every sample acknowledged
// before the request
func snapshot() {
	ws, bs, _ := tokens()
	boot, asked := bootID(), uptimeMs()
	var before []ackTok
	for _, a := range ws {
		if a.boot == boot {
			before = append(before, a)
		}
	}
	req, _ := http.NewRequest("POST", server+"/api/v1/admin/tsdb/snapshot", nil)
	resp, err := httpc.Do(req)
	if err != nil {
		say("snapshot: no answer: %v", err)
		return
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	if resp.StatusCode != 200 {
		say("snapshot: status %d: %.200s", resp.StatusCode, b)
		return
	}
	var r struct {
		Data struct {
			Name string `json:"name"`
		} `json:"data"`
	}
	if json.Unmarshal(b, &r) != nil || r.Data.Name == "" {
		say("snapshot: an answer without a name: %.200s", b)
		return
	}
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("s~%s~%d~%d~%s~%s", r.Data.Name, len(ws), uptimeMs(), boot, serverStart()))
	event("prom.snapshot_acked", r.Data.Name)
	have, err := dumpDir(snapRoot + "/" + r.Data.Name)
	if err != nil {
		say("snapshot: %v", err)
		return
	}
	keep, need, start := retainedFn(), snapNeed(bs, asked, boot), serverStart()
	lost, wrong, excused := judge(before, have, func(a ackTok, s key) bool {
		return keep(s) && need(a, s) && durable(a, s, bs, boot, start)
	})
	if excused > 0 && !judgeOOOSnapshots() {
		say("snapshot %s: %d samples acknowledged before it are not in it, out-of-order samples not judged (ooo_snapshots=skip)", r.Data.Name, excused)
	}
	inOrder := 0
	for _, s := range lost {
		if s.k == 0 {
			inOrder++
		}
	}
	if len(lost)+len(wrong) == 0 {
		say("snapshot %s: every sample of the %d writes acknowledged before it", r.Data.Name, len(before))
		return
	}
	say("snapshot %s: lost: %s; wrong value: %s", r.Data.Name, ids(lost, 12), ids(wrong, 12))
	say("snapshot %s lacks %d samples (%d in order, %d out of order) and has %d with another value, of %d writes acknowledged before it",
		r.Data.Name, len(lost), inOrder, len(lost)-inOrder, len(wrong), len(before))
	fmt.Println("VIOLATION: a snapshot lacks samples acknowledged before it")
	os.Exit(100)
}

// recordSeen records each scrape that the server has and no earlier record names
func recordSeen(boot, start string) {
	have, err := scrapedTimes()
	if err != nil {
		return
	}
	_, _, _, xs := allTokens()
	known := map[int64]bool{}
	for _, x := range xs {
		known[x.n] = true
	}
	var ns []int64
	for n := range have {
		if !known[n] {
			ns = append(ns, n)
		}
	}
	sort.Slice(ns, func(i, j int) bool { return ns[i] < ns[j] })
	up := uptimeMs()
	for _, n := range ns {
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("x~%d~%d~%d~%s~%s", n, have[n][0], up, boot, start))
	}
}

// checkSeen: every scrape that a read found in the server is still there, as the rule of
// acknowledged writes says: after a power cut, or a power loss of the server, only when a block
// that the server listed later holds its time
func checkSeen(bs []blockTok, boot, start string) (lost []string, n int) {
	_, _, _, xs := allTokens()
	if len(xs) == 0 {
		return nil, 0
	}
	have, err := scrapedTimes()
	if err != nil {
		fmt.Println("the server did not answer:", err)
		os.Exit(2)
	}
	hm := headMax()
	for _, x := range xs {
		if hm != 0 && x.ts < hm-retention {
			continue
		}
		n++
		found := false
		for _, t := range have[x.n] {
			if t == x.ts {
				found = true
			}
		}
		if found {
			continue
		}
		need := x.boot == boot && (!powerLoss() || (x.start != "-" && x.start == start))
		for _, b := range bs {
			if b.boot == x.boot && sameLife(x.start, b.start) && b.up > x.up+5000 && b.mint <= x.ts && x.ts < b.maxt {
				need = true
			}
		}
		if need {
			lost = append(lost, fmt.Sprintf("%d@%d", x.n, x.ts))
		}
	}
	return lost, n
}

// checkSamples: every acknowledged sample is queryable with its value. A sample of a write in an
// earlier boot (before a power cut) must be there only when the server listed a block that holds
// it after the write's acknowledgement: until a block is cut, the server keeps new samples in its
// write-ahead log without an fsync. Exit 2 when the server does not answer.
func checkSamples() {
	ws, bs, _ := tokens()
	if len(ws) == 0 {
		fmt.Println("no acknowledged write")
		return
	}
	have, err := readSamples()
	if err != nil {
		fmt.Println("the server did not answer:", err)
		os.Exit(2)
	}
	boot := bootID()
	keep := retainedFn()
	blocked := func(a ackTok, s key) bool {
		for _, b := range bs {
			if inBlock(b, a, s) {
				return true
			}
		}
		return false
	}
	start := serverStart()
	lost, wrong, excused := judge(ws, have, func(a ackTok, s key) bool {
		return keep(s) && (durable(a, s, bs, boot, start) || blocked(a, s))
	})
	seenLost, seen := checkSeen(bs, boot, start)
	if len(seenLost) > 0 {
		fmt.Printf("scrapes no longer queryable: %s\n", strings.Join(seenLost[:min(len(seenLost), 20)], " "))
		fmt.Printf("%d of %d scrapes that a read found in the server are gone\n", len(seenLost), seen)
		if len(lost)+len(wrong) == 0 {
			os.Exit(1)
		}
	}
	if len(lost)+len(wrong) == 0 {
		fmt.Printf("%d acknowledged writes: every sample that must be there is there with its value (%d may be gone: unsynced before a power cut, or past the retention)\n", len(ws), excused)
		return
	}
	cut := 0
	for _, s := range lost {
		for _, a := range ws {
			if a.w == s.w && a.boot != boot {
				cut++
				break
			}
		}
	}
	fmt.Printf("lost: %s\nwrong value: %s\n", ids(lost, 30), ids(wrong, 30))
	fmt.Printf("%d acknowledged samples missing (%d of them from before a power cut, in a block the server had listed), %d with another value, of %d writes\n",
		len(lost), cut, len(wrong), len(ws))
	fmt.Println("acknowledged samples are not returned as written")
	os.Exit(1)
}

// checkSnapshots: every snapshot still holds the samples acknowledged before it (after a power
// cut too: the snapshot API had answered)
func checkSnapshots() {
	ws, bs, ss := tokens()
	if len(ss) == 0 {
		fmt.Println("no snapshot was taken")
		return
	}
	bad, keep := 0, retainedFn()
	for _, s := range ss {
		var before []ackTok
		for _, a := range ws[:min(s.nWrite, len(ws))] {
			if a.boot == s.boot {
				before = append(before, a)
			}
		}
		if _, err := os.Stat(snapHost + "/" + s.name); err != nil {
			fmt.Printf("snapshot %s (taken in boot %s): its directory is gone\n", s.name, s.boot)
			bad++
			continue
		}
		have, err := dumpDir(snapRoot + "/" + s.name)
		if err != nil {
			fmt.Printf("snapshot %s: %v\n", s.name, err)
			bad++
			continue
		}
		need := snapNeed(bs, s.up, s.boot)
		var earlier []blockTok // the blocks listed before the snapshot
		for _, b := range bs {
			if b.boot == s.boot && b.up < s.up {
				earlier = append(earlier, b)
			}
		}
		lost, wrong, _ := judge(before, have, func(a ackTok, k key) bool {
			return keep(k) && need(a, k) && durable(a, k, earlier, s.boot, s.start)
		})
		if len(lost)+len(wrong) > 0 {
			fmt.Printf("snapshot %s (boot %s): lacks %s; wrong %s\n", s.name, s.boot, ids(lost, 10), ids(wrong, 10))
			bad++
		}
	}
	if bad == 0 {
		fmt.Printf("%d snapshots: each holds every sample acknowledged before it\n", len(ss))
		return
	}
	fmt.Printf("%d of %d snapshots lack samples acknowledged before them\n", bad, len(ss))
	os.Exit(1)
}

// scraped returns the exporter's series numbers that the server has
func scraped() map[int64]bool {
	r, err := rangeQuery(scrapeName)
	if err != nil {
		return nil
	}
	out := map[int64]bool{}
	for _, s := range r.Data.Result {
		n, err := strconv.ParseInt(s.Metric["n"], 10, 64)
		if err != nil {
			continue
		}
		for _, p := range s.Values {
			if v := valOf(p[1]); !math.IsNaN(v) {
				out[n] = true
			}
		}
	}
	return out
}

// scrapedTimes returns, for each exporter series number that the server has, the timestamps of
// its samples (staleness markers left out)
func scrapedTimes() (map[int64][]int64, error) {
	r, err := rangeQuery(scrapeName)
	if err != nil {
		return nil, err
	}
	out := map[int64][]int64{}
	for _, s := range r.Data.Result {
		n, err := strconv.ParseInt(s.Metric["n"], 10, 64)
		if err != nil {
			continue
		}
		for _, p := range s.Values {
			if v := valOf(p[1]); !math.IsNaN(v) {
				out[n] = append(out[n], tsOf(p[0]))
			}
		}
	}
	return out, nil
}

// counter sums the samples of a metric on a Prometheus's own /metrics page (not its TSDB)
func counter(base, name string) float64 {
	code, body, err := get(base + "/metrics")
	if err != nil || code != 200 {
		return math.NaN()
	}
	sum := 0.0
	for _, l := range strings.Split(string(body), "\n") {
		if strings.HasPrefix(l, name+" ") || strings.HasPrefix(l, name+"{") {
			f := strings.Fields(l)
			if v, err := strconv.ParseFloat(f[len(f)-1], 64); err == nil {
				sum += v
			}
		}
	}
	return sum
}

// checkTimes: two scrapes that the exporter served at least 900 ms apart (by the guest's uptime)
// carry two timestamps in the server. A scrape's timestamp is the time of the scrape; a clock
// step back may make a later scrape's timestamp smaller, never the same. Exit 2 when the server
// does not answer.
func checkTimes() {
	if b, _ := os.ReadFile("/etc/promc/scrape_times"); strings.TrimSpace(string(b)) == "skip" {
		fmt.Println("not judged (scrape_times=skip): the explorations leave finding 2 out")
		return
	}
	have, err := scrapedTimes()
	if err != nil {
		fmt.Println("the server did not answer:", err)
		os.Exit(2)
	}
	type served struct {
		n, up int64
		boot  string
	}
	var ss []served
	for _, l := range readLines(servedLog) {
		f := strings.Fields(l)
		if len(f) != 3 {
			continue
		}
		n, e1 := strconv.ParseInt(f[0], 10, 64)
		up, e2 := strconv.ParseInt(f[1], 10, 64)
		if e1 == nil && e2 == nil && len(have[n]) == 1 {
			ss = append(ss, served{n, up, f[2]})
		}
	}
	sort.Slice(ss, func(i, j int) bool { return ss[i].n < ss[j].n })
	same, pairs := 0, 0
	var first []string
	for i := 1; i < len(ss); i++ {
		a, b := ss[i-1], ss[i]
		if a.boot != b.boot || b.up-a.up < 900 {
			continue
		}
		pairs++
		if have[a.n][0] == have[b.n][0] {
			same++
			if len(first) < 5 {
				first = append(first, fmt.Sprintf("%d and %d (served %d ms apart) at %d", a.n, b.n, b.up-a.up, have[a.n][0]))
			}
		}
	}
	if same == 0 {
		fmt.Printf("%d pairs of scrapes in the server: each scrape has its own timestamp\n", pairs)
		return
	}
	fmt.Println("the same timestamp: " + strings.Join(first, "; "))
	for _, b := range []struct{ name, base string }{{"server", server}, {"agent", agent}} {
		fmt.Printf("%s: %v samples dropped as duplicates of a timestamp, %v as out of order (its /metrics)\n", b.name,
			counter(b.base, "prometheus_target_scrapes_sample_duplicate_timestamp_total"),
			counter(b.base, "prometheus_target_scrapes_sample_out_of_order_total"))
	}
	fmt.Printf("%d of %d pairs of scrapes served a second apart carry the same timestamp in the server\n", same, pairs)
	os.Exit(1)
}

// checkScrape: of the scrapes the exporter served 10 to 45 s ago in this boot, at least one is in
// the server. Exit 2 when the server does not answer.
func checkScrape() {
	now, boot := uptimeMs(), bootID()
	var recent []int64
	for _, l := range readLines(servedLog) {
		f := strings.Fields(l)
		if len(f) != 3 || f[2] != boot {
			continue
		}
		n, e1 := strconv.ParseInt(f[0], 10, 64)
		up, e2 := strconv.ParseInt(f[1], 10, 64)
		if e1 == nil && e2 == nil && up >= now-45000 && up <= now-10000 {
			recent = append(recent, n)
		}
	}
	if !ready(server) {
		fmt.Println("the server is not ready")
		os.Exit(2)
	}
	if hm := headMax(); hm != 0 && time.Now().UnixMilli() < hm-oooWindowMs+30000 {
		fmt.Printf("not judged: the clock is %d s behind the server's newest sample, past its out-of-order window, so the server refuses new samples by design\n", (hm-time.Now().UnixMilli())/1000)
		return
	}
	have := scraped()
	in := 0
	for _, n := range recent {
		if have[n] {
			in++
		}
	}
	if in > 0 {
		fmt.Printf("%d of the %d scrapes served 10 to 45 s ago are in the server\n", in, len(recent))
		return
	}
	fmt.Printf("none of the %d scrapes served 10 to 45 s ago is in the server (it has %d scraped series)\n", len(recent), len(have))
	os.Exit(1)
}

func dump() {
	have, err := readSamples()
	if err != nil {
		fmt.Println(err)
		os.Exit(2)
	}
	keys := make([]key, 0, len(have))
	for s := range have {
		keys = append(keys, s)
	}
	sort.Slice(keys, func(i, j int) bool {
		if keys[i].w != keys[j].w {
			return keys[i].w < keys[j].w
		}
		if keys[i].k != keys[j].k {
			return keys[i].k < keys[j].k
		}
		return keys[i].ts < keys[j].ts
	})
	for _, s := range keys {
		fmt.Printf("%s %v x%d (want %v)\n", s.id(), have[s].v, have[s].n, valueOf(s))
	}
	fmt.Printf("%d samples; head max %d; %d scraped series\n", len(keys), headMax(), len(scraped()))
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: promc serve|settle|write|read|blocks|snapshot|check samples|check snapshots|check scrape|dump")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "serve":
		serve()
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
	case "blocks":
		blocks()
	case "snapshot":
		snapshot()
	case "check":
		what := ""
		if len(os.Args) > 2 {
			what = os.Args[2]
		}
		switch what {
		case "samples":
			checkSamples()
		case "snapshots":
			checkSnapshots()
		case "scrape":
			checkScrape()
		case "times":
			checkTimes()
		default:
			fmt.Fprintln(os.Stderr, "usage: promc check samples|snapshots|scrape")
			os.Exit(2)
		}
	case "dump":
		dump()
	default:
		fmt.Fprintln(os.Stderr, "unknown command", os.Args[1])
		os.Exit(2)
	}
}
