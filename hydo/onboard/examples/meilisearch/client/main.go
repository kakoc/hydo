// mlc is the test client of the Meilisearch example: the workload's operations and the checks.
// It speaks Meilisearch's HTTP API with Go's standard library only.
//
//	mlc ready             exit 0 when the server answers /health
//	mlc init              the indexes, their settings, the first documents (first_init)
//	mlc op <kind>         enqueue one task; the composer's HYDO_* variables drive it
//	mlc check <rule>      one check over the client's records and the server's state
//	mlc readback          the final: of the messages check: each acknowledged task's final state
//	mlc get <path>        print one GET answer (for a look by hand)
//
// The server is MS_URL (default http://127.0.0.1:7700). MS_DATA (default /data/vol/meili) is the
// server's /meili_data as the guest's root sees it: the checks read the dumps and the snapshot
// there.
//
// The indexes:
//
//	docs     documents c<cmd>_<i> {id, grp: g<cmd%4>, v, body: "alpha w<cmd>"}; filterable grp, v
//	sa, sb   one document each, {id: "m", home: "sa"} and {id: "m", home: "sb"}: index swaps
//	cnt      one document {id: "k", n: 0}: edits by function add 1 to n
//
// Outcomes. An answer 202 with a taskUid is acknowledged: the task is enqueued, and Meilisearch
// "is either processing [it] or will do so in the future". Any other answer is failed (the
// request was refused). No answer is unknown: the task may be enqueued.
//
// $HYDO_ACKED gets one line per acknowledged task, t<uid>:<kind>:<args>:
//
//	t<uid>:add:<cmd>:<n>            documents c<cmd>_0 .. c<cmd>_<n-1> added (POST, replace)
//	t<uid>:upd:<add>:<n>:<v>        v set to <v> on c<add>_0 .. c<add>_<n-1> (PUT, partial update)
//	t<uid>:del:<add>:<k>            c<add>_<k> deleted
//	t<uid>:set:<cmd>                docs's filterable attributes set to grp, v, f<cmd>
//	t<uid>:swap                     sa and sb swapped
//	t<uid>:edit                     cnt's k gets n + 1 (an edit by function)
//	t<uid>:dump, t<uid>:snap        a dump, a snapshot
//
// and the line m<uid>:<oid>, the task's request id below.
//
// $HYDO_OUTCOMES gets <oid> ack|failed|unknown, where the request id <oid> is
// <kind>.<cmd>[.<target>]: a check does not judge a document that an unknown request may have
// changed.
//
// The history tasks (the messages check of hydo.yaml, hydo guide messages): each request is a
// send of its request id to the channel tasks, with the task's uid as its system id, and
// `mlc readback`, the check's final:, reads back each acknowledged task's final state.
package main

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

var (
	base   = envDef("MS_URL", "http://127.0.0.1:7700")
	data   = envDef("MS_DATA", "/data/vol/meili")
	client = &http.Client{Timeout: 10 * time.Second}
)

func envDef(k, d string) string {
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

func trunc(s string, n int) string {
	if len(s) > n {
		return s[:n] + "..."
	}
	return s
}

// call sends one request with a JSON body (nil for none) and returns the status and the body.
func call(method, path string, body any) (int, []byte, error) {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return 0, nil, err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequest(method, base+path, rd)
	if err != nil {
		return 0, nil, err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := client.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return resp.StatusCode, nil, err
	}
	return resp.StatusCode, b, nil
}

func getJSON(path string, out any) error {
	st, b, err := call("GET", path, nil)
	if err != nil {
		return err
	}
	if st != 200 {
		return fmt.Errorf("GET %s: %d %s", path, st, trunc(string(b), 200))
	}
	return json.Unmarshal(b, out)
}

func postJSON(path string, body, out any) error {
	st, b, err := call("POST", path, body)
	if err != nil {
		return err
	}
	if st != 200 {
		return fmt.Errorf("POST %s: %d %s", path, st, trunc(string(b), 200))
	}
	return json.Unmarshal(b, out)
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

// sometimes writes an SDK assertion record to the console: the event of a reaction (A=<id>:cut).
func sometimes(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	fmt.Fprintf(f, "HYDO-ASSERT %s\n", b)
}

// uptimeNs is the guest's uptime in ns, as the history records write it.
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

// hrec writes one record of the history tasks in one write: HYDO-HIST tasks <process> <type>
// <time> <args>. The records go to the console, or to HYDO_HIST_OUT ("off": nowhere).
func hrec(proc int, typ string, args ...string) {
	out := envDef("HYDO_HIST_OUT", "/dev/console")
	if out == "off" {
		return
	}
	f, err := os.OpenFile(out, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	f.WriteString(fmt.Sprintf("HYDO-HIST tasks %d %s %s %s\n", proc, typ, uptimeNs(), strings.Join(args, " ")))
}

func cmdID() int { return atoiDef(os.Getenv("HYDO_CMD_ID"), 0) }

func rng() *rand.Rand {
	s := uint64(atoiDef(os.Getenv("HYDO_SEED"), 0))
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

// enqueue sends one task request. It records the outcome under oid and, when acknowledged, the
// line t<uid>:<rec> in $HYDO_ACKED. It returns the task's uid, or -1.
func enqueue(oid, rec, method, path string, body any) int {
	hrec(cmdID(), "invoke", "send", "tasks", oid)
	st, b, err := call(method, path, body)
	outcomes := os.Getenv("HYDO_OUTCOMES")
	if err != nil {
		appendLine(outcomes, oid+" unknown")
		hrec(cmdID(), "info", "send", "tasks", oid)
		fmt.Fprintf(os.Stderr, "mlc: %s %s: no answer: %v\n", method, path, err)
		return -1
	}
	var t struct {
		TaskUID *int `json:"taskUid"`
	}
	if st != 202 || json.Unmarshal(b, &t) != nil || t.TaskUID == nil {
		appendLine(outcomes, oid+" failed")
		hrec(cmdID(), "fail", "send", "tasks", oid)
		fmt.Fprintf(os.Stderr, "mlc: %s %s: %d %s\n", method, path, st, trunc(string(b), 300))
		return -1
	}
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("t%d:%s", *t.TaskUID, rec))
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("m%d:%s", *t.TaskUID, oid))
	appendLine(outcomes, oid+" ack")
	hrec(cmdID(), "ok", "send", "tasks", fmt.Sprintf("%s;o=%d", oid, *t.TaskUID))
	fmt.Printf("mlc: task %d enqueued: %s\n", *t.TaskUID, rec)
	return *t.TaskUID
}

// ---- tasks ----

type taskError struct {
	Message string `json:"message"`
	Code    string `json:"code"`
	Type    string `json:"type"`
}

type task struct {
	UID        int                        `json:"uid"`
	BatchUID   *int                       `json:"batchUid"`
	IndexUID   *string                    `json:"indexUid"`
	Status     string                     `json:"status"`
	Type       string                     `json:"type"`
	Details    map[string]json.RawMessage `json:"details"`
	Error      *taskError                 `json:"error"`
	EnqueuedAt string                     `json:"enqueuedAt"`
	StartedAt  *string                    `json:"startedAt"`
	FinishedAt *string                    `json:"finishedAt"`
}

func (t task) index() string {
	if t.IndexUID == nil {
		return ""
	}
	return *t.IndexUID
}

// times prints when the task was enqueued, started and finished, and its batch.
func (t task) times() string {
	str := func(p *string) string {
		if p == nil {
			return "-"
		}
		return *p
	}
	b := -1
	if t.BatchUID != nil {
		b = *t.BatchUID
	}
	return fmt.Sprintf("batch %d, enqueued %s, started %s, finished %s", b, t.EnqueuedAt, str(t.StartedAt), str(t.FinishedAt))
}

func (t task) detailInt(k string) int {
	var n int
	if v, ok := t.Details[k]; ok {
		json.Unmarshal(v, &n)
	}
	return n
}

// allTasks lists every task of the queue, by uid.
func allTasks() (map[int]task, error) {
	out := map[int]task{}
	from := -1
	for {
		path := "/tasks?limit=1000"
		if from >= 0 {
			path += "&from=" + strconv.Itoa(from)
		}
		var page struct {
			Results []task `json:"results"`
			Next    *int   `json:"next"`
		}
		if err := getJSON(path, &page); err != nil {
			return nil, err
		}
		for _, t := range page.Results {
			out[t.UID] = t
		}
		if page.Next == nil {
			return out, nil
		}
		from = *page.Next
	}
}

// pending is the number of enqueued and processing tasks.
func pending() (int, error) {
	var p struct {
		Total int `json:"total"`
	}
	err := getJSON("/tasks?statuses=enqueued,processing&limit=1", &p)
	return p.Total, err
}

// drain waits until the queue holds no enqueued or processing task (at most d). It returns false
// when the queue did not drain or the server did not answer.
func drain(d time.Duration) bool {
	end := time.Now().Add(d)
	last := -1
	for {
		n, err := pending()
		if err == nil && n == 0 {
			return true
		}
		if err == nil {
			last = n
		}
		if time.Now().After(end) {
			if err != nil {
				fmt.Println("the server did not answer:", err)
			} else {
				fmt.Printf("%d tasks still enqueued or processing after %s\n", last, d)
			}
			return false
		}
		time.Sleep(500 * time.Millisecond)
	}
}

func waitTask(uid int, d time.Duration) (task, bool) {
	end := time.Now().Add(d)
	for {
		var t task
		if err := getJSON("/tasks/"+strconv.Itoa(uid), &t); err == nil {
			if t.Status == "succeeded" || t.Status == "failed" || t.Status == "canceled" {
				return t, true
			}
		}
		if time.Now().After(end) {
			return task{}, false
		}
		time.Sleep(200 * time.Millisecond)
	}
}

// ---- init ----

func cmdInit() int {
	steps := []struct {
		method, path string
		body         any
	}{
		{"PATCH", "/experimental-features", map[string]any{"editDocumentsByFunction": true}},
		{"POST", "/indexes", map[string]any{"uid": "docs", "primaryKey": "id"}},
		{"POST", "/indexes", map[string]any{"uid": "sa", "primaryKey": "id"}},
		{"POST", "/indexes", map[string]any{"uid": "sb", "primaryKey": "id"}},
		{"POST", "/indexes", map[string]any{"uid": "cnt", "primaryKey": "id"}},
		{"PATCH", "/indexes/docs/settings", map[string]any{"filterableAttributes": []string{"grp", "v"}}},
		{"PATCH", "/indexes/cnt/settings", map[string]any{"filterableAttributes": []string{"id"}}},
		{"POST", "/indexes/sa/documents", []map[string]any{{"id": "m", "home": "sa"}}},
		{"POST", "/indexes/sb/documents", []map[string]any{{"id": "m", "home": "sb"}}},
		{"POST", "/indexes/cnt/documents", []map[string]any{{"id": "k", "n": 0}}},
	}
	for try := 0; try < 60; try++ {
		if st, _, err := call("GET", "/health", nil); err == nil && st == 200 {
			break
		}
		time.Sleep(time.Second)
	}
	for _, s := range steps {
		st, b, err := call(s.method, s.path, s.body)
		if err != nil || (st != 200 && st != 202) {
			fmt.Fprintf(os.Stderr, "mlc: init %s %s: %d %v %s\n", s.method, s.path, st, err, trunc(string(b), 300))
			return 1
		}
		var t struct {
			TaskUID *int `json:"taskUid"`
		}
		if json.Unmarshal(b, &t) == nil && t.TaskUID != nil {
			done, ok := waitTask(*t.TaskUID, 60*time.Second)
			if !ok || done.Status != "succeeded" {
				fmt.Fprintf(os.Stderr, "mlc: init task %d: %s %v\n", *t.TaskUID, done.Status, done.Error)
				return 1
			}
		}
	}
	fmt.Println("mlc: init done")
	return 0
}

// ---- operations ----

func doc(cmd, i int) map[string]any {
	return map[string]any{"id": fmt.Sprintf("c%d_%d", cmd, i), "grp": fmt.Sprintf("g%d", cmd%4), "v": 0,
		"body": fmt.Sprintf("alpha w%d", cmd)}
}

// ackedAdds is every acknowledged addition of earlier commands: cmd -> n.
func ackedAdds() (cmds []int, n map[int]int) {
	n = map[int]int{}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, ":")
		if len(f) == 4 && f[1] == "add" {
			c := atoiDef(f[2], -1)
			if _, ok := n[c]; !ok {
				cmds = append(cmds, c)
			}
			n[c] = atoiDef(f[3], 0)
		}
	}
	return cmds, n
}

func opAdd() int {
	r := rng()
	id := cmdID()
	n := 1 + r.IntN(20)
	docs := make([]map[string]any, n)
	for i := range docs {
		docs[i] = doc(id, i)
	}
	uid := enqueue(fmt.Sprintf("add.%d", id), fmt.Sprintf("add:%d:%d", id, n), "POST", "/indexes/docs/documents", docs)
	if uid < 0 {
		return 1
	}
	sometimes("meili.add_acked", fmt.Sprintf("task %d", uid))
	return 0
}

func opUpd() int {
	r := rng()
	id := cmdID()
	cmds, n := ackedAdds()
	if len(cmds) == 0 {
		return opAdd()
	}
	a := cmds[r.IntN(len(cmds))]
	docs := make([]map[string]any, n[a])
	for i := range docs {
		docs[i] = map[string]any{"id": fmt.Sprintf("c%d_%d", a, i), "v": id}
	}
	if enqueue(fmt.Sprintf("upd.%d.%d", id, a), fmt.Sprintf("upd:%d:%d:%d", a, n[a], id), "PUT", "/indexes/docs/documents", docs) < 0 {
		return 1
	}
	return 0
}

func opDel() int {
	r := rng()
	id := cmdID()
	cmds, n := ackedAdds()
	if len(cmds) == 0 {
		return opAdd()
	}
	a := cmds[r.IntN(len(cmds))]
	k := r.IntN(n[a])
	if enqueue(fmt.Sprintf("del.%d.%d.%d", id, a, k), fmt.Sprintf("del:%d:%d", a, k), "POST", "/indexes/docs/documents/delete-batch",
		[]string{fmt.Sprintf("c%d_%d", a, k)}) < 0 {
		return 1
	}
	return 0
}

func opSet() int {
	id := cmdID()
	if enqueue(fmt.Sprintf("set.%d", id), fmt.Sprintf("set:%d", id), "PATCH", "/indexes/docs/settings",
		map[string]any{"filterableAttributes": []string{"grp", "v", fmt.Sprintf("f%d", id)}}) < 0 {
		return 1
	}
	return 0
}

func opSwap() int {
	id := cmdID()
	uid := enqueue(fmt.Sprintf("swap.%d", id), "swap", "POST", "/swap-indexes",
		[]map[string]any{{"indexes": []string{"sa", "sb"}}})
	if uid < 0 {
		return 1
	}
	sometimes("meili.swap_acked", fmt.Sprintf("task %d", uid))
	return 0
}

func opEdit() int {
	id := cmdID()
	uid := enqueue(fmt.Sprintf("edit.%d", id), "edit", "POST", "/indexes/cnt/documents/edit",
		map[string]any{"function": "doc.n = doc.n + 1", "filter": "id = k"})
	if uid < 0 {
		return 1
	}
	sometimes("meili.edit_acked", fmt.Sprintf("task %d", uid))
	return 0
}

// opDump and opSnap enqueue a dump or a snapshot in one call of four; the other calls add
// documents, so the commands keep their weight in the composition. With MLC_DUMPS=no every
// call adds documents: every power cut empties the dumps (RESULTS.md, finding 3), and its
// verdict would hide the other findings of an exploration.
func opDump() int {
	if os.Getenv("MLC_DUMPS") == "no" || rng().IntN(4) != 0 {
		return opAdd()
	}
	uid := enqueue(fmt.Sprintf("dump.%d", cmdID()), "dump", "POST", "/dumps", nil)
	if uid < 0 {
		return 1
	}
	sometimes("meili.dump_acked", fmt.Sprintf("task %d", uid))
	return 0
}

func opSnap() int {
	if os.Getenv("MLC_DUMPS") == "no" || rng().IntN(4) != 0 {
		return opAdd()
	}
	uid := enqueue(fmt.Sprintf("snap.%d", cmdID()), "snap", "POST", "/snapshots", nil)
	if uid < 0 {
		return 1
	}
	sometimes("meili.snap_acked", fmt.Sprintf("task %d", uid))
	return 0
}

// ---- the client's records ----

type rec struct {
	uid  int
	kind string
	args []string
}

func records() []rec {
	var out []rec
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, ":")
		if len(f) < 2 || !strings.HasPrefix(f[0], "t") {
			continue
		}
		out = append(out, rec{atoiDef(f[0][1:], -1), f[1], f[2:]})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].uid < out[j].uid })
	return out
}

// outcomes is the last outcome of each id in $HYDO_OUTCOMES.
func outcomes() map[string]string {
	out := map[string]string{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		if f := strings.Fields(l); len(f) == 2 {
			out[f[0]] = f[1]
		}
	}
	return out
}

var taskType = map[string]string{
	"add": "documentAdditionOrUpdate", "upd": "documentAdditionOrUpdate", "del": "documentDeletion",
	"set": "settingsUpdate", "swap": "indexSwap", "edit": "documentEdition",
	"dump": "dumpCreation", "snap": "snapshotCreation",
}

// ---- checks ----

// prepare waits for the queue to drain and lists every task. A check exits 1 when the queue
// does not drain: an acknowledged task that never ends breaks the promise as much as a lost one.
func prepare() (map[int]task, int) {
	if !drain(90 * time.Second) {
		return nil, 1
	}
	ts, err := allTasks()
	if err != nil {
		fmt.Println("cannot list the tasks:", err)
		return nil, 2
	}
	return ts, 0
}

// checkTasks: every acknowledged task is in the queue with its own type and ends succeeded or
// failed, never canceled (nobody cancels). A failed task says why. The tasks of one index ran in
// the order of their uids (their batches' uids do not go back). A succeeded addition or update
// received and indexed every document that the client sent.
func checkTasks() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	bad := 0
	for _, r := range records() {
		t, ok := ts[r.uid]
		switch {
		case !ok:
			bad++
			fmt.Printf("lost: task %d (%s) was acknowledged and is not in the queue\n", r.uid, r.kind)
			continue
		case t.Type != taskType[r.kind]:
			bad++
			fmt.Printf("wrong: task %d was acknowledged as %s and is a %s\n", r.uid, r.kind, t.Type)
			continue
		}
		switch t.Status {
		case "succeeded":
		case "failed":
			bad++
			msg := ""
			if t.Error != nil {
				msg = fmt.Sprintf("%s (%s, %s)", t.Error.Message, t.Error.Code, t.Error.Type)
			}
			fmt.Printf("failed: task %d (%s) failed: %s; %s\n", r.uid, r.kind, trunc(msg, 300), t.times())
			continue
		default:
			bad++
			fmt.Printf("status: task %d (%s) is %s\n", r.uid, r.kind, t.Status)
			continue
		}
		if r.kind == "add" || r.kind == "upd" {
			want := atoiDef(r.args[1], -1)
			if got, idx := t.detailInt("receivedDocuments"), t.detailInt("indexedDocuments"); got != want || idx != want {
				bad++
				fmt.Printf("short: task %d (%s of %d documents) succeeded with receivedDocuments %d and indexedDocuments %d; %s\n",
					r.uid, r.kind, want, got, idx, t.times())
			}
		}
	}
	// the order of the tasks of each index
	byIndex := map[string][]task{}
	for _, t := range ts {
		if t.index() != "" && t.BatchUID != nil && (t.Status == "succeeded" || t.Status == "failed") {
			byIndex[t.index()] = append(byIndex[t.index()], t)
		}
	}
	for ix, l := range byIndex {
		sort.Slice(l, func(i, j int) bool { return l[i].UID < l[j].UID })
		for i := 1; i < len(l); i++ {
			if *l[i].BatchUID < *l[i-1].BatchUID {
				bad++
				fmt.Printf("order: on index %s task %d ran in batch %d, after task %d in batch %d\n",
					ix, l[i].UID, *l[i].BatchUID, l[i-1].UID, *l[i-1].BatchUID)
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d broken promises over %d acknowledged tasks\n", bad, len(records()))
		return 1
	}
	fmt.Printf("%d acknowledged tasks, each ended succeeded with all its documents, in order (%d tasks in the queue)\n",
		len(records()), len(ts))
	return 0
}

type stored struct {
	ID   string `json:"id"`
	Grp  string `json:"grp"`
	V    *int   `json:"v"`
	Body string `json:"body"`
}

func fetchDocs(index string) (map[string]stored, error) {
	out := map[string]stored{}
	for off := 0; ; off += 1000 {
		var page struct {
			Results []stored `json:"results"`
			Total   int      `json:"total"`
		}
		if err := postJSON("/indexes/"+index+"/documents/fetch", map[string]any{"offset": off, "limit": 1000}, &page); err != nil {
			return nil, err
		}
		for _, d := range page.Results {
			out[d.ID] = d
		}
		if off+1000 >= page.Total {
			return out, nil
		}
	}
}

func searchHits(index string, q map[string]any) (int, error) {
	q["hitsPerPage"] = 1
	q["page"] = 1
	var a struct {
		TotalHits int `json:"totalHits"`
	}
	err := postJSON("/indexes/"+index+"/search", q, &a)
	return a.TotalHits, err
}

// checkDocs: the documents of index docs are what the succeeded tasks that the client saw
// acknowledged made of them, applied in the order of the tasks' uids: every added document is
// there with its group and the value of its last update, every deleted one is gone. A document
// that an unanswered request may have changed is not judged. Then the search agrees with the
// stored documents: a filter on each group and a word of each judged addition find exactly the
// stored documents that have them.
func checkDocs() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	type want struct {
		exists bool
		grp    string
		v      int
	}
	model := map[string]*want{}
	get := func(id string) *want {
		if model[id] == nil {
			model[id] = &want{}
		}
		return model[id]
	}
	uncertain := map[string]bool{}
	for k, o := range outcomes() {
		if o != "unknown" {
			continue
		}
		f := strings.Split(k, ".")
		switch {
		case f[0] == "add" && len(f) == 2:
			uncertain["add"+f[1]] = true // its n is not known: every c<cmd>_* is skipped
		case f[0] == "upd" && len(f) == 3:
			uncertain["add"+f[2]] = true
		case f[0] == "del" && len(f) == 4:
			uncertain["c"+f[2]+"_"+f[3]] = true
		}
	}
	judged := map[int]bool{}
	for _, r := range records() {
		t, ok := ts[r.uid]
		if !ok || t.Status != "succeeded" {
			continue
		}
		switch r.kind {
		case "add":
			c, n := atoiDef(r.args[0], -1), atoiDef(r.args[1], 0)
			judged[c] = true
			for i := 0; i < n; i++ {
				w := get(fmt.Sprintf("c%d_%d", c, i))
				*w = want{true, fmt.Sprintf("g%d", c%4), 0}
			}
		case "upd":
			a, n, v := atoiDef(r.args[0], -1), atoiDef(r.args[1], 0), atoiDef(r.args[2], -1)
			for i := 0; i < n; i++ {
				w := get(fmt.Sprintf("c%d_%d", a, i))
				if !w.exists { // a partial update of a missing document creates it with v only
					w.grp = ""
				}
				w.exists, w.v = true, v
			}
		case "del":
			get(fmt.Sprintf("c%s_%s", r.args[0], r.args[1])).exists = false
		}
	}
	have, err := fetchDocs("docs")
	if err != nil {
		fmt.Println("cannot read the documents:", err)
		return 2
	}
	lost, wrong, kept, n := 0, 0, 0, 0
	var lostIDs []string
	for id, w := range model {
		cmd := strings.SplitN(strings.TrimPrefix(id, "c"), "_", 2)[0]
		if uncertain[id] || uncertain["add"+cmd] {
			continue
		}
		n++
		d, ok := have[id]
		switch {
		case w.exists && !ok:
			lost++
			lostIDs = append(lostIDs, id)
		case !w.exists && ok:
			kept++
			fmt.Printf("kept: %s was deleted by a succeeded task and is there\n", id)
		case w.exists && (d.V == nil || *d.V != w.v || d.Grp != w.grp):
			wrong++
			got := -1
			if d.V != nil {
				got = *d.V
			}
			fmt.Printf("wrong: %s holds grp=%q v=%d, the succeeded tasks give grp=%q v=%d\n", id, d.Grp, got, w.grp, w.v)
		}
	}
	sort.Strings(lostIDs)
	if lost > 0 {
		fmt.Printf("lost: %d documents of succeeded tasks are missing: %s\n", lost, trunc(strings.Join(lostIDs, " "), 600))
	}
	// the search against the stored documents
	mism := 0
	for g := 0; g < 4; g++ {
		grp := fmt.Sprintf("g%d", g)
		want := 0
		for _, d := range have {
			if d.Grp == grp {
				want++
			}
		}
		got, err := searchHits("docs", map[string]any{"q": "", "filter": fmt.Sprintf("grp = %s", grp)})
		if err != nil {
			fmt.Println("cannot search:", err)
			return 2
		}
		if got != want {
			mism++
			fmt.Printf("search: filter grp = %s finds %d documents, %d are stored\n", grp, got, want)
		}
	}
	var cmds []int
	for c := range judged {
		cmds = append(cmds, c)
	}
	sort.Ints(cmds)
	for i, c := range cmds {
		if i%3 != 0 { // a third of the additions: one search each
			continue
		}
		word := fmt.Sprintf("w%d", c)
		want := 0
		for _, d := range have {
			if strings.HasSuffix(d.Body, " "+word) {
				want++
			}
		}
		// a phrase: exact words only, no prefix (w3 would find w30) and no typo
		got, err := searchHits("docs", map[string]any{"q": `"` + word + `"`})
		if err != nil {
			fmt.Println("cannot search:", err)
			return 2
		}
		if got != want {
			mism++
			fmt.Printf("search: q=%s finds %d documents, %d stored documents have it\n", word, got, want)
		}
	}
	if lost+wrong+kept+mism > 0 {
		fmt.Printf("of %d judged documents %d lost, %d wrong, %d kept after deletion; %d searches disagree\n", n, lost, wrong, kept, mism)
		return 1
	}
	fmt.Printf("%d judged documents as the succeeded tasks left them (%d stored), and the searches agree\n", n, len(have))
	return 0
}

// checkSettings: docs's filterable attributes are those of the succeeded settings task with the
// highest uid (the queue's own record of what each task set).
func checkSettings() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	last := -1
	for uid, t := range ts {
		if t.index() == "docs" && t.Type == "settingsUpdate" && t.Status == "succeeded" {
			if _, ok := t.Details["filterableAttributes"]; ok && uid > last {
				last = uid
			}
		}
	}
	if last < 0 {
		fmt.Println("no settings task")
		return 0
	}
	var want []string
	json.Unmarshal(ts[last].Details["filterableAttributes"], &want)
	var got []string
	if err := getJSON("/indexes/docs/settings/filterable-attributes", &got); err != nil {
		fmt.Println("cannot read the settings:", err)
		return 2
	}
	sort.Strings(want)
	sort.Strings(got)
	if strings.Join(want, ",") != strings.Join(got, ",") {
		fmt.Printf("settings: docs's filterable attributes are %v, the last succeeded settings task %d set %v\n", got, last, want)
		return 1
	}
	fmt.Printf("docs's filterable attributes %v are those of task %d\n", got, last)
	return 0
}

// checkSwap: sa and sb each hold their one document, and an even number of succeeded swaps
// leaves each at home ("Swapping indexes is an atomic transaction").
func checkSwap() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	n := 0
	for _, t := range ts {
		if t.Type == "indexSwap" && t.Status == "succeeded" {
			n++
		}
	}
	bad := 0
	for _, ix := range []string{"sa", "sb"} {
		docs, err := fetchDocs(ix)
		if err != nil {
			fmt.Println("cannot read", ix, err)
			return 2
		}
		home := ix
		if n%2 == 1 {
			home = map[string]string{"sa": "sb", "sb": "sa"}[ix]
		}
		var got []string
		for _, d := range docs {
			got = append(got, d.ID)
		}
		var h struct {
			Home string `json:"home"`
		}
		_, b, _ := call("GET", "/indexes/"+ix+"/documents/m", nil)
		json.Unmarshal(b, &h)
		if len(docs) != 1 || h.Home != home {
			bad++
			fmt.Printf("swap: index %s holds %v with home %q after %d succeeded swaps; want one document with home %q\n", ix, got, h.Home, n, home)
		}
	}
	if bad > 0 {
		printLast(ts, "indexSwap", 3)
		fmt.Printf("sa and sb are swapped otherwise than %d succeeded swaps say\n", n)
		return 1
	}
	fmt.Printf("%d succeeded swaps, sa and sb as they should be\n", n)
	return 0
}

// printLast prints the times of the last k tasks of a type, the newest last.
func printLast(ts map[int]task, typ string, k int) {
	var uids []int
	for uid, t := range ts {
		if t.Type == typ {
			uids = append(uids, uid)
		}
	}
	sort.Ints(uids)
	if len(uids) > k {
		uids = uids[len(uids)-k:]
	}
	for _, uid := range uids {
		fmt.Printf("%s task %d %s: %s\n", typ, uid, ts[uid].Status, ts[uid].times())
	}
}

// checkEdits: cnt's k counts the succeeded edits by function, each applied once.
func checkEdits() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	n := 0
	for _, t := range ts {
		if t.Type == "documentEdition" && t.Status == "succeeded" && t.detailInt("editedDocuments") > 0 {
			n++
		}
	}
	var d struct {
		N int `json:"n"`
	}
	if err := getJSON("/indexes/cnt/documents/k", &d); err != nil {
		fmt.Println("cannot read cnt's k:", err)
		return 2
	}
	if d.N != n {
		printLast(ts, "documentEdition", 3)
		fmt.Printf("edits: cnt's k has n=%d after %d succeeded edits that each add 1\n", d.N, n)
		return 1
	}
	fmt.Printf("cnt's k has n=%d, one per succeeded edit\n", d.N)
	return 0
}

// archive opens a tar.gz file and returns the names of its entries, reading every byte.
func archive(path string) ([]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	z, err := gzip.NewReader(f)
	if err != nil {
		return nil, err
	}
	tr := tar.NewReader(z)
	var names []string
	for {
		h, err := tr.Next()
		if errors.Is(err, io.EOF) {
			return names, nil
		}
		if err != nil {
			return names, err
		}
		if _, err := io.Copy(io.Discard, tr); err != nil {
			return names, err
		}
		names = append(names, h.Name)
	}
}

func hasEntry(names []string, suffix string) bool {
	for _, n := range names {
		if strings.HasSuffix(strings.TrimSuffix(n, "/"), suffix) {
			return true
		}
	}
	return false
}

// checkDumps: the file of every succeeded dump task is a whole archive with the dump's metadata
// and index docs, and the snapshot of a succeeded snapshot task is a whole archive with the task
// queue's database.
func checkDumps() int {
	ts, rc := prepare()
	if rc != 0 {
		return rc
	}
	bad, dumps, snaps := 0, 0, 0
	var uids []int
	for uid := range ts {
		uids = append(uids, uid)
	}
	sort.Ints(uids)
	for _, uid := range uids {
		t := ts[uid]
		if t.Status != "succeeded" {
			continue
		}
		switch t.Type {
		case "dumpCreation":
			dumps++
			var du string
			json.Unmarshal(t.Details["dumpUid"], &du)
			p := filepath.Join(data, "dumps", du+".dump")
			names, err := archive(p)
			if err != nil || !hasEntry(names, "metadata.json") || !hasEntry(names, "indexes/docs") {
				bad++
				st, serr := os.Stat(p)
				size := int64(-1)
				if serr == nil {
					size = st.Size()
				}
				fmt.Printf("dump: task %d succeeded with dump %s; the file has %d bytes, %d entries: %v\n", uid, du, size, len(names), err)
			}
		case "snapshotCreation":
			snaps++
		}
	}
	if snaps > 0 {
		p := filepath.Join(data, "snapshots", "data.ms.snapshot")
		names, err := archive(p)
		if err != nil || !hasEntry(names, "tasks/data.mdb") {
			bad++
			st, serr := os.Stat(p)
			size := int64(-1)
			if serr == nil {
				size = st.Size()
			}
			fmt.Printf("snapshot: %d snapshot tasks succeeded; the snapshot has %d bytes, %d entries: %v\n", snaps, size, len(names), err)
		}
	}
	if bad > 0 {
		return 1
	}
	fmt.Printf("%d dumps and the snapshot of %d snapshot tasks are whole archives\n", dumps, snaps)
	return 0
}

// ---- the read-back of the messages check tasks ----

// enc encodes a value of a history item: %, a space, and the characters the records use.
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

// effect is the f= of a task whose effect the read-back reads, or "" for one it does not read.
// An addition's effect is there when every document it sent is stored with the addition's own
// body. A document that a deletion request may have removed leaves the effect unread.
func effect(r rec, docs map[string]stored, derr error, mayDel map[string]bool) string {
	if r.kind != "add" || derr != nil || len(r.args) < 2 {
		return ""
	}
	for i := 0; i < atoiDef(r.args[1], 0); i++ {
		id := fmt.Sprintf("c%s_%d", r.args[0], i)
		if d, ok := docs[id]; !ok || d.Body != "alpha w"+r.args[0] {
			if mayDel[id] {
				return ""
			}
			return "0"
		}
	}
	return "1"
}

// readBack is the final: of the messages check tasks. It waits up to 90 s for the queue to
// drain, then writes a status record of every acknowledged task: s=ok (succeeded), s=fail with
// the error's code (failed, or canceled), or s=info (still enqueued or processing, or not in
// the queue), with f= where effect reads it. It exits 1 only when it cannot list the tasks.
func readBack() int {
	const proc = 100000
	if !drain(90 * time.Second) {
		fmt.Println("the queue did not drain: its pending tasks are read back as they are")
	}
	ts, err := allTasks()
	if err != nil {
		fmt.Println("cannot list the tasks:", err)
		return 1
	}
	docs, derr := fetchDocs("docs")
	mayDel := map[string]bool{}
	for oid, o := range outcomes() {
		if f := strings.Split(oid, "."); f[0] == "del" && len(f) == 4 && o != "failed" {
			mayDel[fmt.Sprintf("c%s_%s", f[2], f[3])] = true
		}
	}
	recs := map[int]rec{}
	for _, r := range records() {
		recs[r.uid] = r
	}
	var ids, items []string
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.SplitN(l, ":", 2)
		if len(f) != 2 || !strings.HasPrefix(f[0], "m") {
			continue
		}
		uid := atoiDef(f[0][1:], -1)
		it := fmt.Sprintf("%s;o=%d", f[1], uid)
		t, ok := ts[uid]
		switch {
		case !ok:
			it += ";s=info;e=" + enc("not in the queue")
		case t.Status == "succeeded":
			it += ";s=ok"
		case t.Status == "failed" || t.Status == "canceled":
			code := t.Status
			if t.Error != nil && t.Error.Code != "" {
				code = t.Error.Code
			}
			it += ";s=fail;e=" + enc(code)
		default:
			it += ";s=info;e=" + enc(t.Status)
		}
		if e := effect(recs[uid], docs, derr, mayDel); ok && e != "" {
			it += ";f=" + e
		}
		ids, items = append(ids, f[1]), append(items, it)
	}
	// 20 tasks per record, so that no console line grows long
	for i := 0; i < len(items); i += 20 {
		j := min(i+20, len(items))
		hrec(proc, "invoke", append([]string{"status", "tasks"}, ids[i:j]...)...)
		hrec(proc, "ok", append([]string{"status", "tasks"}, items[i:j]...)...)
	}
	fmt.Printf("read back the final states of %d acknowledged tasks\n", len(items))
	return 0
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: mlc ready | init | op <kind> | check <rule> | readback | get <path>")
		os.Exit(2)
	}
	arg := ""
	if len(os.Args) > 2 {
		arg = os.Args[2]
	}
	switch os.Args[1] {
	case "ready":
		if st, _, err := call("GET", "/health", nil); err != nil || st != 200 {
			os.Exit(1)
		}
		os.Exit(0)
	case "init":
		os.Exit(cmdInit())
	case "op":
		ops := map[string]func() int{"add": opAdd, "upd": opUpd, "del": opDel, "set": opSet, "swap": opSwap,
			"edit": opEdit, "dump": opDump, "snap": opSnap}
		if f, ok := ops[arg]; ok {
			os.Exit(f())
		}
	case "check":
		checks := map[string]func() int{"tasks": checkTasks, "docs": checkDocs, "settings": checkSettings,
			"swap": checkSwap, "edits": checkEdits, "dumps": checkDumps}
		if f, ok := checks[arg]; ok {
			os.Exit(f())
		}
	case "readback":
		os.Exit(readBack())
	case "get":
		st, b, err := call("GET", arg, nil)
		fmt.Println(st, err)
		os.Stdout.Write(b)
		fmt.Println()
		os.Exit(0)
	}
	fmt.Fprintln(os.Stderr, "mlc: unknown command", os.Args[1:])
	os.Exit(2)
}
