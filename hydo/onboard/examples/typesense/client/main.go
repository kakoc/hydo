// tsc is the test client of the Typesense example: the cluster's setup, the workload's writes,
// the reads during the run, the read-back and the final checks. It speaks Typesense's HTTP API
// with net/http and sends each request once, without retries.
//
//	tsc ready <addr>      the API at <addr> answers /health, whatever it says (a readiness check)
//	tsc init              wait until the three nodes are healthy with one leader, create the collection
//	tsc primary           print the current leader, waiting up to 15 s for one (the group's primary: command)
//	tsc op <kind>         one write; the composer's HYDO_* variables drive it
//	tsc read              reads of known documents through any node (anytime_read)
//	tsc probe             every node's Raft state, recorded for check available (anytime_leader)
//	tsc readback          the final: of the messages check: each write's effect, read back
//	tsc check <rule>      one check: converge, serves, export, bdelete, search, schema, available
//	tsc dump <node>       print one node's documents (for a look by hand)
//	tsc raw <node> <method> <path> [<body>] [<timeout s>]   one request (for a look by hand)
//
// The nodes are ts1, ts2 and ts3 (10.89.0.11 to .13), with the API on port 8108. A write goes to a
// node that the seed picks; a follower forwards it to the leader, as Typesense does for every
// client that talks to any node.
//
// Writes, one id each: <letter>.<doc>.<seq>.<tag>, with one letter per kind (c, u, p, d, f, g, b,
// i, a), short because hvx keeps 64 KiB of the guest's console per boot. A document is a
// register: its value is the tag of the write that last set it (w<cmd>), or "-" when it does not
// exist.
//
//	create  POST /collections/docs/documents of a new document d<cmd>
//	upsert  POST ...?action=upsert of a known document, deleted ones too: a new version
//	update  PATCH /collections/docs/documents/<id> of an existing document
//	delete  DELETE /collections/docs/documents/<id> of an existing document
//	fdelete DELETE /collections/docs/documents?filter_by=id:=<id> && tag:=<version>, and
//	fupdate PATCH of the same filter: a delete or update of the last acknowledged version only
//	import  POST /collections/docs/documents/import?action=create of 2 to 12 new documents
//	        i<cmd>x<k>; one write per line. One import in eight carries 100 documents, an answer
//	        of more than one batch of 40 lines, and one in eight 20 documents of 12 KB, a request
//	        body of more than one chunk of Typesense's request stream.
//	bdelete DELETE /collections/docs/documents?filter_by=tag:=<tag>&batch_size=$TSC_BATCH (2) of
//	        the documents of an earlier import that still hold its tag (at least 3): one write
//	        per document
//	alter   PATCH /collections/docs that adds the optional field x<cmd> (the schema; id
//	        a.x<cmd>.0.x<cmd>)
//
// One command at a time works on a document (a lock file under /tmp/tsc/lock). A document's last
// known value and seq are in /tmp/tsc/keys/<doc>, for the documents whose every write got a
// certain answer. A document whose write got no certain answer is never written again, so each
// document ends with at most one write of unknown outcome.
//
// Outcomes. $HYDO_ACKED gets the id of each acknowledged write, $HYDO_OUTCOMES gets
// "<id> ack|failed|unknown". A 2xx answer acknowledges. 400, 404, 409 and 422, and 503 "Not
// Ready or Lagging", mean that Typesense did nothing: failed. Every other answer, a timeout and no
// answer are unknown.
//
// The history writes (hydo guide messages): each write is a send of its id to the channel w, and
// an import is one send of all of its ids.
// `tsc readback` reads every document back from the leader after a barrier write and writes the
// final state of each write: s=ok for acknowledged, s=fail for failed, s=info for unknown, and
// f=1 or f=0 for whether the write's effect is there now, wherever that is decided.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

const (
	apiKey   = "hydo-key"
	port     = "8108"
	stateDir = "/tmp/tsc"
	coll     = "docs"
)

var nodes = []string{"ts1", "ts2", "ts3"}

var addrs = map[string]string{"ts1": "10.89.0.11", "ts2": "10.89.0.12", "ts3": "10.89.0.13"}

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

// hrec writes one record of the history writes: HYDO-HIST writes <process> <type> <time> <args>.
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
	f.WriteString(fmt.Sprintf("HYDO-HIST writes %d %s %s %s\n", proc, typ, uptimeNs(), strings.Join(args, " ")))
}

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
	if len(s) > 160 {
		s = s[:160] + "..."
	}
	return fmt.Sprintf("HTTP %d %s", a.status, s)
}

func call(node, method, path string, body []byte, timeout time.Duration) answer {
	cl := &http.Client{Timeout: timeout, Transport: &http.Transport{DisableKeepAlives: true}}
	var rd io.Reader
	if body != nil {
		rd = bytes.NewReader(body)
	}
	req, err := http.NewRequest(method, "http://"+node+":"+port+path, rd)
	if err != nil {
		return answer{err: err}
	}
	req.Header.Set("X-TYPESENSE-API-KEY", apiKey)
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

// outcome turns an answer into ack, failed or unknown.
func outcome(a answer) string {
	switch {
	case a.err != nil:
		return "unknown"
	case a.status >= 200 && a.status < 300:
		return "ack"
	case a.status == 400 || a.status == 404 || a.status == 409 || a.status == 422:
		return "failed"
	case a.status == 503 && strings.Contains(string(a.body), "Not Ready or Lagging"):
		return "failed"
	}
	return "unknown"
}

// health is whether a node answers /health with ok.
func health(node string) bool {
	a := call(node, "GET", "/health", nil, 3*time.Second)
	return a.err == nil && a.status == 200 && strings.Contains(string(a.body), "true")
}

// state is a node's Raft state from /debug: 1 is the leader, 4 a follower; -1 when it does not answer.
func state(node string) int {
	a := call(node, "GET", "/debug", nil, 3*time.Second)
	if a.err != nil || a.status != 200 {
		return -1
	}
	var d struct {
		State int `json:"state"`
	}
	if json.Unmarshal(a.body, &d) != nil {
		return -1
	}
	return d.State
}

func leader() string {
	for _, n := range nodes {
		if state(n) == 1 {
			return n
		}
	}
	return ""
}

// waitLeader waits for a healthy leader until the deadline.
func waitLeader(until time.Time) string {
	for {
		if l := leader(); l != "" && health(l) {
			return l
		}
		if time.Now().After(until) {
			return ""
		}
		time.Sleep(time.Second)
	}
}

// ---- documents ----

type doc struct {
	ID    string `json:"id"`
	Tag   string `json:"tag"`
	Title string `json:"title"`
	V     int    `json:"v"`
	Body  string `json:"body,omitempty"`
}

func newDoc(id, tag string, cmd int, pad int) doc {
	d := doc{ID: id, Tag: tag, Title: "doc " + id + " " + tag, V: cmd}
	if pad > 0 {
		d.Body = strings.Repeat("lorem ipsum dolor sit amet ", pad/27+1)[:pad]
	}
	return d
}

type keyState struct {
	seq int
	val string
}

type op struct {
	kind, key string
	seq       int
	val       string
}

// The history ids are short (the console of hvx keeps 64 KiB per boot): one letter per kind.
var letters = map[string]string{"create": "c", "upsert": "u", "update": "p", "delete": "d", "import": "i", "alter": "a", "fdelete": "f", "fupdate": "g", "bdelete": "b"}

func (o op) id() string { return fmt.Sprintf("%s.%s.%d.%s", letters[o.kind], o.key, o.seq, o.val) }

func parseID(id string) (op, bool) {
	f := strings.Split(id, ".")
	if len(f) != 4 {
		return op{}, false
	}
	for k, l := range letters {
		if l == f[0] {
			return op{k, f[1], atoiDef(f[2], -1), f[3]}, true
		}
	}
	return op{}, false
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
	tmp := filepath.Join(stateDir, "keys", "."+key)
	os.WriteFile(tmp, []byte(fmt.Sprintf("%d %s\n", s.seq, s.val)), 0o644)
	os.Rename(tmp, filepath.Join(stateDir, "keys", key))
}

func retire(key string) { os.Remove(filepath.Join(stateDir, "keys", key)) }

// pickKey locks a known document; exists says whether it must exist now.
func pickKey(r *rand.Rand, exists bool) (string, keyState) {
	ents, _ := os.ReadDir(filepath.Join(stateDir, "keys"))
	var names []string
	for _, e := range ents {
		if n := e.Name(); !strings.HasPrefix(n, ".") {
			names = append(names, n)
		}
	}
	sort.Strings(names)
	r.Shuffle(len(names), func(i, j int) { names[i], names[j] = names[j], names[i] })
	for _, k := range names {
		if !lock(k) {
			continue
		}
		s, ok := loadState(k)
		if ok && (!exists || s.val != "-") {
			return k, s
		}
		unlock(k)
	}
	return "", keyState{}
}

// begin records a write's start; rec false leaves out its history record (an import's lines
// share one record).
func begin(o op, rec bool) {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" unknown")
	if rec {
		hrec(cmdID(), "invoke", "send", "w", o.id())
	}
}

// finish records the outcome of o and updates the document's state.
func finish(o op, oc string, a answer, before keyState, existed, rec bool) {
	appendLine(os.Getenv("HYDO_OUTCOMES"), o.id()+" "+oc)
	reg := o.kind != "alter"
	switch oc {
	case "ack":
		appendLine(os.Getenv("HYDO_ACKED"), o.id())
		if rec {
			hrec(cmdID(), "ok", "send", "w", o.id())
		}
		if reg {
			saveState(o.key, keyState{o.seq, o.val})
		}
	case "failed":
		if rec {
			hrec(cmdID(), "fail", "send", "w", o.id())
		}
		if reg && existed {
			saveState(o.key, keyState{o.seq, before.val})
		}
		fmt.Fprintf(os.Stderr, "tsc: %s: failed: %s\n", o.id(), a.text())
	default:
		if rec {
			hrec(cmdID(), "info", "send", "w", o.id())
		}
		if reg {
			retire(o.key)
		}
		fmt.Fprintf(os.Stderr, "tsc: %s: unknown: %s\n", o.id(), a.text())
	}
}

func docPath(id string) string { return "/collections/" + coll + "/documents/" + url.PathEscape(id) }

func runOp(kind string) int {
	r := rng()
	cmd := cmdID()
	tag := fmt.Sprintf("w%d", cmd)
	node := nodes[r.IntN(len(nodes))]
	const tmo = 10 * time.Second

	switch kind {
	case "create":
		key := fmt.Sprintf("d%d", cmd)
		lock(key)
		defer unlock(key)
		o := op{kind, key, 0, tag}
		begin(o, true)
		b, _ := json.Marshal(newDoc(key, tag, cmd, 0))
		a := call(node, "POST", "/collections/"+coll+"/documents", b, tmo)
		finish(o, outcome(a), a, keyState{}, false, true)
	case "upsert", "update", "delete", "fdelete", "fupdate":
		key, s := pickKey(r, kind != "upsert")
		if key == "" {
			return 0
		}
		defer unlock(key)
		var a answer
		o := op{kind, key, s.seq + 1, tag}
		if kind == "delete" || kind == "fdelete" {
			o.val = "-"
		}
		begin(o, true)
		switch kind {
		case "upsert":
			b, _ := json.Marshal(newDoc(key, tag, cmd, 0))
			a = call(node, "POST", "/collections/"+coll+"/documents?action=upsert", b, tmo)
		case "update":
			b, _ := json.Marshal(map[string]any{"tag": tag, "title": "doc " + key + " " + tag, "v": cmd})
			a = call(node, "PATCH", docPath(key), b, tmo)
		case "delete":
			a = call(node, "DELETE", docPath(key), nil, tmo)
		case "fdelete", "fupdate":
			// by a filter on the document's id and its known version: the write applies only
			// where the document holds that version, so it acts as a compare-and-set
			q := url.Values{}
			q.Set("filter_by", "id:="+key+" && tag:="+s.val)
			path := "/collections/" + coll + "/documents?" + q.Encode()
			var field string
			if kind == "fdelete" {
				a = call(node, "DELETE", path, nil, tmo)
				field = "num_deleted"
			} else {
				b, _ := json.Marshal(map[string]any{"tag": tag, "title": "doc " + key + " " + tag, "v": cmd})
				a = call(node, "PATCH", path, b, tmo)
				field = "num_updated"
			}
			if outcome(a) == "ack" {
				var res map[string]any
				n := -1.0
				if json.Unmarshal(a.body, &res) == nil {
					if v, ok := res[field].(float64); ok {
						n = v
					}
				}
				if n == 0 {
					// Typesense matched no document: it holds another version than the
					// last acknowledged one, or none; the write did nothing
					a = answer{status: 409, body: a.body}
					fmt.Fprintf(os.Stderr, "tsc: %s matched no document with %s\n", o.id(), s.val)
				} else if n != 1 {
					a = answer{status: 500, body: a.body}
				}
			}
		}
		finish(o, outcome(a), a, s, true, true)
	case "import":
		n, pad := 2+r.IntN(11), 0
		switch r.IntN(8) {
		case 0:
			n, pad = 20, 12000 // a request body of more than one chunk
		case 1:
			n = 100 // an answer of more than one batch of 40 lines
		}
		var body bytes.Buffer
		var ops []op
		for k := 0; k < n; k++ {
			key := fmt.Sprintf("i%dx%d", cmd, k)
			lock(key)
			defer unlock(key)
			o := op{kind, key, 0, tag}
			ops = append(ops, o)
			b, _ := json.Marshal(newDoc(key, tag, cmd, pad))
			body.Write(b)
			body.WriteByte('\n')
		}
		var ids []string
		for _, o := range ops {
			begin(o, false)
			ids = append(ids, o.id())
		}
		hrec(cmdID(), "invoke", append([]string{"send", "w"}, ids...)...)
		a := call(node, "POST", "/collections/"+coll+"/documents/import?action=create", body.Bytes(), tmo)
		lines := []string{}
		if a.err == nil && a.status == 200 {
			for _, l := range strings.Split(strings.TrimSpace(string(a.body)), "\n") {
				lines = append(lines, l)
			}
		}
		for i, o := range ops {
			oc := outcome(a)
			if oc == "ack" {
				oc = "unknown"
				if i < len(lines) {
					var res struct {
						Success bool `json:"success"`
						Code    int  `json:"code"`
					}
					if json.Unmarshal([]byte(lines[i]), &res) == nil {
						if res.Success {
							oc = "ack"
						} else if res.Code == 400 || res.Code == 404 || res.Code == 409 || res.Code == 422 {
							oc = "failed"
						}
					}
				}
			}
			la := a
			if i < len(lines) && oc != "ack" {
				la = answer{status: a.status, body: []byte(lines[i])}
			}
			finish(o, oc, la, keyState{}, false, false)
			ids[i] = o.id() + ";s=" + map[string]string{"ack": "ok", "failed": "fail", "unknown": "info"}[oc]
		}
		if outcome(a) == "unknown" {
			hrec(cmdID(), "info", "send", "w", strings.Join(ids, " "))
		} else {
			hrec(cmdID(), "ok", "send", "w", strings.Join(ids, " "))
		}
		if a.err == nil && a.status == 200 && len(lines) != len(ops) {
			fmt.Fprintf(os.Stderr, "tsc: import of %d documents answered %d lines\n", len(ops), len(lines))
		}
	case "bdelete":
		return batchDelete(r, node, tmo)
	case "alter":
		f := fmt.Sprintf("x%d", cmd)
		o := op{kind, f, 0, f}
		begin(o, true)
		b, _ := json.Marshal(map[string]any{"fields": []map[string]any{{"name": f, "type": "string", "optional": true}}})
		a := call(node, "PATCH", "/collections/"+coll, b, 60*time.Second)
		finish(o, outcome(a), a, keyState{}, false, true)
	default:
		fmt.Fprintf(os.Stderr, "tsc: unknown write %s\n", kind)
		return 2
	}
	return 0
}

// batchDelete deletes the documents of one earlier import that still hold its tag, by a filter
// on the tag, in batches of 2 (the parameter batch_size). It takes an import whose every document
// has a certain state and is free, so that the filter matches exactly the documents it locked.
func batchDelete(r *rand.Rand, node string, tmo time.Duration) int {
	cmd := cmdID()
	docs := map[string][]string{} // import tag -> its documents
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if o, ok := parseID(f[0]); ok && o.kind == "import" {
			docs[o.val] = append(docs[o.val], o.key)
		}
	}
	var tags []string
	for t := range docs {
		tags = append(tags, t)
	}
	sort.Strings(tags)
	r.Shuffle(len(tags), func(i, j int) { tags[i], tags[j] = tags[j], tags[i] })
	for _, t := range tags {
		var locked, hold []string
		var states []keyState
		ok := true
		seen := map[string]bool{}
		for _, k := range docs[t] {
			if seen[k] {
				continue
			}
			seen[k] = true
			if !lock(k) {
				ok = false
				break
			}
			locked = append(locked, k)
			st, has := loadState(k)
			if !has {
				ok = false // a write of unknown outcome: the filter could match it or not
				break
			}
			if st.val == t {
				hold = append(hold, k)
				states = append(states, st)
			}
		}
		if !ok || len(hold) < 3 {
			for _, k := range locked {
				unlock(k)
			}
			continue
		}
		defer func() {
			for _, k := range locked {
				unlock(k)
			}
		}()
		var ops []op
		var ids []string
		for i, k := range hold {
			o := op{"bdelete", k, states[i].seq + 1, "-"}
			ops = append(ops, o)
			begin(o, false)
			ids = append(ids, o.id())
		}
		hrec(cmd, "invoke", append([]string{"send", "w"}, ids...)...)
		q := url.Values{}
		q.Set("filter_by", "tag:="+t)
		q.Set("batch_size", envDef("TSC_BATCH", "2"))
		a := call(node, "DELETE", "/collections/"+coll+"/documents?"+q.Encode(), nil, tmo)
		oc := outcome(a)
		if oc == "ack" {
			var res struct {
				N int `json:"num_deleted"`
			}
			if json.Unmarshal(a.body, &res) != nil || res.N != len(hold) {
				fmt.Fprintf(os.Stderr, "tsc: a delete of %d documents with %s answered %s\n", len(hold), t, a.text())
				oc = "unknown"
			}
		}
		st := map[string]string{"ack": "ok", "failed": "fail", "unknown": "info"}[oc]
		for i, o := range ops {
			finish(o, oc, a, states[i], true, false)
			ids[i] = o.id() + ";s=" + st
		}
		if oc == "unknown" {
			hrec(cmd, "info", "send", "w", strings.Join(ids, " "))
		} else {
			hrec(cmd, "ok", "send", "w", strings.Join(ids, " "))
		}
		return 0
	}
	return 0
}

// ---- reads during the run ----

// written is every value that a write sent, per document, from $HYDO_OUTCOMES.
func written() map[string]map[string]bool {
	w := map[string]map[string]bool{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) != 2 {
			continue
		}
		o, ok := parseID(f[0])
		if !ok || o.kind == "alter" {
			continue
		}
		if w[o.key] == nil {
			w[o.key] = map[string]bool{"-": true}
		}
		w[o.key][o.val] = true
	}
	return w
}

// getDoc reads one document from a node: its tag, "-" when it is not there.
func getDoc(node, id string) (string, error) {
	a := call(node, "GET", docPath(id), nil, 5*time.Second)
	if a.err != nil {
		return "", a.err
	}
	switch a.status {
	case 200:
		var d doc
		if err := json.Unmarshal(a.body, &d); err != nil {
			return "", err
		}
		return d.Tag, nil
	case 404:
		return "-", nil
	}
	return "", fmt.Errorf("%s", a.text())
}

func readSome() int {
	r := rng()
	w := written()
	var keys []string
	for k := range w {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	r.Shuffle(len(keys), func(i, j int) { keys[i], keys[j] = keys[j], keys[i] })
	if len(keys) > 5 {
		keys = keys[:5]
	}
	n := nodes[r.IntN(len(nodes))]
	for _, k := range keys {
		v, err := getDoc(n, k)
		if err != nil {
			return 0
		}
		if !w[k][v] {
			if w2 := written(); w2[k][v] {
				continue
			}
			msg := fmt.Sprintf("phantom read: %s answered %s for %s, which no write sent", n, v, k)
			console("tsc: " + msg)
			fmt.Println(msg)
			return 100
		}
	}
	return 0
}

// ---- the leader during the run ----

const probeFile = stateDir + "/probes"

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

// probe asks every node for its Raft state once a second for 12 s, and records each round as
// "<uptime ms> <nodes that answered> <leaders> <states>" in /tmp/tsc/probes (anytime_leader).
func probe() int {
	end := time.Now().Add(12 * time.Second)
	for time.Now().Before(end) {
		probeOnce()
		time.Sleep(time.Second)
	}
	return 0
}

func probeOnce() {
	t := uptimeMs()
	alive, leaders := 0, 0
	var st []string
	for _, n := range nodes {
		a := call(n, "GET", "/debug", nil, 2*time.Second)
		s := -1
		if a.err == nil && a.status == 200 {
			var d struct {
				State int `json:"state"`
			}
			if json.Unmarshal(a.body, &d) == nil {
				s = d.State
			}
		}
		if s >= 0 {
			alive++
		}
		if s == 1 {
			leaders++
		}
		st = append(st, fmt.Sprintf("%s=%d", n, s))
	}
	os.MkdirAll(stateDir, 0o755)
	appendLine(probeFile, fmt.Sprintf("%d %d %d %s", t, alive, leaders, strings.Join(st, ",")))
}

// checkAvailable: the probes of the run never saw two or more nodes answer without a leader
// for longer than 30 s. Typesense's elections take the election timeout of 5 s and a few
// rounds; a cluster of three keeps serving with one node down.
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
	for i, p := range ps {
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
		_ = i
	}
	fmt.Printf("%d probes of the leader\n", len(ps))
	if worst > 30000 {
		fmt.Printf("from %.1f s to %.1f s of uptime no node was the leader while two or more answered (%s)\n", float64(from)/1000, float64(to)/1000, at)
		fmt.Printf("no leader for %d s while two or more nodes answered\n", worst/1000)
		return 1
	}
	return 0
}

// ---- the read-back ----

func opsByKey() (map[string][]op, map[string]string) {
	oc := map[string]string{}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) == 2 {
			oc[f[0]] = f[1]
		}
	}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		oc[l] = "ack"
	}
	by := map[string][]op{}
	for id := range oc {
		if o, ok := parseID(id); ok {
			by[o.key] = append(by[o.key], o)
		}
	}
	for k := range by {
		sort.Slice(by[k], func(i, j int) bool { return by[k][i].seq < by[k][j].seq })
	}
	return by, oc
}

// barrier upserts the document "barrier" through node and waits for the answer. Typesense
// applies the writes of one collection in log order, so when the barrier is acknowledged, every
// earlier write of the collection is applied on the leader.
func barrier(node string) error {
	b, _ := json.Marshal(newDoc("barrier", fmt.Sprintf("b%d", time.Now().UnixNano()), 0, 0))
	a := call(node, "POST", "/collections/"+coll+"/documents?action=upsert", b, 20*time.Second)
	if outcome(a) != "ack" {
		return fmt.Errorf("the barrier write through %s: %s", node, a.text())
	}
	return nil
}

// export reads every document of a node: id -> tag. It asks for one batch of everything, so
// that its answer is one chunk: after a backward step of the wall clock an answer of several
// chunks stops after the first (RESULTS.md, finding 1), and checkExport judges that.
func export(node string) (map[string]string, error) {
	a := call(node, "GET", "/collections/"+coll+"/documents/export?include_fields=id,tag&batch_size=1000000", nil, 60*time.Second)
	if a.err != nil || a.status != 200 {
		return nil, fmt.Errorf("export from %s: %s", node, a.text())
	}
	m := map[string]string{}
	for _, l := range strings.Split(strings.TrimSpace(string(a.body)), "\n") {
		if l == "" {
			continue
		}
		var d doc
		if err := json.Unmarshal([]byte(l), &d); err != nil {
			return nil, fmt.Errorf("export from %s: %v", node, err)
		}
		if d.ID != "barrier" && !strings.HasPrefix(d.ID, "serves") {
			m[d.ID] = d.Tag
		}
	}
	return m, nil
}

// leaderState waits for a healthy leader and a barrier through it, and reads its documents.
func leaderState(wait time.Duration) (string, map[string]string, error) {
	until := time.Now().Add(wait)
	var last error
	for {
		l := waitLeader(until)
		if l == "" {
			if last == nil {
				last = fmt.Errorf("no healthy leader")
			}
			return "", nil, last
		}
		if err := barrier(l); err != nil {
			last = err
		} else if m, err := export(l); err != nil {
			last = err
		} else {
			return l, m, nil
		}
		if time.Now().After(until) {
			return "", nil, last
		}
		time.Sleep(2 * time.Second)
	}
}

func readBack() int {
	const proc = 100000
	by, oc := opsByKey()
	l, have, err := leaderState(150 * time.Second)
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
		if ops[0].kind == "alter" {
			continue
		}
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
						it += ";f=0;e=" + enc("the document holds "+v)
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
		hrec(proc, "invoke", append([]string{"status", "w"}, ids[i:j]...)...)
		hrec(proc, "ok", append([]string{"status", "w"}, items[i:j]...)...)
	}
	fmt.Printf("read back %d writes of %d documents from the leader %s\n", len(items), len(have), l)
	return 0
}

// ---- checks ----

// checkConverge: every node is healthy, and every node's documents are the leader's.
func checkConverge() int {
	l, want, err := leaderState(20 * time.Second)
	if err != nil {
		fmt.Printf("no leader's documents: %v\n", err)
		return 1
	}
	bad := 0
	for _, n := range nodes {
		if n == l {
			continue
		}
		if !health(n) {
			fmt.Printf("%s is not healthy (/debug state %d)\n", n, state(n))
			return 1
		}
		got, err := export(n)
		if err != nil {
			fmt.Println(err)
			return 1
		}
		for k, w := range want {
			if g, ok := got[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("%s lacks %s, which the leader %s holds as %s\n", n, k, l, w)
				}
			} else if g != w {
				if bad++; bad <= 5 {
					fmt.Printf("%s holds %s = %s, and the leader %s holds %s\n", n, k, g, l, w)
				}
			}
		}
		for k, g := range got {
			if _, ok := want[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("%s holds %s = %s, which the leader %s does not hold\n", n, k, g, l)
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("a follower differs from the leader in %d documents\n", bad)
		return 1
	}
	return 0
}

// search is every document that a search for everything finds on a node: id -> tag.
func search(node, q, filter string) (map[string]string, int, error) {
	m := map[string]string{}
	found := 0
	for page := 1; page < 200; page++ {
		p := url.Values{}
		p.Set("q", q)
		p.Set("query_by", "title")
		p.Set("per_page", "250")
		p.Set("page", strconv.Itoa(page))
		p.Set("include_fields", "id,tag")
		if filter != "" {
			p.Set("filter_by", filter)
		}
		a := call(node, "GET", "/collections/"+coll+"/documents/search?"+p.Encode(), nil, 30*time.Second)
		if a.err != nil || a.status != 200 {
			return nil, 0, fmt.Errorf("search on %s: %s", node, a.text())
		}
		var res struct {
			Found int `json:"found"`
			Hits  []struct {
				Document doc `json:"document"`
			} `json:"hits"`
		}
		if err := json.Unmarshal(a.body, &res); err != nil {
			return nil, 0, fmt.Errorf("search on %s: %v", node, err)
		}
		found = res.Found
		for _, h := range res.Hits {
			if h.Document.ID != "barrier" && !strings.HasPrefix(h.Document.ID, "serves") {
				m[h.Document.ID] = h.Document.Tag
			}
		}
		if len(res.Hits) < 250 {
			break
		}
	}
	return m, found, nil
}

// checkSearch: on every node, a search for everything finds exactly the documents that the node
// stores, each with its stored version, and a search for a document's own tag finds it.
func checkSearch() int {
	if _, _, err := leaderState(20 * time.Second); err != nil {
		fmt.Printf("no leader: %v\n", err)
		return 2
	}
	bad := 0
	for _, n := range nodes {
		stored, err := export(n)
		if err != nil {
			fmt.Println(err)
			return 2
		}
		found, _, err := search(n, "*", "")
		if err != nil {
			fmt.Println(err)
			return 2
		}
		for k, v := range stored {
			if g, ok := found[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("%s stores %s = %s, and a search for everything does not find it\n", n, k, v)
				}
			} else if g != v {
				if bad++; bad <= 5 {
					fmt.Printf("%s stores %s = %s, and a search finds it as %s\n", n, k, v, g)
				}
			}
		}
		for k, g := range found {
			if _, ok := stored[k]; !ok {
				if bad++; bad <= 5 {
					fmt.Printf("a search on %s finds %s = %s, which the node does not store\n", n, k, g)
				}
			}
		}
		// the text search: each of up to 20 documents by its own tag
		var ks []string
		for k := range stored {
			ks = append(ks, k)
		}
		sort.Strings(ks)
		if len(ks) > 20 {
			ks = ks[len(ks)-20:]
		}
		for _, k := range ks {
			m, _, err := search(n, stored[k], "id:="+k)
			if err != nil {
				fmt.Println(err)
				return 2
			}
			if m[k] != stored[k] {
				if bad++; bad <= 5 {
					fmt.Printf("a search on %s for %s does not find %s\n", n, stored[k], k)
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("the search index differs from the stored documents in %d cases\n", bad)
		return 1
	}
	return 0
}

// checkSchema: every field that an acknowledged alter added is in the schema of every node.
func checkSchema() int {
	_, oc := opsByKey()
	var want []string
	for id, o := range oc {
		if p, ok := parseID(id); ok && p.kind == "alter" && o == "ack" {
			want = append(want, p.key)
		}
	}
	sort.Strings(want)
	if _, _, err := leaderState(20 * time.Second); err != nil {
		fmt.Printf("no leader: %v\n", err)
		return 2
	}
	bad := 0
	for _, n := range nodes {
		a := call(n, "GET", "/collections/"+coll, nil, 10*time.Second)
		if a.err != nil || a.status != 200 {
			fmt.Printf("the schema from %s: %s\n", n, a.text())
			return 2
		}
		var s struct {
			Fields []struct {
				Name string `json:"name"`
			} `json:"fields"`
		}
		if json.Unmarshal(a.body, &s) != nil {
			return 2
		}
		have := map[string]bool{}
		for _, f := range s.Fields {
			have[f.Name] = true
		}
		for _, f := range want {
			if !have[f] {
				if bad++; bad <= 5 {
					fmt.Printf("the schema on %s lacks the field %s, which an acknowledged alter added\n", n, f)
				}
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d acknowledged fields are missing from a node's schema\n", bad)
		return 1
	}
	return 0
}

// checkExport: on every node an export with Typesense's default batch size, the documented way
// to back a collection up, ends within 20 s with every document of the node's one-batch export.
func checkExport() int {
	for _, n := range nodes {
		want, err := export(n)
		if err != nil {
			fmt.Println(err)
			return 2
		}
		a := call(n, "GET", "/collections/"+coll+"/documents/export", nil, 20*time.Second)
		if a.err != nil || a.status != 200 {
			fmt.Printf("%d documents in one batch, and the export of %s with the default batch size ended with %s after %d bytes\n", len(want), n, a.text(), len(a.body))
			fmt.Printf("an export with the default batch size did not end on %s\n", n)
			return 1
		}
		got := 0
		for _, l := range strings.Split(strings.TrimSpace(string(a.body)), "\n") {
			var d doc
			if json.Unmarshal([]byte(l), &d) == nil && d.ID != "barrier" && !strings.HasPrefix(d.ID, "serves") {
				got++
			}
		}
		if got != len(want) {
			fmt.Printf("the export of %s gave %d documents, and the export in one batch %d\n", n, got, len(want))
			return 1
		}
	}
	return 0
}

// checkServes: after the heal, a new document written through each node is acknowledged and
// then readable on every node.
//
// The reads wait up to 15 s without another write: Raft's heartbeats tell the followers that the
// last write is committed, and a follower that is not told serves the version before it.
func checkServes() int {
	tags := make([]string, len(nodes))
	for i, n := range nodes {
		id := fmt.Sprintf("serves%d", i)
		tags[i] = fmt.Sprintf("s%d", time.Now().UnixNano())
		b, _ := json.Marshal(newDoc(id, tags[i], 0, 0))
		a := call(n, "POST", "/collections/"+coll+"/documents?action=upsert", b, 10*time.Second)
		if outcome(a) != "ack" {
			fmt.Printf("a write through %s is not acknowledged: %s\n", n, a.text())
			return 1
		}
	}
	until := time.Now().Add(15 * time.Second)
	for {
		missing := ""
		for _, n := range nodes {
			for i := range nodes {
				if v, err := getDoc(n, fmt.Sprintf("serves%d", i)); err != nil || v != tags[i] {
					missing = fmt.Sprintf("%s does not have the write through %s 15 s after its acknowledgement (it holds %q)", n, nodes[i], v)
				}
			}
		}
		if missing == "" {
			return 0
		}
		if time.Now().After(until) {
			fmt.Println(missing)
			fmt.Println("an acknowledged write is not on every node 15 s after its acknowledgement")
			return 1
		}
		time.Sleep(time.Second)
	}
}

// checkBatchDelete: every delete by a filter in batches whose answer the client did not get has
// deleted all of its documents or none, on the leader. A half-done one is still running or
// stopped: Typesense runs each later batch from a timer of its HTTP event loop.
func checkBatchDelete() int {
	by, oc := opsByKey()
	groups := map[string][]string{} // the import's tag -> the documents of a delete of unknown outcome
	for k, ops := range by {
		for _, o := range ops {
			if o.kind == "bdelete" && oc[o.id()] == "unknown" {
				// the tag the document held before the delete is its import's
				for _, p := range ops {
					if p.kind == "import" {
						groups[p.val] = append(groups[p.val], k)
					}
				}
			}
		}
	}
	if len(groups) == 0 {
		return 0
	}
	l, have, err := leaderState(20 * time.Second)
	if err != nil {
		fmt.Printf("no leader's documents: %v\n", err)
		return 2
	}
	var tags []string
	for t := range groups {
		tags = append(tags, t)
	}
	sort.Strings(tags)
	bad := 0
	for _, t := range tags {
		left := 0
		for _, k := range groups[t] {
			if have[k] == t {
				left++
			}
		}
		if left > 0 && left < len(groups[t]) {
			if bad++; bad <= 5 {
				fmt.Printf("on the leader %s the delete of the documents with %s removed %d of %d\n", l, t, len(groups[t])-left, len(groups[t]))
			}
		}
	}
	if bad > 0 {
		fmt.Printf("%d deletes by a filter stopped part way\n", bad)
		return 1
	}
	return 0
}

func cmdDump(node string) int {
	m, err := export(node)
	if err != nil {
		fmt.Println(err)
		return 2
	}
	var ks []string
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	for _, k := range ks {
		fmt.Printf("%s %s\n", k, m[k])
	}
	fmt.Printf("%s: %d documents, /debug state %d, healthy %v\n", node, len(m), state(node), health(node))
	return 0
}

// cmdReady: the API answers /health at addr, healthy or not. A node alone has no quorum, so
// "healthy" would keep the first node of a restarted group from ever being ready.
func cmdReady(addr string) int {
	a := call(addr, "GET", "/health", nil, 3*time.Second)
	if a.err != nil {
		return 1
	}
	return 0
}

// cmdInit waits until every node is healthy with one leader, then creates the collection.
func cmdInit() int {
	until := time.Now().Add(170 * time.Second)
	for {
		ok := true
		for _, n := range nodes {
			if !health(n) {
				ok = false
			}
		}
		l := leader()
		if ok && l != "" {
			schema := map[string]any{
				"name": coll,
				"fields": []map[string]any{
					{"name": "title", "type": "string"},
					{"name": "tag", "type": "string", "facet": true},
					{"name": "v", "type": "int32"},
					{"name": "body", "type": "string", "optional": true, "index": false},
				},
			}
			b, _ := json.Marshal(schema)
			a := call(l, "POST", "/collections", b, 30*time.Second)
			if a.status == 201 || a.status == 409 {
				fmt.Printf("the collection %s is on the leader %s\n", coll, l)
				return 0
			}
			fmt.Printf("creating the collection on %s: %s\n", l, a.text())
		}
		if time.Now().After(until) {
			for _, n := range nodes {
				fmt.Printf("%s: /debug state %d, healthy %v\n", n, state(n), health(n))
			}
			fmt.Println("the cluster did not become healthy")
			return 1
		}
		time.Sleep(time.Second)
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: tsc ready <addr> | init | primary | op <kind> | read | readback | check <rule> | dump <node>")
		os.Exit(2)
	}
	arg := func(i int) string {
		if len(os.Args) > i {
			return os.Args[i]
		}
		return ""
	}
	rc := 0
	switch os.Args[1] {
	case "ready":
		rc = cmdReady(arg(2))
	case "init":
		rc = cmdInit()
	case "primary":
		// hydo asks this 10 times a second apart; an election after a kill of the leader can
		// take longer (RESULTS.md, "What hydo lacked"), so it waits up to 15 s itself
		until := time.Now().Add(15 * time.Second)
		for {
			if l := leader(); l != "" {
				fmt.Println(l)
				break
			}
			if time.Now().After(until) {
				rc = 1
				break
			}
			time.Sleep(time.Second)
		}
	case "op":
		rc = runOp(arg(2))
	case "read":
		rc = readSome()
	case "readback":
		rc = readBack()
	case "probe":
		rc = probe()
	case "check":
		switch arg(2) {
		case "converge":
			rc = checkConverge()
		case "search":
			rc = checkSearch()
		case "schema":
			rc = checkSchema()
		case "export":
			rc = checkExport()
		case "available":
			rc = checkAvailable()
		case "bdelete":
			rc = checkBatchDelete()
		case "serves":
			rc = checkServes()
		default:
			fmt.Fprintf(os.Stderr, "tsc: unknown check %s\n", arg(2))
			rc = 2
		}
	case "dump":
		rc = cmdDump(arg(2))
	case "raw":
		// tsc raw <node> <method> <path> [<body>] [<timeout s>]: one request, for a look by hand
		var body []byte
		if arg(5) != "" {
			body = []byte(arg(5))
		}
		a := call(arg(2), arg(3), arg(4), body, time.Duration(atoiDef(arg(6), 10))*time.Second)
		fmt.Println(a.text())
		if a.err != nil {
			rc = 1
		}
	default:
		fmt.Fprintf(os.Stderr, "tsc: unknown command %s\n", os.Args[1])
		rc = 2
	}
	os.Exit(rc)
}
