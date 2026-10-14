// spread-drv — a black-box consistency driver for the multi-broker spread harness.
//
// Reads a fuzzed PROGRAM from stdin (one op per line), drives it at two spread brokers over
// HTTP (B1=:8080, B2=:8081, sharing one log), then DRAINS the queue and checks the multi-broker
// invariants from the outside — a mini-Jepsen consistency check. It NEVER modifies spread; it
// only observes HTTP responses. Violations print `BUG-ASSERT <name>` (caught by the harness bug
// oracle); reachability markers print `RPT sat <name>` (the property SDK). Determinism: the
// driver is a pure function of the program + the two brokers' deterministic responses.
//
// Program ops (unknown lines ignored, so a byte-mutating proposer degrades gracefully):
//
//	S <b> <group> <body>          send <body> to <group> via broker b (1|2)
//	R <b>                         receive+delete a batch via broker b
//	P <gA> <gB> <bodyA> <bodyB>   CONCURRENT send: bodyA→gA via B1 and bodyB→gB via B2 at once
//	                              (races the shared-log append -> exercises fencing/conflict).
//	                              gA != gB so per-group FIFO stays well-defined.
//	X <b>                         CRASH broker b (SIGKILL its pid) -> failover test
//	KN <i> / KC <i>               CRASH (SIGKILL) log NODE i / CONTROL replica i of the native tier
//	                              -> loglet seal+reconfiguration / VSR view-change under load.
//	QN <i> / QC <i>               PARTITION (SIGSTOP) log node i / control replica i off, healed at
//	                              the end. Tier faults are capped (>=2/3 of each group always live),
//	                              so the tier keeps quorum and consistency stays a meaningful check.
//
// After the program, the driver drains via BOTH brokers to completion.
//
// A send outcome is one of ACCEPTED (200 + durable id/dedup), REJECTED (a clean 4xx — definitively
// not stored) or AMBIGUOUS (timeout/503 — indeterminate: the tier may still commit via idempotent
// retry). Ambiguous sends are exempt from the invariants below (delivering one is valid
// at-least-once; not delivering it is valid too) — asserting otherwise is unsound (Jepsen).
//
// Invariants checked:
//   - no-split-brain : a DEFINITIVELY rejected (4xx) body must never be delivered
//   - no-phantom     : a body never sent at all must never be delivered
//   - fifo           : per group, first-delivery order of ACCEPTED bodies == accepted (program) order
//   - at-least-once  : every accepted body is received at least once (nothing lost, incl. failover)
//   - broker-liveness: a broker we did NOT crash must still be alive at the end (no unexpected death)
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
)

// sortedKeys returns a map's keys in a stable order, so the driver's serial output is
// deterministic regardless of Go's randomized map iteration.
func sortedKeys(m map[string][]string) []string {
	ks := make([]string, 0, len(m))
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}

var client = &http.Client{Timeout: 2 * time.Second}

func port(b string) string {
	if b == "2" {
		return "8081"
	}
	return "8080"
}

// post sends a JSON body and returns (status, responseBody). status 0 on transport error.
func post(p, path, body string) (int, []byte) {
	req, err := http.NewRequest("POST", "http://127.0.0.1:"+p+path, strings.NewReader(body))
	if err != nil {
		return 0, nil
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := client.Do(req)
	if err != nil {
		if os.Getenv("DRV_DEBUG") != "" {
			fmt.Printf("DBG %s%s ERR %v\n", p, path, err)
		}
		return 0, nil
	}
	defer resp.Body.Close()
	rb0, _ := io.ReadAll(resp.Body)
	if os.Getenv("DRV_DEBUG") != "" {
		bs := string(rb0)
		if len(bs) > 200 {
			bs = bs[:200]
		}
		fmt.Printf("DBG %s%s -> %d %s\n", p, path, resp.StatusCode, bs)
	}
	return resp.StatusCode, rb0
}

type sendResult struct {
	ID          string `json:"id"`
	DuplicateOf string `json:"duplicate_of"`
}
type sendResp struct {
	Results []sendResult `json:"results"`
}
type message struct {
	ID      string `json:"id"`
	Receipt string `json:"receipt"`
	Group   string `json:"group"`
	Body    string `json:"body"`
}
type recvResp struct {
	Messages []message `json:"messages"`
}

func jsonEscape(s string) string {
	b, _ := json.Marshal(s)
	return string(b) // includes surrounding quotes
}

func main() {
	args := os.Args[1:]
	if len(args) >= 1 {
		switch args[0] {
		case "health": // health <port> — poll /healthz until 200 (reliable Go HTTP; nc is flaky).
			for i := 0; i < 6000; i++ {
				req, _ := http.NewRequest("GET", "http://127.0.0.1:"+args[1]+"/healthz", nil)
				if resp, err := client.Do(req); err == nil {
					resp.Body.Close()
					if resp.StatusCode == 200 {
						return
					}
				}
				time.Sleep(20 * time.Millisecond)
			}
			fmt.Println("health: timeout")
			return
		case "create": // create <port> <queue>
			st, _ := post(args[1], "/v1/queues", `{"name":`+jsonEscape(args[2])+`}`)
			fmt.Printf("create %s -> %d\n", args[2], st)
			return
		case "healthz": // healthz <port> — ONE GET /healthz; exit 0 iff 200. For a racy boot loop
			// that must tell "broker up" from "broker died" fast (unlike the blocking `health`).
			req, _ := http.NewRequest("GET", "http://127.0.0.1:"+args[1]+"/healthz", nil)
			if resp, err := client.Do(req); err == nil {
				resp.Body.Close()
				if resp.StatusCode == 200 {
					os.Exit(0)
				}
			}
			os.Exit(1)
		case "smoke": // smoke <port> — Phase-1 single-broker end-to-end: create queue q, send two
			// ordered messages to one group, receive+delete them, and check FIFO/at-least-once from
			// the outside. Fully self-contained (no second broker, no drain loop) and deterministic,
			// so it doubles as the hvx boot oracle. Prints exactly one HYDO-ORACLE: OK|VIOLATION line.
			p := args[1]
			smoke(p)
			return
		}
	}
	runProgram()
}

// smoke drives the Phase-1 single-broker workload against port p and prints one
// HYDO-ORACLE: line. It observes only HTTP responses (never touches spread's internals).
func smoke(p string) {
	fail := func(why string) { fmt.Printf("HYDO-ORACLE: VIOLATION %s\n", why); }

	// 1. create the queue (idempotent: a 200/201 or an already-exists 409 are all fine).
	cst, _ := post(p, "/v1/queues", `{"name":"q"}`)
	if cst != 200 && cst != 201 && cst != 409 {
		fail(fmt.Sprintf("create_queue status=%d", cst))
		return
	}

	// 2. send two ordered messages to one group (per-group FIFO is well-defined).
	sent := []string{"spread-msg-0", "spread-msg-1"}
	for _, body := range sent {
		payload := `{"messages":[{"group":"g1","body":` + jsonEscape(body) + `}]}`
		st, rb := post(p, "/v1/queues/q/messages", payload)
		if st != 200 {
			fail(fmt.Sprintf("send status=%d body=%s", st, body))
			return
		}
		var sr sendResp
		if json.Unmarshal(rb, &sr) != nil || len(sr.Results) != 1 ||
			(sr.Results[0].ID == "" && sr.Results[0].DuplicateOf == "") {
			fail(fmt.Sprintf("send no durable id body=%s resp=%s", body, string(rb)))
			return
		}
	}

	// 3. receive+delete until we have both bodies (bounded), recording delivery order.
	var got []string
	for round := 0; round < 200 && len(got) < len(sent); round++ {
		st, rb := post(p, "/v1/queues/q/receive", `{"max":100}`)
		if st != 200 {
			fail(fmt.Sprintf("receive status=%d", st))
			return
		}
		var rr recvResp
		if json.Unmarshal(rb, &rr) != nil {
			fail("receive bad json")
			return
		}
		if len(rr.Messages) == 0 {
			continue
		}
		receipts := make([]string, 0, len(rr.Messages))
		for _, m := range rr.Messages {
			got = append(got, m.Body)
			receipts = append(receipts, m.Receipt)
		}
		rj, _ := json.Marshal(receipts)
		for k := 0; k < 40; k++ {
			if dst, _ := post(p, "/v1/queues/q/delete", `{"receipts":`+string(rj)+`}`); dst == 200 {
				break
			}
		}
	}

	// 4. FIFO + at-least-once: exactly the two bodies, in send order.
	if len(got) != len(sent) {
		fail(fmt.Sprintf("count sent=%d got=%d %v", len(sent), len(got), got))
		return
	}
	for i := range sent {
		if got[i] != sent[i] {
			fail(fmt.Sprintf("fifo pos=%d want=%s got=%s", i, sent[i], got[i]))
			return
		}
	}
	fmt.Printf("HYDO-ORACLE: OK sent=%d received=%d fifo=ok\n", len(sent), len(got))
}

func runProgram() {
	// accepted[group] = bodies accepted, in program order; acceptedSet / rejectedSet by body.
	accepted := map[string][]string{}
	acceptedSet := map[string]bool{}
	rejectedSet := map[string]bool{}  // DEFINITIVE rejections (clean 4xx) — must never be delivered.
	ambiguousSet := map[string]bool{} // timeout/503 — MAY be delivered (the tier can still commit).
	recv := map[string][]string{}
	anyAccept, anyFenced, gotAny, anyConc := false, false, false, false
	anyPause, anyNack := false, false
	killed := map[string]bool{} // brokers this program intentionally crashed (SIGKILL)
	paused := map[string]bool{} // brokers currently partitioned off (SIGSTOP)
	// Native-tier members faulted by this program (log nodes /n<i>.pid, control replicas
	// /c<i>.pid). Capped by the proposer so at most one of each is ever gone -> the tier keeps
	// quorum and must stay consistent through seal/reconfiguration and VSR view-change.
	killedNode, pausedNode := map[string]bool{}, map[string]bool{}
	killedCtrl, pausedCtrl := map[string]bool{}, map[string]bool{}
	anyNodeKill, anyCtrlKill, anyNodePause, anyCtrlPause := false, false, false, false

	signalPidFile := func(path string, sig syscall.Signal) bool {
		pidB, err := os.ReadFile(path)
		if err != nil {
			return false
		}
		pid, e := strconv.Atoi(strings.TrimSpace(string(pidB)))
		if e != nil {
			return false
		}
		return syscall.Kill(pid, sig) == nil
	}
	signalBroker := func(b string, sig syscall.Signal) bool {
		return signalPidFile("/b"+b+".pid", sig)
	}
	// livePort routes a single-broker op away from a broker we've partitioned off — a real client
	// retries another endpoint rather than hanging on the unreachable one. P (concurrent) does NOT
	// use this: it intentionally hits both, so a send to a paused broker times out and is rejected.
	livePort := func(b string) string {
		if paused[b] {
			if b == "1" {
				return port("2")
			}
			return port("1")
		}
		return port(b)
	}

	// receiveDelete does one receive+delete via broker `p`, recording delivered bodies into
	// `recv` in delivery order. Returns how many it got. Receive response is {"messages":[...]}.
	receiveDelete := func(p string) int {
		st, rb := post(p, "/v1/queues/q/receive", `{"max":100}`)
		if st != 200 {
			return 0
		}
		var rr recvResp
		if json.Unmarshal(rb, &rr) != nil || len(rr.Messages) == 0 {
			return 0
		}
		receipts := make([]string, 0, len(rr.Messages))
		for _, m := range rr.Messages {
			recv[m.Group] = append(recv[m.Group], m.Body)
			receipts = append(receipts, m.Receipt)
		}
		rj, _ := json.Marshal(receipts)
		// CONFIRM the delete. A delete that fails/times out (e.g. during a VSR view-change or a
		// loglet reconfiguration) would leave these messages IN-FLIGHT for the 30s visibility
		// timeout — and under FIFO that blocks each group's SUCCESSOR, which a fast drain would
		// then wrongly report as lost. A real consumer must confirm deletion, not fire-and-forget,
		// so retry until acknowledged (idempotent by receipt; a broker that already applied it
		// answers 200 again). The receive only succeeds on a reachable broker, so this never spins
		// on a dead one.
		for k := 0; k < 40; k++ {
			if dst, _ := post(p, "/v1/queues/q/delete", `{"receipts":`+string(rj)+`}`); dst == 200 {
				break
			}
		}
		gotAny = true
		return len(rr.Messages)
	}

	// classifySend records a send's outcome into exactly one of three buckets:
	//   * ACCEPTED   — 200 with a durable id (or a dedup `duplicate_of`): the body IS in the log
	//                  and MUST be delivered (at-least-once) in accepted order (fifo).
	//   * REJECTED   — a DEFINITIVE, deterministic refusal (a clean 4xx): the tier decided not to
	//                  store it, so it must NEVER be delivered (no-split-brain).
	//   * AMBIGUOUS  — a TIMEOUT (st==0) or 503 (backpressure/mid-reconfiguration): the outcome is
	//                  INDETERMINATE. During a reconfiguration the tier can still commit the append
	//                  via its idempotent internal retry, so the body MAY or MAY NOT be delivered —
	//                  and BOTH are correct (at-least-once with an ambiguous ack). Asserting an
	//                  ambiguous send "must never be delivered" is unsound (Jepsen's
	//                  indeterminate-result rule); such a body is exempt from BOTH invariants.
	// Called from the SEQUENTIAL point (never concurrently) so the maps are race-free.
	classifySend := func(group, body string, st int, rb []byte) {
		if st == 200 {
			var sr sendResp
			if json.Unmarshal(rb, &sr) == nil && len(sr.Results) == 1 &&
				(sr.Results[0].ID != "" || sr.Results[0].DuplicateOf != "") {
				accepted[group] = append(accepted[group], body)
				acceptedSet[body] = true
				anyAccept = true
				return
			}
		}
		anyFenced = true // a send that did not cleanly land (definitive OR ambiguous) — seam reached.
		if st >= 400 && st < 500 {
			rejectedSet[body] = true // definitive refusal -> must never be delivered.
		} else {
			ambiguousSet[body] = true // timeout / 503 -> may commit; exempt from both invariants.
		}
	}
	sendPayload := func(group, body string) string {
		return `{"messages":[{"group":` + jsonEscape(group) + `,"body":` + jsonEscape(body) + `}]}`
	}

	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 0, 1<<20), 1<<20)
	for sc.Scan() {
		f := strings.Fields(sc.Text())
		if len(f) == 0 {
			continue
		}
		switch f[0] {
		case "S":
			if len(f) < 4 {
				continue
			}
			st, rb := post(livePort(f[1]), "/v1/queues/q/messages", sendPayload(f[2], f[3]))
			classifySend(f[2], f[3], st, rb)
		case "R":
			if len(f) >= 2 {
				receiveDelete(livePort(f[1]))
			}
		case "P": // concurrent send: bodyA->gA via B1 and bodyB->gB via B2 at the same instant.
			if len(f) < 5 {
				continue
			}
			gA, gB, bodyA, bodyB := f[1], f[2], f[3], f[4]
			type sres struct {
				st int
				rb []byte
			}
			out := make([]sres, 2)
			spec := []struct{ p, g, body string }{{"8080", gA, bodyA}, {"8081", gB, bodyB}}
			var wg sync.WaitGroup
			wg.Add(2)
			for i := range spec {
				go func(i int) {
					defer wg.Done()
					st, rb := post(spec[i].p, "/v1/queues/q/messages", sendPayload(spec[i].g, spec[i].body))
					out[i] = sres{st, rb}
				}(i)
			}
			wg.Wait()
			anyConc = true
			// classify AFTER both finish, in fixed order — deterministic, no concurrent map writes.
			for i := range spec {
				classifySend(spec[i].g, spec[i].body, out[i].st, out[i].rb)
			}
		case "X": // crash broker b: SIGKILL. Failover test.
			if len(f) >= 2 && signalBroker(f[1], syscall.SIGKILL) {
				killed[f[1]] = true
			}
		case "Z": // partition broker b OFF: SIGSTOP (alive but unreachable, still 'holds' its lease).
			if len(f) >= 2 && !killed[f[1]] && signalBroker(f[1], syscall.SIGSTOP) {
				paused[f[1]] = true
				anyPause = true
			}
		case "U": // heal the partition: SIGCONT broker b (it must detect it was fenced, not corrupt).
			if len(f) >= 2 && paused[f[1]] {
				signalBroker(f[1], syscall.SIGCONT)
				paused[f[1]] = false
			}
		case "KN": // CRASH log node i: SIGKILL -> loglet seal + reconfiguration onto survivors.
			if len(f) >= 2 && signalPidFile("/n"+f[1]+".pid", syscall.SIGKILL) {
				killedNode[f[1]] = true
				anyNodeKill = true
			}
		case "KC": // CRASH control replica i: SIGKILL -> VSR view-change if it was primary.
			if len(f) >= 2 && signalPidFile("/c"+f[1]+".pid", syscall.SIGKILL) {
				killedCtrl[f[1]] = true
				anyCtrlKill = true
			}
		case "QN": // PARTITION log node i off: SIGSTOP (silent; if it was the sequencer -> reconfigure).
			if len(f) >= 2 && !killedNode[f[1]] && signalPidFile("/n"+f[1]+".pid", syscall.SIGSTOP) {
				pausedNode[f[1]] = true
				anyNodePause = true
			}
		case "QC": // PARTITION control replica i off: SIGSTOP (if it was the VSR primary -> view-change).
			if len(f) >= 2 && !killedCtrl[f[1]] && signalPidFile("/c"+f[1]+".pid", syscall.SIGSTOP) {
				pausedCtrl[f[1]] = true
				anyCtrlPause = true
			}
		case "N": // receive via b, then NACK (visibility 0) instead of delete -> redelivery at HEAD.
			if len(f) >= 2 {
				p := livePort(f[1])
				st, rb := post(p, "/v1/queues/q/receive", `{"max":100}`)
				if st == 200 {
					var rr recvResp
					if json.Unmarshal(rb, &rr) == nil {
						for _, m := range rr.Messages {
							recv[m.Group] = append(recv[m.Group], m.Body) // it WAS delivered once
							post(p, "/v1/queues/q/visibility", `{"receipt":`+jsonEscape(m.Receipt)+`,"visibility_timeout_ms":0}`)
							anyNack = true
							gotAny = true
						}
					}
				}
			}
		}
	}

	// Heal any lingering partition so the final drain + liveness see the whole cluster; a broker
	// that was fenced while partitioned stays a live PROCESS (healthz 200) even if its actors stopped.
	for b := range paused {
		if paused[b] {
			signalBroker(b, syscall.SIGCONT)
			paused[b] = false
		}
	}
	// Heal any partitioned tier member too, so the final drain reads from a full-strength tier
	// (a SIGSTOP'd node/replica would otherwise never answer and stall reads behind it).
	for i := range pausedNode {
		if pausedNode[i] {
			signalPidFile("/n"+i+".pid", syscall.SIGCONT)
			pausedNode[i] = false
		}
	}
	for i := range pausedCtrl {
		if pausedCtrl[i] {
			signalPidFile("/c"+i+".pid", syscall.SIGCONT)
			pausedCtrl[i] = false
		}
	}

	// Drain to TRUE quiescence, using each broker's OWN queue-depth metric as ground truth instead
	// of an "N empty receives = drained" guess. After a sequencer crash the broker tailer's
	// long-poll read to the (dead, still-chain-listed) replica sits out its full deadline (~10.5 s
	// per step, because a FAILED DIAL does not raise PeerDown), so a message stays LEGITIMATELY
	// queued far longer than any fixed patience — the empty-receive heuristic gives up and cries
	// "lost". spread_queue_messages / spread_queue_in_flight settle to 0 only when the queue is
	// really empty on that pod, so drain until both are 0 on every LIVE broker (or a budget). A body
	// still missing WITH messages==0 (and applied_seq past its position) is a genuine state-machine
	// loss; messages still >0 at the budget is a HANG. (Diagnosis credit: the spread/log-native side
	// traced the stall to the tailer's failed-dial-no-PeerDown path.)
	live := []string{}
	for _, b := range []string{"1", "2"} {
		if !killed[b] {
			live = append(live, port(b))
		}
	}
	metricVal := func(body, name string) float64 {
		sum := 0.0
		for _, ln := range strings.Split(body, "\n") {
			if strings.HasPrefix(ln, name) && (len(ln) == len(name) || ln[len(name)] == ' ' || ln[len(name)] == '{') {
				fs := strings.Fields(ln)
				if len(fs) >= 2 {
					if v, err := strconv.ParseFloat(fs[len(fs)-1], 64); err == nil {
						sum += v
					}
				}
			}
		}
		return sum
	}
	getMetrics := func(p string) (string, bool) {
		req, _ := http.NewRequest("GET", "http://127.0.0.1:"+p+"/metrics", nil)
		resp, err := client.Do(req)
		if err != nil {
			return "", false
		}
		defer resp.Body.Close()
		b, _ := io.ReadAll(resp.Body)
		return string(b), true
	}
	metricsLine := func(p string) string {
		body, ok := getMetrics(p)
		if !ok {
			return "unreachable"
		}
		names := []string{"spread_queue_messages", "spread_queue_in_flight", "spread_queue_sends_total",
			"spread_queue_deletes_total", "spread_queue_expiries_total", "spread_log_applied_seq",
			"spread_log_append_retries_total", "spread_log_lag_restarts_total"}
		parts := []string{}
		for _, n := range names {
			parts = append(parts, strings.TrimPrefix(n, "spread_")+"="+fmt.Sprintf("%g", metricVal(body, n)))
		}
		return strings.Join(parts, " ")
	}
	queueEmpty := func() bool {
		if len(live) == 0 {
			return true
		}
		for _, p := range live {
			body, ok := getMetrics(p)
			if !ok {
				return false // cannot confirm empty
			}
			if metricVal(body, "spread_queue_messages") > 0.5 || metricVal(body, "spread_queue_in_flight") > 0.5 {
				return false
			}
		}
		return true
	}

	// Pre-drain snapshot: if messages>=1 here, a body "lost" by a fixed-patience drain is actually
	// still IN the state machine (a stall), not gone.
	for _, b := range []string{"1", "2"} {
		if !killed[b] {
			fmt.Printf("METRICS pre-drain b%s: %s\n", b, metricsLine(port(b)))
		}
	}

	drainedClean := false
	for i := 0; i < 800; i++ {
		if receiveDelete("8080")+receiveDelete("8081") > 0 {
			continue // made progress — keep pulling immediately
		}
		if queueEmpty() {
			drainedClean = true
			break
		}
		time.Sleep(500 * time.Millisecond) // let a stalled tailer take its next step (cheap under -icount)
	}

	for _, b := range []string{"1", "2"} {
		if !killed[b] {
			fmt.Printf("METRICS post-drain b%s: %s\n", b, metricsLine(port(b)))
		}
	}
	if !drainedClean {
		fmt.Println("DRAIN did not reach an authoritative-empty queue within budget (possible hang)")
	}

	// ---- consistency checks ----
	violations := 0
	fail := func(name, detail string) {
		violations++
		fmt.Printf("BUG-ASSERT %s %s\n", name, detail)
	}
	// no-split-brain: a received body must have been ACCEPTED, or at worst AMBIGUOUS (a timed-out /
	// 503 send that the tier committed anyway — valid at-least-once). A DEFINITIVELY rejected body
	// (clean 4xx) that is delivered is real split-brain; a body NEVER sent at all is a phantom.
	recvSet := map[string]int{}
	for _, g := range sortedKeys(recv) {
		for _, body := range recv[g] {
			recvSet[body]++
			if !acceptedSet[body] {
				if rejectedSet[body] {
					fail("split-brain", fmt.Sprintf("group=%s body=%q was definitively rejected (4xx) but delivered", g, body))
				} else if !ambiguousSet[body] {
					fail("phantom-message", fmt.Sprintf("group=%s body=%q delivered but never sent", g, body))
				}
			}
		}
	}
	// at-least-once: every accepted body is delivered at least once.
	lost := []string{}
	for body := range acceptedSet {
		if recvSet[body] == 0 {
			lost = append(lost, body)
		}
	}
	sort.Strings(lost)
	for _, body := range lost {
		fail("lost-message", fmt.Sprintf("body=%q accepted but never delivered", body))
	}
	// fifo: per group, first-delivery order matches accepted order (drop redeliveries). Only
	// ACCEPTED bodies have a defined position — an ambiguous send (timed-out/503 that committed)
	// has no client-known order, so it is skipped here rather than counted as a reorder.
	for _, g := range sortedKeys(accepted) {
		acc := accepted[g]
		seen := map[string]bool{}
		firsts := []string{}
		for _, body := range recv[g] {
			if !acceptedSet[body] {
				continue // ambiguous/redelivered-unknown: no defined FIFO position
			}
			if !seen[body] {
				seen[body] = true
				firsts = append(firsts, body)
			}
		}
		// firsts must be a prefix of acc (same order); anything delivered must be in order.
		n := len(firsts)
		if n > len(acc) {
			n = len(acc)
		}
		for i := 0; i < n; i++ {
			if firsts[i] != acc[i] {
				fail("fifo-reorder", fmt.Sprintf("group=%s pos=%d got=%q want=%q", g, i, firsts[i], acc[i]))
				break
			}
		}
	}

	// broker-liveness: any broker we did NOT crash must still answer /healthz — an unexpected
	// death (a request that killed a broker) is a crash bug, distinct from an intentional X.
	for _, b := range []string{"1", "2"} {
		if killed[b] {
			continue
		}
		alive := false
		if req, err := http.NewRequest("GET", "http://127.0.0.1:"+port(b)+"/healthz", nil); err == nil {
			if resp, e := client.Do(req); e == nil {
				resp.Body.Close()
				alive = resp.StatusCode == 200
			}
		}
		if !alive {
			fail("broker-died", fmt.Sprintf("broker %s died unexpectedly (not crashed by the program)", b))
		}
	}

	// reachability markers (property SDK): did the fuzz actually exercise the interesting states?
	rpt := func(name string, cond bool) {
		fmt.Printf("RPT decl %s\n", name)
		if cond {
			fmt.Printf("RPT sat %s\n", name)
		}
	}
	rpt("send-accepted", anyAccept)
	rpt("send-fenced", anyFenced)
	rpt("queue-drained", gotAny)
	rpt("send-concurrent", anyConc)
	rpt("broker-crashed", len(killed) > 0)
	rpt("broker-paused", anyPause)
	rpt("msg-nacked", anyNack)
	rpt("node-crashed", anyNodeKill)     // a log node was SIGKILLed -> tier resealed onto survivors
	rpt("control-crashed", anyCtrlKill)  // a control replica was SIGKILLed -> VSR tolerated/failed over
	rpt("node-paused", anyNodePause)     // a log node was partitioned off (silent)
	rpt("control-paused", anyCtrlPause)  // a control replica was partitioned off (silent)

	if violations == 0 {
		fmt.Println("spread: consistent")
	}
}
