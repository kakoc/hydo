// blnkc is the test client of the Blnk example: the workload's operations, the wait for the
// queue to settle, and the check that needs more than SQL. It uses only Go's standard library.
//
//	blnkc ready [url]             exit 0 when Blnk's API (or the page at url) answers 200
//	blnkc init                    fund the accounts (the first_ command)
//	blnkc op <kind>               one client operation; the composer's HYDO_* variables drive it
//	blnkc pending <pg-tx>         print how many saved operations have no final state yet
//	blnkc recover                 Blnk's documented queue recovery (POST /transactions/recover)
//	blnkc check model <pg-tx> <pg-bal>   the acknowledged operations determine every balance
//
// <pg-tx> and <pg-bal> are JSON lines from hydo-sql (the checks' queries in hydo/checks/).
//
// Every operation is one token, written to $HYDO_ACKED when the client saw it acknowledged and
// to $HYDO_OUTCOMES with its outcome:
//
//	<op>~<src>,<dst>,<amount>                     one transfer (d t s c p o, and h for a hold)
//	<op>~<src>,<dst>,<amount>|<src>,<dst>,<amount> an atomic bulk of two transfers (k)
//	<hold>a~C<amount>, <hold>b~C<amount>, <hold>b~V  the commits and the void of a hold
//
// <op> is a kind letter and the composer's entry number (t17); it is also the transaction's
// reference, which Blnk keeps unique. Amounts are in cents of USD (precision 100). Account names
// are short: u1..u4 users, s1 s2 shops, c1 c2 credit lines, e the empty account, m the merchant
// that holds are for, w @world; in Blnk each is the balance with the indicator @<name>.
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"net/url"
	"os"
	"sort"
	"strconv"
	"strings"
	"syscall"
	"time"
)

var (
	base     = env("BLNK_URL", "http://10.89.0.13:5001")
	attempts = atoi(env("BLNKC_ATTEMPTS", "3"))
	timeout  = time.Duration(atoi(env("BLNKC_TIMEOUT_MS", "4000"))) * time.Millisecond
	client   = &http.Client{Timeout: timeout}
)

// The chart of accounts. Users start with 5,000.00 and never run short; shops start with 20.00,
// so their spends compete for funds; a credit line may go down to -creditLimit; the empty
// account never has funds, so every transfer from it must be rejected.
var (
	users       = []string{"u1", "u2", "u3", "u4"}
	shops       = []string{"s1", "s2"}
	credits     = []string{"c1", "c2"}
	creditLimit = int64(5000) // cents: overdraft_limit 50 at precision 100
	seed        = []struct {
		ref, dst string
		amt      int64
	}{{"init-u1", "u1", 500000}, {"init-u2", "u2", 500000}, {"init-u3", "u3", 500000}, {"init-u4", "u4", 500000},
		{"init-s1", "s1", 2000}, {"init-s2", "s2", 2000}}
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

func indicator(short string) string {
	if short == "w" {
		return "@world"
	}
	return "@" + short
}

func main() {
	if len(os.Args) < 2 {
		fail2("usage: blnkc ready|init|op <kind>|pending <pg-tx>|recover|check model <pg-tx> <pg-bal>")
	}
	switch os.Args[1] {
	case "ready":
		path := "/ledgers"
		if len(os.Args) > 2 { // another URL, for the worker's health page
			base, path = os.Args[2], ""
		}
		st, _, err := do("GET", path, nil)
		if err != nil || st != 200 {
			os.Exit(1)
		}
	case "init":
		os.Exit(initAccounts())
	case "op":
		if len(os.Args) < 3 {
			fail2("usage: blnkc op <kind>")
		}
		os.Exit(op(os.Args[2]))
	case "pending":
		if len(os.Args) < 3 {
			fail2("usage: blnkc pending <pg-tx>")
		}
		os.Exit(pending(os.Args[2]))
	case "recover":
		st, b, err := do("POST", "/transactions/recover?threshold=2m", nil)
		fmt.Printf("recover: %d %s %v\n", st, trim(b), err)
		if err != nil || st >= 300 {
			os.Exit(1)
		}
	case "check":
		if len(os.Args) < 5 || os.Args[2] != "model" {
			fail2("usage: blnkc check model <pg-tx> <pg-bal>")
		}
		os.Exit(checkModel(os.Args[3], os.Args[4]))
	default:
		fail2("unknown command %s", os.Args[1])
	}
}

// do sends one request and returns the status and body; err is a transport error (no answer).
func do(method, path string, body any) (int, []byte, error) {
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
		return 0, nil, err // the answer was cut: unknown
	}
	return resp.StatusCode, b, nil
}

// errCode is Blnk's error_detail.code of an error answer.
func errCode(b []byte) string {
	var e struct {
		Detail struct {
			Code string `json:"code"`
		} `json:"error_detail"`
	}
	_ = json.Unmarshal(b, &e)
	return e.Detail.Code
}

func field(b []byte, k string) string {
	var m map[string]any
	if json.Unmarshal(b, &m) != nil {
		return ""
	}
	s, _ := m[k].(string)
	return s
}

func trim(b []byte) string {
	s := strings.ReplaceAll(string(b), "\n", " ")
	if len(s) > 300 {
		s = s[:300]
	}
	return s
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

var outcomeName = []string{"ack", "failed", "unknown"}

// send runs one request with retries; every attempt carries the same reference. The outcome is
// ack on a 2xx answer, or on a retry answered "reference already used" (an earlier attempt was
// saved); failed when every attempt got a definite refusal (4xx) or reached no Blnk (connection
// refused); unknown when an attempt got no answer or a 5xx and no later attempt was acknowledged.
// conflictUnknown: a retry answered 409 GEN_CONFLICT ("a commit or void is already queued") is
// unknown, since the earlier attempt may be the queued one.
func send(method, path string, body any, conflictUnknown bool) (outcome, []byte, string) {
	sawUnknown := false
	last := ""
	for a := 1; a <= attempts; a++ {
		st, b, err := do(method, path, body)
		code := errCode(b)
		switch {
		case err != nil && errors.Is(err, syscall.ECONNREFUSED):
			last = "not sent: " + err.Error()
		case err != nil:
			sawUnknown, last = true, "no answer: "+err.Error()
		case st >= 200 && st < 300:
			if a > 1 {
				assert("blnkc.retry_acked", "an operation was acknowledged on a retry")
			}
			return ack, b, fmt.Sprintf("%d", st)
		case st >= 500:
			sawUnknown, last = true, fmt.Sprintf("%d %s", st, trim(b))
		case code == "TXN_DUPLICATE_REFERENCE" && sawUnknown:
			assert("blnkc.retry_duplicate", "a retry was answered: reference already used")
			return ack, b, fmt.Sprintf("%d %s (an earlier attempt was saved)", st, code)
		case code == "GEN_CONFLICT" && conflictUnknown && sawUnknown:
			return unknown, b, fmt.Sprintf("%d %s (an earlier attempt may be queued)", st, code)
		default:
			last = fmt.Sprintf("%d %s", st, trim(b))
			if sawUnknown {
				return unknown, b, last
			}
			return failed, b, last
		}
		time.Sleep(time.Duration(300*a) * time.Millisecond)
	}
	if !sawUnknown {
		return failed, nil, last
	}
	return unknown, nil, last
}

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

type posting struct {
	Src, Dst string
	Amt      int64
}

func (p posting) String() string { return fmt.Sprintf("%s,%s,%d", p.Src, p.Dst, p.Amt) }

// txnBody is a POST /transactions body (or one element of a bulk).
func txnBody(ref string, p posting, extra map[string]any) map[string]any {
	b := map[string]any{
		"precise_amount": p.Amt, "precision": 100, "currency": "USD", "reference": ref,
		"source": indicator(p.Src), "destination": indicator(p.Dst), "description": ref,
	}
	for k, v := range extra {
		b[k] = v
	}
	return b
}

func initAccounts() int {
	for _, s := range seed {
		ok := false
		for i := 0; i < 60 && !ok; i++ {
			st, b, err := do("POST", "/transactions", txnBody(s.ref, posting{"w", s.dst, s.amt}, map[string]any{"allow_overdraft": true}))
			switch {
			case err == nil && (st == 201 || errCode(b) == "TXN_DUPLICATE_REFERENCE"):
				ok = true
			default:
				fmt.Printf("fund %s: %d %s %v\n", s.dst, st, trim(b), err)
				time.Sleep(time.Second)
			}
		}
		if !ok {
			return 1
		}
	}
	// wait until the worker applied every funding transfer
	for i := 0; i < 120; i++ {
		done := 0
		for _, s := range seed {
			if st, b, err := do("GET", "/transactions/reference/"+s.ref+"_q", nil); err == nil && st == 200 && field(b, "status") == "APPLIED" {
				done++
			}
		}
		if done == len(seed) {
			fmt.Printf("%d accounts funded\n", done)
			return 0
		}
		time.Sleep(time.Second)
	}
	fmt.Println("the funding transfers were not applied within 120 s")
	return 1
}

// ---- operations ----

func op(kind string) int {
	seedN, _ := strconv.ParseUint(env("HYDO_SEED", "1"), 10, 64)
	id := env("HYDO_CMD_ID", "0")
	r := rand.New(rand.NewPCG(seedN, 0xb1c))
	pick := func(l []string) string { return l[r.IntN(len(l))] }
	two := func(l []string) (string, string) {
		a := r.IntN(len(l))
		b := (a + 1 + r.IntN(len(l)-1)) % len(l)
		return l[a], l[b]
	}
	amt := func(lo, hi int64) int64 { return lo + r.Int64N(hi-lo+1) }
	var (
		ref   string
		p     posting
		extra map[string]any
	)
	switch kind {
	case "deposit": // money from outside, to a user or a shop (a sale)
		ref = "d" + id
		dst := pick(users)
		if r.IntN(3) == 0 {
			dst = pick(shops)
		}
		p, extra = posting{"w", dst, amt(100, 5000)}, map[string]any{"allow_overdraft": true}
	case "transfer": // between two users, who never run short
		ref = "t" + id
		a, b := two(users)
		p = posting{a, b, amt(1, 500)}
	case "spend": // a shop pays a user; shops run short, so Blnk must refuse some
		ref = "s" + id
		p = posting{pick(shops), pick(users), amt(300, 900)}
	case "draw": // a draw on a credit line, which may go below zero down to its limit
		ref = "c" + id
		p, extra = posting{pick(credits), pick(users), amt(1000, 3000)},
			map[string]any{"allow_overdraft": true, "overdraft_limit": creditLimit / 100}
	case "repay": // a user repays a credit line
		ref = "p" + id
		p = posting{pick(users), pick(credits), amt(1000, 3000)}
	case "overdraw": // from the empty account: Blnk must reject it
		ref = "o" + id
		p = posting{"e", pick(users), 100}
	case "hold":
		return hold(id, r)
	case "bulk":
		return bulk(id, r)
	default:
		fmt.Printf("unknown op %s\n", kind)
		return 2
	}
	token := ref + "~" + p.String()
	record(token, unknown) // before the first attempt: a killed command leaves "unknown"
	o, _, why := send("POST", "/transactions", txnBody(ref, p, extra), false)
	record(token, o)
	fmt.Printf("%s %s %s\n", token, outcomeName[o], why)
	if o == ack {
		return 0
	}
	return 1
}

// waitStatus polls GET /transactions/reference/<ref> until the transaction has one of want, for
// up to d; it returns the transaction's id and status ("" when not found in time).
func waitStatus(ref string, d time.Duration, want ...string) (string, string) {
	end := time.Now().Add(d)
	for time.Now().Before(end) {
		st, b, err := do("GET", "/transactions/reference/"+url.PathEscape(ref), nil)
		if err == nil && st == 200 {
			s := field(b, "status")
			for _, w := range want {
				if s == w {
					return field(b, "transaction_id"), s
				}
			}
		}
		time.Sleep(500 * time.Millisecond)
	}
	return "", ""
}

// hold places an inflight hold from a user for the merchant, commits part of it, and then
// commits the rest or voids it. Each step is an operation of its own with its own reference;
// the next step waits until Blnk applied the one before.
func hold(id string, r *rand.Rand) int {
	ref := "h" + id
	p := posting{users[r.IntN(len(users))], "m", 500 + r.Int64N(1501)}
	first := 1 + r.Int64N(p.Amt-1)
	voidRest := r.IntN(2) == 0
	token := ref + "~" + p.String()
	record(token, unknown)
	o, b, why := send("POST", "/transactions", txnBody(ref, p, map[string]any{"inflight": true}), false)
	record(token, o)
	fmt.Printf("%s %s %s\n", token, outcomeName[o], why)
	if o != ack {
		return 1
	}
	parent := field(b, "transaction_id")
	if parent == "" || field(b, "reference") != ref {
		parent, _ = waitStatus(ref, 10*time.Second, "QUEUED")
	}
	leg, st := waitStatus(ref+"_q", 30*time.Second, "INFLIGHT", "REJECTED")
	if st != "INFLIGHT" || parent == "" {
		fmt.Printf("%s: the hold is %q after 30 s; no commit sent\n", ref, st)
		return 1
	}
	action := func(sub, kind string, amount int64) bool {
		aref := ref + sub
		t := aref + "~V"
		body := map[string]any{"status": "void", "reference": aref}
		if kind == "commit" {
			t = fmt.Sprintf("%s~C%d", aref, amount)
			body = map[string]any{"status": "commit", "precise_amount": amount, "reference": aref}
		}
		record(t, unknown)
		o, _, why := send("PUT", "/transactions/inflight/"+parent, body, true)
		record(t, o)
		fmt.Printf("%s %s %s\n", t, outcomeName[o], why)
		if o != ack {
			return false
		}
		if _, s := waitStatus(leg+"_"+kind+"_"+aref, 30*time.Second, "APPLIED", "VOID"); s == "" {
			fmt.Printf("%s: not applied within 30 s\n", aref)
			return false
		}
		return true
	}
	if !action("a", "commit", first) {
		return 1
	}
	if voidRest {
		if !action("b", "void", 0) {
			return 1
		}
	} else if !action("b", "commit", p.Amt-first) {
		return 1
	}
	return 0
}

// bulk sends two transfers as one atomic bulk. One time in three the second comes from the
// empty account, so Blnk must reject the whole bulk.
func bulk(id string, r *rand.Rand) int {
	ref := "k" + id
	a, b := users[r.IntN(len(users))], users[r.IntN(len(users))]
	if a == b {
		b = users[(indexOf(users, a)+1)%len(users)]
	}
	ps := []posting{{a, b, 1 + r.Int64N(500)}}
	if r.IntN(3) == 0 {
		ps = append(ps, posting{"e", users[r.IntN(len(users))], 100})
	} else {
		c := users[r.IntN(len(users))]
		d := users[(indexOf(users, c)+1+r.IntN(len(users)-1))%len(users)]
		ps = append(ps, posting{c, d, 1 + r.Int64N(500)})
	}
	var items []any
	var parts []string
	for i, p := range ps {
		items = append(items, txnBody(fmt.Sprintf("%s.%d", ref, i+1), p, nil))
		parts = append(parts, p.String())
	}
	token := ref + "~" + strings.Join(parts, "|")
	record(token, unknown)
	o, ans, why := send("POST", "/transactions/bulk", map[string]any{"atomic": true, "transactions": items}, false)
	if o == ack && errCode(ans) == "TXN_DUPLICATE_REFERENCE" {
		// a retry finds the first transfer's reference used: the bulk may have failed after it
		o = unknown
	}
	record(token, o)
	fmt.Printf("%s %s %s\n", token, outcomeName[o], why)
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

// ---- what PostgreSQL holds ----

// txRow is one row of blnk.transactions, with the indicators of its balances.
type txRow struct {
	ID, Ref, Status, Parent, Src, Dst string
	Amt                               int64
}

func readJSONL(path string) []map[string]*string {
	f, err := os.Open(path)
	if err != nil {
		fail2("read %s: %v", path, err)
	}
	defer f.Close()
	var out []map[string]*string
	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 1<<20), 1<<20)
	for s.Scan() {
		l := strings.TrimSpace(s.Text())
		if l == "" {
			continue
		}
		var row map[string]*string
		if err := json.Unmarshal([]byte(l), &row); err != nil {
			fail2("bad row in %s: %s", path, l)
		}
		out = append(out, row)
	}
	return out
}

func str(row map[string]*string, k string) string {
	if v := row[k]; v != nil {
		return *v
	}
	return ""
}

func num(row map[string]*string, k string) int64 {
	s := str(row, k)
	if s == "" {
		return 0
	}
	if i := strings.IndexByte(s, '.'); i >= 0 {
		s = s[:i]
	}
	n, err := strconv.ParseInt(s, 10, 64)
	if err != nil {
		fail2("bad number %q in %s", str(row, k), k)
	}
	return n
}

func readTx(path string) map[string]*txRow {
	rows := map[string]*txRow{}
	for _, r := range readJSONL(path) {
		t := &txRow{ID: str(r, "transaction_id"), Ref: str(r, "reference"), Status: str(r, "status"),
			Parent: str(r, "parent_transaction"), Src: strings.TrimPrefix(str(r, "src"), "@"),
			Dst: strings.TrimPrefix(str(r, "dst"), "@"), Amt: num(r, "precise_amount")}
		if t.Src == "world" {
			t.Src = "w"
		}
		if t.Dst == "world" {
			t.Dst = "w"
		}
		rows[t.Ref] = t
	}
	return rows
}

// ---- the client's records ----

type clientOp struct {
	token, ref string
	ps         []posting // the transfers (one, two for a bulk, the hold's amount for a hold)
	action     string    // C or V for the action of a hold, "" otherwise
	amount     int64     // a commit's amount
	outcome    string    // ack | unknown | failed
}

func parseToken(tok string) (*clientOp, error) {
	i := strings.IndexByte(tok, '~')
	if i < 2 {
		return nil, fmt.Errorf("bad token %q", tok)
	}
	o := &clientOp{token: tok, ref: tok[:i]}
	rest := tok[i+1:]
	switch {
	case rest == "V":
		o.action = "V"
		return o, nil
	case strings.HasPrefix(rest, "C"):
		n, err := strconv.ParseInt(rest[1:], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("bad commit in %q", tok)
		}
		o.action, o.amount = "C", n
		return o, nil
	}
	for _, part := range strings.Split(rest, "|") {
		f := strings.Split(part, ",")
		if len(f) != 3 {
			return nil, fmt.Errorf("bad transfer %q in %q", part, tok)
		}
		n, err := strconv.ParseInt(f[2], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("bad amount in %q", tok)
		}
		o.ps = append(o.ps, posting{f[0], f[1], n})
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

// clientOps reads every operation with its last outcome; an acknowledgement wins.
func clientOps() (map[string]*clientOp, []string) {
	ops := map[string]*clientOp{}
	var order []string
	add := func(tok, outc string) {
		o, err := parseToken(tok)
		if err != nil {
			fmt.Println(err)
			return
		}
		if prev, ok := ops[o.ref]; ok {
			if prev.outcome != "ack" {
				prev.outcome = outc
			}
			return
		}
		o.outcome = outc
		ops[o.ref] = o
		order = append(order, o.ref)
	}
	for _, l := range readLines(os.Getenv("HYDO_OUTCOMES")) {
		if f := strings.Fields(l); len(f) == 2 {
			add(f[0], f[1])
		}
	}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		add(l, "ack")
		ops[strings.SplitN(l, "~", 2)[0]].outcome = "ack"
	}
	return ops, order
}

// holdOf: the hold an action belongs to (h12a -> h12).
func holdOf(ref string) string { return ref[:len(ref)-1] }

// actionRow finds the child that Blnk made for a hold's action: <leg>_commit_<ref> or
// <leg>_void_<ref>.
func actionRow(rows map[string]*txRow, leg *txRow, o *clientOp) *txRow {
	if leg == nil {
		return nil
	}
	kind := "commit"
	if o.action == "V" {
		kind = "void"
	}
	return rows[leg.ID+"_"+kind+"_"+o.ref]
}

// pending counts what has not reached a final state: transactions saved as QUEUED without their
// _q copy, and acknowledged commits or voids of a hold without their transaction.
func pending(pgTx string) int {
	rows := readTx(pgTx)
	n := 0
	for ref, t := range rows {
		// (an item of a bulk has the bulk's id as its parent; it is an original all the same)
		if t.Status == "QUEUED" && rows[ref+"_q"] == nil {
			n++
		}
	}
	ops, _ := clientOps()
	for _, o := range ops {
		if o.action != "" && o.outcome == "ack" && actionRow(rows, rows[holdOf(o.ref)+"_q"], o) == nil {
			n++
		}
	}
	fmt.Println(n)
	return 0
}

// ---- the model check ----

// vols: account -> [credit, debit, inflight credit, inflight debit]
type vols map[string]*[4]int64

func (v vols) get(a string) *[4]int64 {
	if v[a] == nil {
		v[a] = &[4]int64{}
	}
	return v[a]
}

func (v vols) post(p posting) {
	v.get(p.Dst)[0] += p.Amt
	v.get(p.Src)[1] += p.Amt
}

func (v vols) hold(p posting, remaining int64) {
	v.get(p.Dst)[2] += remaining
	v.get(p.Src)[3] += remaining
}

var volNames = []string{"credit", "debit", "inflight credit", "inflight debit"}

func diff(what string, want, got vols) []string {
	keys := map[string]bool{}
	for a := range want {
		keys[a] = true
	}
	for a := range got {
		keys[a] = true
	}
	var out []string
	for a := range keys {
		w, g := [4]int64{}, [4]int64{}
		if x := want[a]; x != nil {
			w = *x
		}
		if x := got[a]; x != nil {
			g = *x
		}
		for i := range w {
			if w[i] != g[i] {
				out = append(out, fmt.Sprintf("%s: @%s %s: expected %d, got %d", what, a, volNames[i], w[i], g[i]))
			}
		}
	}
	sort.Strings(out)
	return out
}

// say prints a line of the check's output and writes it to the console too: only a check's
// last line reaches the verdict, and the console keeps the others in the run's log.
func say(f string, a ...any) {
	line := fmt.Sprintf(f, a...)
	fmt.Println(line)
	if c, err := os.OpenFile("/dev/console", os.O_WRONLY, 0); err == nil {
		fmt.Fprintf(c, "blnkc check model: %s\n", line)
		c.Close()
	}
}

// checkModel: faults do not change the business result. From the client's records alone:
//   - every acknowledged operation is in Blnk once (by its reference), an unanswered one at
//     most once, one refused for certain never, and Blnk holds no transaction the client did
//     not send;
//   - every operation Blnk saved reached a final state;
//   - the operations whose result the client knows in advance have it: deposits, transfers
//     between users, repayments and holds from users apply (users never run short), a transfer
//     from the empty account is rejected, an atomic bulk applies whole, or not at all when one
//     of its transfers comes from the empty account;
//   - the transfers the client sent, as far as Blnk applied them, give every balance's credit,
//     debit and inflight volumes, as PostgreSQL holds them and as the API reports them.
//
// The amounts come from the client's tokens, not from Blnk's rows.
func checkModel(pgTx, pgBal string) int {
	rows := readTx(pgTx)
	ops, order := clientOps()
	want := vols{}
	for _, s := range seed {
		want.post(posting{"w", s.dst, s.amt})
	}
	var missing, refusedApplied, unsettled, wrongResult, notAtomic int
	var acked, unk, unkIn int
	bad := func(counter *int, f string, a ...any) {
		*counter++
		say(f, a...)
	}
	known := map[string]bool{}
	for _, s := range seed {
		known[s.ref], known[s.ref+"_q"] = true, true
	}
	holds := map[string][]*clientOp{}
	for _, ref := range order {
		o := ops[ref]
		if o.action != "" {
			holds[holdOf(ref)] = append(holds[holdOf(ref)], o)
			continue
		}
		var refs []string
		if len(o.ps) == 2 {
			refs = []string{ref + ".1", ref + ".2"}
		} else {
			refs = []string{ref}
		}
		present := 0
		for _, r := range refs {
			known[r], known[r+"_q"] = true, true
			if rows[r] != nil {
				present++
			}
		}
		switch o.outcome {
		case "ack":
			acked++
			if present < len(refs) {
				bad(&missing, "acknowledged %s: %d of its %d transactions are in Blnk", o.token, present, len(refs))
			}
		case "unknown":
			unk++
			if present > 0 {
				unkIn++
			}
		default:
			if present > 0 {
				bad(&refusedApplied, "refused %s is in Blnk", o.token)
			}
		}
		if present == 0 {
			continue
		}
		kind := ref[0]
		applied := 0
		for i, r := range refs {
			if rows[r] == nil {
				continue
			}
			q := rows[r+"_q"]
			if q == nil {
				bad(&unsettled, "%s (%s) is saved as %s and never reached a final state", o.token, r, rows[r].Status)
				continue
			}
			p := o.ps[i]
			switch {
			case kind == 'h' && q.Status == "INFLIGHT":
				applied++ // its commits and void are counted below
			case q.Status == "APPLIED" && kind != 'h':
				// a rolled-back item of an atomic bulk is refunded: count its refund
				// (both moves stay in the volumes: the transfer and its reverse)
				if rf := rows[q.ID+"_refund"]; rf != nil && rf.Status == "APPLIED" {
					known[q.ID+"_refund"] = true
					want.post(p)
					want.post(posting{p.Dst, p.Src, p.Amt})
					continue
				}
				applied++
				want.post(p)
			case q.Status == "REJECTED":
			default:
				bad(&wrongResult, "%s (%s) ended %s", o.token, r, q.Status)
			}
		}
		switch kind {
		case 'd', 't', 'p', 'h':
			if applied != 1 {
				bad(&wrongResult, "%s was not applied: users never run short", o.token)
			}
		case 'o':
			if applied != 0 {
				bad(&wrongResult, "%s from the empty account was applied", o.token)
			}
		case 'k':
			fromEmpty := o.ps[1].Src == "e"
			if (fromEmpty && applied != 0) || (!fromEmpty && applied != 2 && present == 2) || (applied == 1) {
				bad(&notAtomic, "atomic bulk %s: %d of its 2 transfers applied", o.token, applied)
			}
		}
	}
	// holds: what was committed moves money; what is still held is inflight
	for _, ref := range order {
		o := ops[ref]
		if o.action != "" || ref[0] != 'h' {
			continue
		}
		leg := rows[ref+"_q"]
		if leg == nil || leg.Status != "INFLIGHT" {
			continue
		}
		p := o.ps[0]
		committed, voided := int64(0), false
		for _, a := range holds[ref] {
			row := actionRow(rows, leg, a)
			if row != nil {
				known[row.Ref] = true
			}
			switch a.outcome {
			case "ack":
				acked++
				if row == nil {
					bad(&missing, "acknowledged %s of hold %s is not in Blnk", a.token, o.token)
				}
			case "unknown":
				unk++
				if row != nil {
					unkIn++
				}
			default:
				if row != nil {
					bad(&refusedApplied, "refused %s of hold %s is in Blnk", a.token, o.token)
				}
			}
			if row == nil {
				continue
			}
			if a.action == "V" {
				voided = true
			} else if row.Status == "APPLIED" {
				committed += a.amount
				want.post(posting{p.Src, p.Dst, a.amount})
			}
		}
		if !voided {
			want.hold(p, p.Amt-committed)
		}
	}
	foreign := 0
	for ref := range rows {
		if !known[ref] {
			bad(&foreign, "transaction %s comes from no operation the client sent", ref)
		}
	}

	pg := vols{}
	for _, r := range readJSONL(pgBal) {
		a := strings.TrimPrefix(str(r, "indicator"), "@")
		if a == "world" {
			a = "w"
		}
		*pg.get(a) = [4]int64{num(r, "credit_balance"), num(r, "debit_balance"), num(r, "inflight_credit_balance"), num(r, "inflight_debit_balance")}
	}
	api := vols{}
	for a := range pg {
		st, b, err := do("GET", "/balances/indicator/"+url.PathEscape(indicator(a))+"/currency/USD", nil)
		if err != nil || st != 200 {
			fmt.Printf("the API does not answer for @%s: %d %v\n", a, st, err)
			return 2
		}
		d := json.NewDecoder(bytes.NewReader(b))
		d.UseNumber()
		var raw map[string]any
		if err := d.Decode(&raw); err != nil {
			fmt.Printf("bad balance answer for @%s: %s\n", a, trim(b))
			return 2
		}
		g := func(k string) int64 {
			n, _ := raw[k].(json.Number).Int64()
			return n
		}
		*api.get(a) = [4]int64{g("credit_balance"), g("debit_balance"), g("inflight_credit_balance"), g("inflight_debit_balance")}
	}
	dPG, dAPI := diff("postgres", want, pg), diff("api", want, api)
	for i, l := range append(dPG, dAPI...) {
		if i < 12 {
			say("%s", l)
		}
	}
	nbad := missing + refusedApplied + unsettled + wrongResult + notAtomic + foreign + len(dPG) + len(dAPI)
	summary := fmt.Sprintf("%d acknowledged and %d unanswered operations (%d of them in Blnk): %d missing, %d refused but applied, %d never final, %d with a wrong result, %d bulks not atomic, %d foreign; %d volumes differ in postgres, %d in the API",
		acked, unk, unkIn, missing, refusedApplied, unsettled, wrongResult, notAtomic, foreign, len(dPG), len(dAPI))
	say("%s", summary)
	if nbad > 0 {
		return 1
	}
	return 0
}
