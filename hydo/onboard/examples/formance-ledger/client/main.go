// ledgerc is the test client of the Formance Ledger example: the workload's operations and the
// two checks that need more than SQL. It uses only Go's standard library.
//
//	ledgerc ready              exit 0 when the ledger's API answers (the service's readiness)
//	ledgerc init               create the ledger "main" (idempotent; the first_ command)
//	ledgerc op <kind>          one client operation; the composer's HYDO_* variables drive it
//	ledgerc check model <pg>   the acknowledged operations determine the ledger (rule kind 2)
//	ledgerc check once         every acknowledged operation is in the ledger once, the others at most once
//	ledgerc check api-sums     every balance the API reports is the sum of the postings it reports
//	ledgerc check log-replay   replaying the log gives every balance, and holds each transaction once
//
// Every operation is described by one token, written to $HYDO_ACKED when the client saw it
// acknowledged and to $HYDO_OUTCOMES with its outcome:
//
//	<op>~<src>,<dst>,<asset>,<amount>+<src>,<dst>,<asset>,<amount>...   postings (short names: posting.String)
//	<op>~R<orig op>                                                     the revert of <orig op>
//
// with `|` between the transactions of a bulk. <op> is a kind letter and the composer's entry
// number (t17). Every transaction the client creates carries the metadata hydo_op=<op> (and
// hydo_part=<n> in a bulk), so the check can find it; the metadata is not unique in the ledger.
// Duplicates are kept out by the ledger's idempotency keys (Idempotency-Key, the bulk's "ik").
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"math/rand/v2"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

const ledgerName = "main"

var (
	base     = env("LEDGER_URL", "http://10.89.0.12:3068")
	useIK    = env("LEDGERC_IK", "1") == "1"
	attempts = atoi(env("LEDGERC_ATTEMPTS", "3"))
	timeout  = time.Duration(atoi(env("LEDGERC_TIMEOUT_MS", "4000"))) * time.Millisecond
	txsFile  = env("LEDGERC_TXS", "/tmp/ledgerc/txs") // "<op> <txid> <token>" of acknowledged transactions
	client   = &http.Client{Timeout: timeout}
)

// The chart of accounts. users and fees never go below zero; a credit line may go down to
// -creditLimit (numscript "allowing overdraft up to"); world is the unbounded outside.
var (
	users       = []string{"users:1", "users:2", "users:3", "users:4", "users:5", "users:6"}
	credits     = []string{"credit:1", "credit:2"}
	assets      = []string{"USD/2", "EUR/2"}
	creditLimit = int64(5000)
)

func env(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func atoi(s string) int {
	n, err := strconv.Atoi(s)
	if err != nil {
		fail2("bad number %q", s)
	}
	return n
}

func fail2(f string, a ...any) {
	fmt.Printf(f+"\n", a...)
	os.Exit(2)
}

func main() {
	if len(os.Args) < 2 {
		fail2("usage: ledgerc ready|init|op <kind>|check <name>")
	}
	switch os.Args[1] {
	case "ready":
		st, _, err := do("GET", "/v2/_info", "", nil)
		if err != nil || st != 200 {
			os.Exit(1)
		}
	case "init":
		initLedger()
	case "op":
		if len(os.Args) < 3 {
			fail2("usage: ledgerc op <kind>")
		}
		os.Exit(op(os.Args[2]))
	case "check":
		if len(os.Args) < 3 {
			fail2("usage: ledgerc check model <pg volumes>|api-sums")
		}
		switch os.Args[2] {
		case "model":
			if len(os.Args) < 4 {
				fail2("usage: ledgerc check model <pg volumes jsonl>")
			}
			os.Exit(checkModel(os.Args[3], true))
		case "once":
			os.Exit(checkModel("", false))
		case "api-sums":
			os.Exit(checkAPISums())
		case "log-replay":
			os.Exit(checkLogReplay())
		}
		fail2("unknown check %s", os.Args[2])
	default:
		fail2("unknown command %s", os.Args[1])
	}
}

// do sends one request and returns the status and body; err is a transport error (no answer).
func do(method, path, ik string, body any) (int, []byte, error) {
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
	if ik != "" {
		req.Header.Set("Idempotency-Key", ik)
	}
	resp, err := client.Do(req)
	if err != nil {
		return 0, nil, err
	}
	defer resp.Body.Close()
	lastHit.Store(resp.Header.Get("Idempotency-Hit") == "true")
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return 0, nil, err // the answer was cut: unknown
	}
	return resp.StatusCode, b, nil
}

func initLedger() {
	for i := 0; i < 60; i++ {
		st, b, err := do("POST", "/v2/"+ledgerName, "", map[string]any{})
		if err == nil && (st == 204 || st == 200 || st == 201) {
			fmt.Println("ledger created")
			return
		}
		if err == nil && st == 400 && bytes.Contains(b, []byte("LEDGER_ALREADY_EXISTS")) {
			fmt.Println("ledger exists")
			return
		}
		fmt.Printf("create ledger: %d %s %v\n", st, b, err)
		time.Sleep(time.Second)
	}
	os.Exit(1)
}

// ---- operations ----

type posting struct {
	Src, Dst, Asset string
	Amt             int64
}

// String is the posting in a token. Short names keep the client's records small: hydo carries
// $HYDO_ACKED and $HYDO_OUTCOMES into the recovery boot after a power cut through the console.
func (p posting) String() string {
	return fmt.Sprintf("%s,%s,%s,%d", short(p.Src), short(p.Dst), short(p.Asset), p.Amt)
}

// The short names in tokens: u<n> users:<n>, c<n> credit:<n>, f fees, w world, U USD/2, E EUR/2.
var shortNames = map[string]string{"fees": "f", "world": "w", "USD/2": "U", "EUR/2": "E"}

func short(name string) string {
	if s, ok := shortNames[name]; ok {
		return s
	}
	if n, ok := strings.CutPrefix(name, "users:"); ok {
		return "u" + n
	}
	if n, ok := strings.CutPrefix(name, "credit:"); ok {
		return "c" + n
	}
	if n, ok := strings.CutPrefix(name, "shops:"); ok {
		return "s" + n
	}
	return name
}

func long(s string) string {
	for k, v := range shortNames {
		if v == s {
			return k
		}
	}
	switch {
	case strings.HasPrefix(s, "u"):
		return "users:" + s[1:]
	case strings.HasPrefix(s, "c"):
		return "credit:" + s[1:]
	case strings.HasPrefix(s, "s"):
		return "shops:" + s[1:]
	}
	return s
}

func (p posting) json() map[string]any {
	return map[string]any{"source": p.Src, "destination": p.Dst, "asset": p.Asset, "amount": p.Amt}
}

func postingsToken(ps []posting) string {
	s := make([]string, len(ps))
	for i, p := range ps {
		s[i] = p.String()
	}
	return strings.Join(s, "+")
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

// send runs one request with retries. A retry reuses the idempotency key (when the client uses
// keys). The outcome is ack on a 2xx answer; failed only when every attempt got a definite
// business refusal (4xx) or reached no ledger (connection refused); unknown when an attempt got
// no answer or a 5xx and no later attempt was acknowledged (an earlier attempt may still be
// applied, even when a later one is refused).
func send(path, ik string, body any) (outcome, []byte, string) {
	sawUnknown := false
	last := ""
	for a := 1; a <= attempts; a++ {
		st, b, err := do("POST", path, ik, body)
		switch {
		case err != nil && errors.Is(err, syscall.ECONNREFUSED):
			last = "not sent: " + err.Error() // nothing listened: this attempt reached no ledger
		case err != nil:
			sawUnknown, last = true, "no answer: "+err.Error()
		case st >= 200 && st < 300:
			if a > 1 {
				// what the faults made the client do, as properties in the run's report
				assert("ledgerc.retry_acked", "an operation was acknowledged on a retry")
				if lastHit.Load() {
					assert("ledgerc.idempotency_hit", "a retry was answered from the idempotency key (Idempotency-Hit: true)")
				}
			}
			return ack, b, fmt.Sprintf("%d", st)
		case st >= 500:
			sawUnknown, last = true, fmt.Sprintf("%d %s", st, trim(b))
		default:
			last = fmt.Sprintf("%d %s", st, trim(b))
			if sawUnknown {
				return unknown, b, last
			}
			return failed, b, last
		}
		time.Sleep(time.Duration(200*a) * time.Millisecond)
	}
	if !sawUnknown {
		return failed, nil, last // no attempt reached the ledger
	}
	return unknown, nil, last
}

// lastHit: the last answer carried Idempotency-Hit: true.
var lastHit atomic.Bool

// assert writes a "sometimes" record of the assertion SDKs' format to the console, so that the
// run's report counts how often a fault drove the client down this path.
func assert(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	defer f.Close()
	b, _ := json.Marshal(map[string]any{"t": "sometimes", "id": id, "ok": true, "msg": msg})
	fmt.Fprintf(f, "HYDO-ASSERT %s\n", b)
}

func trim(b []byte) string {
	s := strings.ReplaceAll(string(b), "\n", " ")
	if len(s) > 300 {
		s = s[:300]
	}
	return s
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	_ = os.MkdirAll(filepath.Dir(path), 0o755)
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

func record(token string, o outcome) {
	switch o {
	case ack:
		appendLine(os.Getenv("HYDO_ACKED"), token)
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" ack")
	case failed:
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" failed")
	default:
		appendLine(os.Getenv("HYDO_OUTCOMES"), token+" unknown")
	}
}

func op(kind string) int {
	seed, _ := strconv.ParseUint(env("HYDO_SEED", "1"), 10, 64)
	id := env("HYDO_CMD_ID", "0")
	r := rand.New(rand.NewPCG(seed, 0x5eed))
	pick := func(l []string) string { return l[r.IntN(len(l))] }
	two := func() (string, string) {
		a := r.IntN(len(users))
		b := (a + 1 + r.IntN(len(users)-1)) % len(users)
		return users[a], users[b]
	}
	amt := func(lo, hi int64) int64 { return lo + r.Int64N(hi-lo+1) }

	var (
		short = ""
		ps    []posting
		extra = map[string]any{}
		path  = "/v2/" + ledgerName + "/transactions"
	)
	switch kind {
	case "deposit": // money enters from outside
		short = "d" + id
		ps = []posting{{"world", pick(users), pick(assets), amt(500, 8000)}}
	case "transfer": // a payment with a fee: two postings from one source
		short = "t" + id
		a, b := two()
		asset := pick(assets)
		v := amt(1, 2000)
		ps = []posting{{a, b, asset, v}, {a, "fees", asset, v/100 + 1}}
	case "exchange": // two assets in one transaction
		short = "x" + id
		a := pick(users)
		v := amt(10, 1000)
		ps = []posting{{a, "world", "USD/2", v}, {"world", a, "EUR/2", v * 9 / 10}}
	case "backdated": // a transfer with an effective date in the past
		short = "b" + id
		a, b := two()
		ps = []posting{{a, b, pick(assets), amt(1, 1500)}}
		back := time.Duration(1+r.IntN(3600)) * time.Second
		extra["timestamp"] = time.Now().UTC().Add(-back).Format(time.RFC3339Nano)
	case "credit": // drawn on a credit line, which may go below zero down to its limit
		short = "c" + id
		ps = []posting{{pick(credits), pick(users), "USD/2", amt(100, 2500)}}
	case "repay":
		short = "p" + id
		ps = []posting{{pick(users), pick(credits), "USD/2", amt(100, 2500)}}
	case "revert":
		return revert(id, r)
	case "bulk":
		return bulk(id, r)
	case "open":
		return open(id, r)
	default:
		fmt.Printf("unknown op %s\n", kind)
		return 2
	}
	token := short + "~" + postingsToken(ps)
	record(token, unknown) // before the first attempt: a killed command leaves "unknown"
	body := map[string]any{"metadata": map[string]string{"hydo_op": short}}
	for k, v := range extra {
		body[k] = v
	}
	if kind == "credit" {
		p := ps[0]
		body["script"] = map[string]any{"plain": fmt.Sprintf(
			"send [%s %d] (\n  source = @%s allowing overdraft up to [%s %d]\n  destination = @%s\n)\n",
			p.Asset, p.Amt, p.Src, p.Asset, creditLimit, p.Dst)}
	} else {
		pj := make([]any, len(ps))
		for i, p := range ps {
			pj[i] = p.json()
		}
		body["postings"] = pj
	}
	ik := ""
	if useIK {
		ik = "op-" + short
	}
	o, b, why := send(path, ik, body)
	record(token, o)
	fmt.Printf("%s %s %s\n", token, []string{"ack", "failed", "unknown"}[o], why)
	if o == ack {
		var resp struct {
			Data struct {
				ID json.Number `json:"id"`
			} `json:"data"`
		}
		if err := json.Unmarshal(b, &resp); err == nil && resp.Data.ID != "" {
			appendLine(txsFile, short+" "+resp.Data.ID.String()+" "+token)
		}
		return 0
	}
	return 1
}

// open: a new shop's first two sales arrive at once, one in each asset. Two transactions sent
// together to an account that does not exist yet (each its own operation and key).
func open(id string, r *rand.Rand) int {
	shop := "shops:" + id
	var wg sync.WaitGroup
	rc := make([]int, len(assets))
	for i, asset := range assets {
		short := fmt.Sprintf("o%s%c", id, 'a'+i)
		ps := []posting{{"world", shop, asset, 100 + r.Int64N(900)}}
		wg.Add(1)
		go func() {
			defer wg.Done()
			token := short + "~" + postingsToken(ps)
			record(token, unknown)
			ik := ""
			if useIK {
				ik = "op-" + short
			}
			o, _, why := send("/v2/"+ledgerName+"/transactions", ik, map[string]any{
				"metadata": map[string]string{"hydo_op": short},
				"postings": []any{ps[0].json()},
			})
			record(token, o)
			fmt.Printf("%s %s %s\n", token, []string{"ack", "failed", "unknown"}[o], why)
			if o != ack {
				rc[i] = 1
			}
		}()
	}
	wg.Wait()
	return rc[0] | rc[1]
}

// revert reverts one acknowledged transaction the client created (not a revert, not a bulk).
func revert(id string, r *rand.Rand) int {
	b, _ := os.ReadFile(txsFile)
	var cands [][]string
	for _, l := range strings.Split(strings.TrimSpace(string(b)), "\n") {
		f := strings.Fields(l)
		if len(f) == 3 && f[0][0] != 'r' && f[0][0] != 'k' {
			cands = append(cands, f)
		}
	}
	if len(cands) == 0 {
		fmt.Println("nothing to revert yet")
		return 0
	}
	c := cands[r.IntN(len(cands))]
	short := "r" + id
	token := short + "~R" + c[0]
	atEffective := r.IntN(2) == 0
	record(token, unknown)
	ik := ""
	if useIK {
		ik = "op-" + short
	}
	path := fmt.Sprintf("/v2/%s/transactions/%s/revert?atEffectiveDate=%t", ledgerName, c[1], atEffective)
	at := map[bool]string{true: "1", false: "0"}[atEffective] // for the check final_revert_dates.sql
	o, _, why := send(path, ik, map[string]any{"metadata": map[string]string{"hydo_op": short, "hydo_at": at}})
	record(token, o)
	fmt.Printf("%s (tx %s, atEffectiveDate=%t) %s %s\n", token, c[1], atEffective,
		[]string{"ack", "failed", "unknown"}[o], why)
	if o == ack {
		return 0
	}
	return 1
}

// bulk sends two transfers as one atomic bulk: both are applied or neither.
func bulk(id string, r *rand.Rand) int {
	short := "k" + id
	var elems []any
	var parts []string
	for i := 1; i <= 2; i++ {
		a := users[r.IntN(len(users))]
		b := users[(r.IntN(len(users)-1)+1+indexOf(users, a))%len(users)]
		p := posting{a, b, assets[r.IntN(len(assets))], 1 + r.Int64N(1500)}
		parts = append(parts, p.String())
		e := map[string]any{"action": "CREATE_TRANSACTION", "data": map[string]any{
			"postings": []any{p.json()},
			"metadata": map[string]string{"hydo_op": short, "hydo_part": strconv.Itoa(i)},
		}}
		if useIK {
			e["ik"] = fmt.Sprintf("op-%s.%d", short, i)
		}
		elems = append(elems, e)
	}
	token := short + "~" + strings.Join(parts, "|")
	record(token, unknown)
	o, _, why := send("/v2/"+ledgerName+"/_bulk?atomic=true", "", elems)
	record(token, o)
	fmt.Printf("%s %s %s\n", token, []string{"ack", "failed", "unknown"}[o], why)
	if o == ack {
		return 0
	}
	return 1
}

func indexOf(l []string, s string) int {
	for i, v := range l {
		if v == s {
			return i
		}
	}
	return -1
}

// ---- reading the ledger through its API ----

type apiTx struct {
	ID       json.Number       `json:"id"`
	Metadata map[string]string `json:"metadata"`
	Reverted bool              `json:"reverted"`
	Postings []struct {
		Source      string      `json:"source"`
		Destination string      `json:"destination"`
		Asset       string      `json:"asset"`
		Amount      json.Number `json:"amount"`
	} `json:"postings"`
}

type apiAccount struct {
	Address string `json:"address"`
	Volumes map[string]struct {
		Input  json.Number `json:"input"`
		Output json.Number `json:"output"`
	} `json:"volumes"`
}

// list reads every page of a cursor endpoint.
func list[T any](path string) ([]T, error) {
	var all []T
	cursor := ""
	for page := 0; page < 10000; page++ {
		p := path + "pageSize=100"
		if cursor != "" {
			p = path + "cursor=" + url.QueryEscape(cursor)
		}
		st, b, err := get(p)
		if err != nil {
			return nil, err
		}
		if st != 200 {
			return nil, fmt.Errorf("GET %s: %d %s", p, st, trim(b))
		}
		var resp struct {
			Cursor struct {
				HasMore bool   `json:"hasMore"`
				Next    string `json:"next"`
				Data    []T    `json:"data"`
			} `json:"cursor"`
		}
		d := json.NewDecoder(bytes.NewReader(b))
		d.UseNumber()
		if err := d.Decode(&resp); err != nil {
			return nil, err
		}
		all = append(all, resp.Cursor.Data...)
		if !resp.Cursor.HasMore {
			return all, nil
		}
		cursor = resp.Cursor.Next
	}
	return nil, errors.New("too many pages")
}

func get(p string) (int, []byte, error) {
	var lastErr error
	for a := 0; a < 5; a++ {
		st, b, err := do("GET", p, "", nil)
		if err == nil && st < 500 {
			return st, b, nil
		}
		lastErr = err
		if err == nil {
			lastErr = fmt.Errorf("%d %s", st, trim(b))
		}
		time.Sleep(time.Second)
	}
	return 0, nil, lastErr
}

// volumes: account -> asset -> [input, output]
type volumes map[string]map[string]*[2]big.Int

func (v volumes) add(acc, asset string, in, out *big.Int) {
	if v[acc] == nil {
		v[acc] = map[string]*[2]big.Int{}
	}
	if v[acc][asset] == nil {
		v[acc][asset] = &[2]big.Int{}
	}
	x := v[acc][asset]
	x[0].Add(&x[0], in)
	x[1].Add(&x[1], out)
}

func (v volumes) post(src, dst, asset string, amt *big.Int) {
	zero := new(big.Int)
	v.add(src, asset, zero, amt)
	v.add(dst, asset, amt, zero)
}

func bigOf(s string) *big.Int {
	n, ok := new(big.Int).SetString(s, 10)
	if !ok {
		fail2("bad amount %q", s)
	}
	return n
}

// diff lists the (account, asset) pairs whose volumes differ; zero volumes equal missing ones.
func diff(want, got volumes) []string {
	keys := map[string]bool{}
	for a, m := range want {
		for s := range m {
			keys[a+" "+s] = true
		}
	}
	for a, m := range got {
		for s := range m {
			keys[a+" "+s] = true
		}
	}
	var out []string
	for k := range keys {
		f := strings.Fields(k)
		w, g := [2]big.Int{}, [2]big.Int{}
		if x := want[f[0]][f[1]]; x != nil {
			w = *x
		}
		if x := got[f[0]][f[1]]; x != nil {
			g = *x
		}
		if w[0].Cmp(&g[0]) != 0 || w[1].Cmp(&g[1]) != 0 {
			out = append(out, fmt.Sprintf("%s %s: expected in=%s out=%s, got in=%s out=%s",
				f[0], f[1], w[0].String(), w[1].String(), g[0].String(), g[1].String()))
		}
	}
	sort.Strings(out)
	return out
}

func apiVolumes() (volumes, error) {
	accs, err := list[apiAccount]("/v2/" + ledgerName + "/accounts?expand=volumes&")
	if err != nil {
		return nil, err
	}
	v := volumes{}
	for _, a := range accs {
		for asset, x := range a.Volumes {
			v.add(a.Address, asset, bigOf(x.Input.String()), bigOf(x.Output.String()))
		}
	}
	return v, nil
}

// checkAPISums: every balance the API reports is the sum of the postings of the transactions
// it reports (a revert is a transaction with the reversed postings).
func checkAPISums() int {
	txs, err := list[apiTx]("/v2/" + ledgerName + "/transactions?")
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	got, err := apiVolumes()
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	want := volumes{}
	for _, t := range txs {
		for _, p := range t.Postings {
			want.post(p.Source, p.Destination, p.Asset, bigOf(p.Amount.String()))
		}
	}
	d := diff(want, got)
	for i, l := range d {
		if i < 10 {
			fmt.Println(l)
		}
	}
	if len(d) > 0 {
		fmt.Printf("%d account volumes differ from the sum of the postings of %d transactions\n", len(d), len(txs))
		return 1
	}
	fmt.Printf("the volumes of every account are the sum of the postings of %d transactions\n", len(txs))
	return 0
}

type apiLog struct {
	ID   json.Number `json:"id"`
	Type string      `json:"type"`
	Data struct {
		Transaction apiTx `json:"transaction"`
	} `json:"data"`
}

// checkLogReplay: the log alone gives the ledger. Replaying the transactions of the
// NEW_TRANSACTION and REVERTED_TRANSACTION logs gives every account's volumes, and the log holds
// each transaction the API lists exactly once.
func checkLogReplay() int {
	logs, err := list[apiLog]("/v2/" + ledgerName + "/logs?")
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	txs, err := list[apiTx]("/v2/" + ledgerName + "/transactions?")
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	got, err := apiVolumes()
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	want := volumes{}
	inLog := map[string]int{}
	for _, l := range logs {
		if l.Type != "NEW_TRANSACTION" && l.Type != "REVERTED_TRANSACTION" {
			continue
		}
		inLog[l.Data.Transaction.ID.String()]++
		for _, p := range l.Data.Transaction.Postings {
			want.post(p.Source, p.Destination, p.Asset, bigOf(p.Amount.String()))
		}
	}
	bad := 0
	for _, t := range txs {
		if n := inLog[t.ID.String()]; n != 1 {
			bad++
			fmt.Printf("transaction %s is in the log %d times\n", t.ID, n)
		}
		delete(inLog, t.ID.String())
	}
	for id := range inLog {
		bad++
		fmt.Printf("the log has transaction %s, which the API does not list\n", id)
	}
	d := diff(want, got)
	for i, l := range d {
		if i < 10 {
			fmt.Println(l)
		}
	}
	fmt.Printf("%d logs, %d transactions: %d transactions not once in the log, %d account volumes differ from the replay\n",
		len(logs), len(txs), bad, len(d))
	if bad+len(d) > 0 {
		return 1
	}
	return 0
}

// ---- the model check ----

type clientOp struct {
	token, short string
	txs          [][]posting // one entry per ledger transaction the operation creates
	revertOf     string
	outcome      string // ack | unknown | failed
}

func parseToken(tok string) (*clientOp, error) {
	i := strings.IndexByte(tok, '~')
	if i < 1 {
		return nil, fmt.Errorf("bad token %q", tok)
	}
	o := &clientOp{token: tok, short: tok[:i]}
	rest := tok[i+1:]
	if strings.HasPrefix(rest, "R") {
		o.revertOf = rest[1:]
		o.txs = [][]posting{nil}
		return o, nil
	}
	for _, tx := range strings.Split(rest, "|") {
		var ps []posting
		for _, p := range strings.Split(tx, "+") {
			f := strings.Split(p, ",")
			if len(f) != 4 {
				return nil, fmt.Errorf("bad posting %q in %q", p, tok)
			}
			n, err := strconv.ParseInt(f[3], 10, 64)
			if err != nil {
				return nil, fmt.Errorf("bad amount in %q", tok)
			}
			ps = append(ps, posting{long(f[0]), long(f[1]), long(f[2]), n})
		}
		o.txs = append(o.txs, ps)
	}
	return o, nil
}

func readLines(path string) []string {
	if path == "" {
		return nil
	}
	f, err := os.Open(path)
	if err != nil {
		return nil
	}
	defer f.Close()
	var out []string
	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 1<<20), 1<<20)
	for s.Scan() {
		if l := strings.TrimSpace(s.Text()); l != "" {
			out = append(out, l)
		}
	}
	return out
}

// checkModel: the operations the client saw determine the ledger. Every acknowledged operation
// is in the ledger exactly once, an operation without an answer at most once (all of its
// transactions or none), an operation refused for certain not at all, nothing else is there, and
// the volumes of every account, as the API reports them and as PostgreSQL holds them, are those
// that the operations found in the ledger produce. It knows nothing of the ledger beyond the
// operations' postings.
func checkModel(pgFile string, withVolumes bool) int {
	ops := map[string]*clientOp{}
	order := []string{}
	add := func(tok, outc string) {
		o, err := parseToken(tok)
		if err != nil {
			fmt.Println(err)
			return
		}
		if prev, ok := ops[o.short]; ok {
			if prev.outcome != "ack" {
				prev.outcome = outc
			}
			return
		}
		o.outcome = outc
		ops[o.short] = o
		order = append(order, o.short)
	}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		f := strings.Fields(l)
		if len(f) == 2 {
			add(f[0], f[1])
		}
	}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		add(l, "ack")
		ops[strings.SplitN(l, "~", 2)[0]].outcome = "ack"
	}

	txs, err := list[apiTx]("/v2/" + ledgerName + "/transactions?")
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	inLedger := map[string]int{} // op -> number of its transactions in the ledger
	foreign := 0
	for _, t := range txs {
		s := t.Metadata["hydo_op"]
		if s == "" || ops[s] == nil {
			foreign++
			fmt.Printf("transaction %s (hydo_op=%q) comes from no operation the client sent\n", t.ID, s)
			continue
		}
		inLedger[s]++
	}

	want := volumes{}
	var missing, dup, partial, refusedApplied, acked, unk, unkIn int
	for _, s := range order {
		o := ops[s]
		n := inLedger[s]
		parts := len(o.txs)
		switch o.outcome {
		case "ack":
			acked++
			if n < parts {
				missing++
				fmt.Printf("acknowledged %s is in the ledger %d times, expected %d\n", o.token, n, parts)
			}
		case "unknown":
			unk++
			if n > 0 {
				unkIn++
			}
			if n != 0 && n != parts {
				partial++
				fmt.Printf("unanswered %s is in the ledger %d times, expected 0 or %d\n", o.token, n, parts)
			}
		default:
			if n > 0 {
				refusedApplied++
				fmt.Printf("refused %s is in the ledger %d times\n", o.token, n)
			}
		}
		if n > parts {
			dup++
			fmt.Printf("%s %s is in the ledger %d times, expected %d\n", o.outcome, o.token, n, parts)
		}
		if n == 0 {
			continue
		}
		// what the operation did, applied as often as the ledger holds it (a duplicate is
		// reported above; counting it here keeps the volume comparison about volumes)
		times := (n + parts - 1) / parts
		for k := 0; k < times; k++ {
			if o.revertOf != "" {
				orig := ops[o.revertOf]
				if orig == nil {
					fmt.Printf("%s reverts %s, which the client never sent\n", o.token, o.revertOf)
					continue
				}
				for _, tx := range orig.txs {
					for _, p := range tx {
						want.post(p.Dst, p.Src, p.Asset, big.NewInt(p.Amt))
					}
				}
				continue
			}
			for _, tx := range o.txs {
				for _, p := range tx {
					want.post(p.Src, p.Dst, p.Asset, big.NewInt(p.Amt))
				}
			}
		}
	}

	if !withVolumes {
		bad := missing + dup + partial + refusedApplied + foreign
		fmt.Printf("%d acknowledged and %d unanswered operations (%d of them applied): %d missing, %d duplicated, %d partial, %d refused but applied, %d foreign\n",
			acked, unk, unkIn, missing, dup, partial, refusedApplied, foreign)
		if bad > 0 {
			return 1
		}
		return 0
	}
	api, err := apiVolumes()
	if err != nil {
		fmt.Println("ledger API unreadable:", err)
		return 2
	}
	dAPI := diff(want, api)
	for i, l := range dAPI {
		if i < 8 {
			fmt.Println("api:", l)
		}
	}
	pg := volumes{}
	for _, l := range readLines(pgFile) {
		var row map[string]*string
		if err := json.Unmarshal([]byte(l), &row); err != nil {
			fmt.Println("bad postgres row:", l)
			return 2
		}
		g := func(k string) string {
			if row[k] == nil {
				return "0"
			}
			return *row[k]
		}
		pg.add(g("accounts_address"), g("asset"), bigOf(g("input")), bigOf(g("output")))
	}
	dPG := diff(want, pg)
	for i, l := range dPG {
		if i < 8 {
			fmt.Println("postgres:", l)
		}
	}
	bad := missing + dup + partial + refusedApplied + foreign + len(dAPI) + len(dPG)
	fmt.Printf("%d acknowledged and %d unanswered operations (%d of them applied): %d missing, %d duplicated, %d partial, %d refused but applied, %d foreign; %d volumes differ in the API, %d in postgres\n",
		acked, unk, unkIn, missing, dup, partial, refusedApplied, foreign, len(dAPI), len(dPG))
	if bad > 0 {
		return 1
	}
	return 0
}
