// tkc is the test client of the TiKV example: the workload's operations, the read-backs of the
// history checks, and the final checks. It uses TiKV's documented Go client (client-go v2, the
// version of TiDB v8.5.8) with the API version 2, so that RawKV and TxnKV share the cluster.
//
//	tkc init              wait for three stores, then write the bank's accounts (first_init)
//	tkc op <kind>         one client operation; the composer's HYDO_* variables drive it
//	tkc readback <hist>   read every key back and record the reads (the history checks)
//	tkc check <rule>      one final or always rule over the client's records and the store
//	tkc get <key>         print one key, raw (r/...) or transactional (anything else)
//	tkc regions           print PD's regions with their leaders
//
// PD is TK_PD (default pd1:2379,pd2:2379,pd3:2379).
//
// Outcomes. A transaction whose commit returns no error is acknowledged. A transaction that
// failed before its commit, or whose commit failed with a write conflict, is failed: it wrote
// nothing. Every other error leaves the outcome unknown, because a commit can be applied after
// the client gave up (an undetermined result, a timeout, a lost answer). A raw write that
// returns an error is unknown. A raw compare-and-swap that answers "not swapped" is failed.
//
// The keys:
//
//	la/k0..la/k7      transactional lists of numbers, "1,2,3" (history "la")
//	reg/k0..reg/k4    raw registers, the value a number (history "reg"); CAS in atomic mode
//	acct/0..acct/7    transactional bank accounts, 100 each; transfers keep the total at 800
//	r/<id>, t/<id>    one raw or one transactional write per "put" operation, 2 to 8 KiB, so
//	                  that the regions grow and split
//
// $HYDO_ACKED gets r<id> or t<id> for every acknowledged put.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/rand/v2"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydohist"
	"github.com/pingcap/kvproto/pkg/kvrpcpb"
	plog "github.com/pingcap/log"
	"github.com/tikv/client-go/v2/config"
	tikverr "github.com/tikv/client-go/v2/error"
	"github.com/tikv/client-go/v2/rawkv"
	"github.com/tikv/client-go/v2/txnkv"
	"github.com/tikv/client-go/v2/txnkv/transaction"
)

const (
	nLA    = 8
	nReg   = 5
	nAcct  = 8
	start  = 100
	nTotal = nAcct * start
)

var pdAddrs = strings.Split(env("TK_PD", "pd1:2379,pd2:2379,pd3:2379"), ",")

func env(k, d string) string {
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

func quietLogs() {
	lg, p, err := plog.InitLogger(&plog.Config{Level: env("TK_LOG", "fatal")})
	if err == nil {
		plog.ReplaceGlobals(lg, p)
	}
	config.UpdateGlobal(func(c *config.Config) {
		c.TiKVClient.GrpcConnectionCount = 1
	})
}

// ---- clients ----

func apiOpt() kvrpcpb.APIVersion {
	if env("TK_API", "2") == "1" {
		return kvrpcpb.APIVersion_V1
	}
	return kvrpcpb.APIVersion_V2
}

func txnClient() (*txnkv.Client, error) {
	return txnkv.NewClient(pdAddrs, txnkv.WithAPIVersion(apiOpt()))
}

func rawClient(ctx context.Context) (*rawkv.Client, error) {
	c, err := rawkv.NewClientWithOpts(ctx, pdAddrs, rawkv.WithAPIVersion(apiOpt()))
	if err != nil {
		return nil, err
	}
	return c.SetAtomicForCAS(true), nil
}

type outcome int

const (
	ack outcome = iota
	failed
	unknown
)

func (o outcome) String() string { return [...]string{"ack", "failed", "unknown"}[o] }

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

func record(id string, o outcome) { appendLine(os.Getenv("HYDO_OUTCOMES"), id+" "+o.String()) }

func cmdID() int { return atoiDef(os.Getenv("HYDO_CMD_ID"), 0) }

func rng() *rand.Rand {
	s := uint64(atoiDef(os.Getenv("HYDO_SEED"), 0))
	return rand.New(rand.NewPCG(s, uint64(cmdID())))
}

func opCtx() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), time.Duration(atoiDef(os.Getenv("TK_TIMEOUT_S"), 12))*time.Second)
}

// commitOutcome judges a commit's error.
func commitOutcome(err error) outcome {
	switch {
	case err == nil:
		return ack
	case tikverr.IsErrWriteConflict(err):
		return failed
	default:
		return unknown
	}
}

// txnMode makes one transaction in three use async commit and one-phase commit, the faster
// commit paths that TiDB turns on by default; the others use the classic two-phase commit.
func txnMode(txn *transaction.KVTxn, id int) string {
	if id%3 == 0 {
		txn.SetEnableAsyncCommit(true)
		txn.SetEnable1PC(true)
		return "async"
	}
	return "2pc"
}

// ---- lists ----

func parseList(v []byte) []string {
	if len(v) == 0 {
		return nil
	}
	return strings.Split(string(v), ",")
}

// get reads a key in a transaction; a missing key is nil.
func get(ctx context.Context, txn *transaction.KVTxn, k string) ([]byte, error) {
	e, err := txn.Get(ctx, []byte(k))
	if tikverr.IsErrNotFound(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return e.Value, nil
}

// opLA is one list-append transaction of 1 to 4 micro-ops on la/k0..k7. An append reads the
// list and writes it back longer. Elements are unique: <cmd id>*10 + the micro-op's index.
func opLA() int {
	r := rng()
	id := cmdID()
	n := 1 + r.IntN(4)
	type mop struct {
		app  bool
		key  int
		elem int
	}
	ops := make([]mop, n)
	inv := make([]string, n)
	for i := range ops {
		ops[i] = mop{app: r.IntN(2) == 0, key: r.IntN(nLA), elem: id*10 + i}
		k := fmt.Sprintf("k%d", ops[i].key)
		if ops[i].app {
			inv[i] = hydohist.Append(k, strconv.Itoa(ops[i].elem))
		} else {
			inv[i] = hydohist.Read(k)
		}
	}
	h := hydohist.New("la")
	op := h.Invoke(id, "txn", inv...)
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		op.Fail()
		record(strconv.Itoa(id), failed)
		fmt.Fprintln(os.Stderr, "tkc: la: client:", err)
		return 1
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		op.Fail()
		record(strconv.Itoa(id), failed)
		fmt.Fprintln(os.Stderr, "tkc: la: begin:", err)
		return 1
	}
	mode := txnMode(txn, id)
	done := make([]string, n)
	for i, m := range ops {
		k := fmt.Sprintf("k%d", m.key)
		v, err := get(ctx, txn, "la/"+k)
		if err != nil {
			txn.Rollback()
			op.Fail()
			record(strconv.Itoa(id), failed)
			fmt.Fprintln(os.Stderr, "tkc: la: get:", err)
			return 1
		}
		l := parseList(v)
		if m.app {
			l = append(l, strconv.Itoa(m.elem))
			if err := txn.Set([]byte("la/"+k), []byte(strings.Join(l, ","))); err != nil {
				txn.Rollback()
				op.Fail()
				record(strconv.Itoa(id), failed)
				return 1
			}
			done[i] = inv[i]
		} else {
			done[i] = hydohist.ReadResult(k, l...)
		}
	}
	o := commitOutcome(txn.Commit(ctx))
	switch o {
	case ack:
		op.Ok(done...)
	case failed:
		op.Fail()
	default:
		op.Info()
	}
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "tkc: la:", mode, o)
		return 1
	}
	return 0
}

// ---- raw registers ----

func opReg() int {
	r := rng()
	id := cmdID()
	k := fmt.Sprintf("k%d", r.IntN(nReg))
	key := []byte("reg/" + k)
	h := hydohist.New("reg")
	ctx, cancel := opCtx()
	defer cancel()
	kind := r.IntN(3)
	old := strconv.Itoa(r.IntN(id + 1))
	if r.IntN(2) == 0 && id > 3 {
		old = strconv.Itoa(id - 1 - r.IntN(3))
	}
	v := strconv.Itoa(id)
	var op *hydohist.Op
	switch kind {
	case 0:
		op = h.Invoke(id, "read", k)
	case 1:
		op = h.Invoke(id, "write", k, v)
	default:
		op = h.Invoke(id, "cas", k, old, v)
	}
	c, err := rawClient(ctx)
	if err != nil {
		op.Fail()
		record(strconv.Itoa(id), failed)
		fmt.Fprintln(os.Stderr, "tkc: reg: client:", err)
		return 1
	}
	defer c.Close()
	o := unknown
	switch kind {
	case 0:
		val, err := c.Get(ctx, key)
		switch {
		case err != nil:
			o = failed // a read changes nothing
			op.Fail()
		case val == nil:
			o = ack
			op.Ok(k, "nil")
		default:
			o = ack
			op.Ok(k, string(val))
		}
	case 1:
		if err := c.Put(ctx, key, []byte(v)); err == nil {
			o = ack
			op.Ok()
		} else {
			op.Info()
		}
	default:
		_, swapped, err := c.CompareAndSwap(ctx, key, []byte(old), []byte(v))
		switch {
		case err != nil:
			op.Info()
		case swapped:
			o = ack
			op.Ok()
		default:
			o = failed
			op.Fail()
		}
	}
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "tkc: reg:", o)
		return 1
	}
	return 0
}

// ---- bank ----

func opBank() int {
	r := rng()
	id := cmdID()
	from := r.IntN(nAcct)
	to := (from + 1 + r.IntN(nAcct-1)) % nAcct
	amt := 1 + r.IntN(30)
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	mode := txnMode(txn, id)
	bal := func(a int) (int, error) {
		v, err := get(ctx, txn, fmt.Sprintf("acct/%d", a))
		if err != nil {
			return 0, err
		}
		if v == nil {
			return 0, fmt.Errorf("acct/%d is missing", a)
		}
		return strconv.Atoi(string(v))
	}
	bf, err1 := bal(from)
	bt, err2 := bal(to)
	if err := errors.Join(err1, err2); err != nil || bf < amt {
		txn.Rollback()
		record(strconv.Itoa(id), failed)
		if err != nil {
			fmt.Fprintln(os.Stderr, "tkc: bank:", err)
		}
		return 1
	}
	txn.Set([]byte(fmt.Sprintf("acct/%d", from)), []byte(strconv.Itoa(bf-amt)))
	txn.Set([]byte(fmt.Sprintf("acct/%d", to)), []byte(strconv.Itoa(bt+amt)))
	o := commitOutcome(txn.Commit(ctx))
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "tkc: bank:", mode, o)
		return 1
	}
	return 0
}

// opCrash is a client that crashes inside a commit: it prepares a transfer like opBank, writes
// the transaction's locks (Percolator's prewrite, with the default 3 s time to live) and exits
// before the commit. Percolator's answer to such a crash is the lock's time to live: once it has
// passed, the next reader or writer of the key rolls the transaction back. The transfer keeps
// the total, so the bank rules hold whichever way the transaction is resolved.
func opCrash() int {
	r := rng()
	id := cmdID()
	from := r.IntN(nAcct)
	to := (from + 1 + r.IntN(nAcct-1)) % nAcct
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	txn, err := c.Begin()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	bf, err1 := get(ctx, txn, fmt.Sprintf("acct/%d", from))
	bt, err2 := get(ctx, txn, fmt.Sprintf("acct/%d", to))
	if err := errors.Join(err1, err2); err != nil || bf == nil || bt == nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	nf, _ := strconv.Atoi(string(bf))
	nt, _ := strconv.Atoi(string(bt))
	amt := 1
	if nf < amt {
		record(strconv.Itoa(id), failed)
		return 1
	}
	txn.Set([]byte(fmt.Sprintf("acct/%d", from)), []byte(strconv.Itoa(nf-amt)))
	txn.Set([]byte(fmt.Sprintf("acct/%d", to)), []byte(strconv.Itoa(nt+amt)))
	cm, err := transaction.TxnProbe{KVTxn: txn}.NewCommitter(0)
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	if err := cm.PrewriteAllMutations(ctx); err != nil {
		record(strconv.Itoa(id), unknown)
		fmt.Fprintln(os.Stderr, "tkc: crash: prewrite:", trunc(err.Error(), 200))
		return 1
	}
	record(strconv.Itoa(id), unknown)
	fmt.Printf("tkc: crash: transaction %d locked acct/%d and acct/%d and exits before its commit\n", txn.StartTS(), from, to)
	os.Exit(0) // no Close, no commit, no rollback: the locks stay
	return 0
}

// ---- unique writes ----

// pad is the value of put <id>: n bytes that depend only on the id, so a check can recompute it.
func pad(id int) []byte {
	n := 2048 + (id*7919)%6144
	b := make([]byte, n)
	x := uint32(id)*2654435761 + 1
	for i := range b {
		x ^= x << 13
		x ^= x >> 17
		x ^= x << 5
		b[i] = 'a' + byte(x%26)
	}
	return b
}

func opPut() int {
	id := cmdID()
	ctx, cancel := opCtx()
	defer cancel()
	if id%2 == 0 {
		c, err := rawClient(ctx)
		if err != nil {
			record(strconv.Itoa(id), failed)
			return 1
		}
		defer c.Close()
		if err := c.Put(ctx, []byte(fmt.Sprintf("r/%d", id)), pad(id)); err != nil {
			record(strconv.Itoa(id), unknown)
			fmt.Fprintln(os.Stderr, "tkc: put raw: unknown")
			return 1
		}
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("r%d", id))
		record(strconv.Itoa(id), ack)
		return 0
	}
	c, err := txnClient()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		record(strconv.Itoa(id), failed)
		return 1
	}
	mode := txnMode(txn, id)
	txn.Set([]byte(fmt.Sprintf("t/%d", id)), pad(id))
	o := commitOutcome(txn.Commit(ctx))
	record(strconv.Itoa(id), o)
	if o != ack {
		fmt.Fprintln(os.Stderr, "tkc: put txn:", mode, o)
		return 1
	}
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("t%d", id))
	return 0
}

// ---- PD: regions and operators ----

var pdHTTP = &http.Client{Timeout: 4 * time.Second}

func pdGet(path string, out any) error {
	var last error
	for _, a := range pdAddrs {
		resp, err := pdHTTP.Get("http://" + a + path)
		if err != nil {
			last = err
			continue
		}
		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil || resp.StatusCode != 200 {
			last = fmt.Errorf("GET %s: %d %s", path, resp.StatusCode, trunc(string(body), 200))
			continue
		}
		return json.Unmarshal(body, out)
	}
	return last
}

func pdPost(path string, in any) error {
	b, _ := json.Marshal(in)
	var last error
	for _, a := range pdAddrs {
		resp, err := pdHTTP.Post("http://"+a+path, "application/json", bytes.NewReader(b))
		if err != nil {
			last = err
			continue
		}
		body, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if resp.StatusCode != 200 {
			last = fmt.Errorf("POST %s: %d %s", path, resp.StatusCode, trunc(string(body), 200))
			continue
		}
		return nil
	}
	return last
}

func trunc(s string, n int) string {
	if len(s) > n {
		return s[:n] + "..."
	}
	return s
}

type peer struct {
	ID      uint64 `json:"id"`
	StoreID uint64 `json:"store_id"`
}

type region struct {
	ID       uint64 `json:"id"`
	StartKey string `json:"start_key"`
	EndKey   string `json:"end_key"`
	Peers    []peer `json:"peers"`
	Leader   peer   `json:"leader"`
}

type regions struct {
	Count   int      `json:"count"`
	Regions []region `json:"regions"`
}

type stores struct {
	Count  int `json:"count"`
	Stores []struct {
		Store struct {
			ID        uint64 `json:"id"`
			Address   string `json:"address"`
			StateName string `json:"state_name"`
		} `json:"store"`
	} `json:"stores"`
}

// opMove asks PD for one change of the regions: a leader moves to another store, a region splits
// in half, or two neighbouring regions merge. The workload's keys stay where they are; only the
// regions and their leaders change. Its outcome is not recorded: PD may refuse or drop it.
func opMove() int {
	r := rng()
	var rs regions
	if err := pdGet("/pd/api/v1/regions", &rs); err != nil || len(rs.Regions) == 0 {
		fmt.Fprintln(os.Stderr, "tkc: move: regions:", err)
		return 0
	}
	sort.Slice(rs.Regions, func(i, j int) bool { return rs.Regions[i].StartKey < rs.Regions[j].StartKey })
	i := r.IntN(len(rs.Regions))
	g := rs.Regions[i]
	var err error
	var what string
	switch r.IntN(4) {
	case 0, 1:
		var to []uint64
		for _, p := range g.Peers {
			if p.StoreID != g.Leader.StoreID {
				to = append(to, p.StoreID)
			}
		}
		if len(to) == 0 {
			return 0
		}
		s := to[r.IntN(len(to))]
		what = fmt.Sprintf("transfer the leader of region %d to store %d", g.ID, s)
		err = pdPost("/pd/api/v1/operators", map[string]any{"name": "transfer-leader", "region_id": g.ID, "to_store_id": s})
	case 2:
		what = fmt.Sprintf("split region %d", g.ID)
		err = pdPost("/pd/api/v1/operators", map[string]any{"name": "split-region", "region_id": g.ID, "policy": "approximate"})
	default:
		if i+1 >= len(rs.Regions) {
			return 0
		}
		what = fmt.Sprintf("merge region %d into %d", rs.Regions[i+1].ID, g.ID)
		err = pdPost("/pd/api/v1/operators", map[string]any{"name": "merge-region", "source_region_id": rs.Regions[i+1].ID, "target_region_id": g.ID})
	}
	if err != nil {
		fmt.Fprintln(os.Stderr, "tkc: move:", what+":", err)
		return 0
	}
	fmt.Println("tkc: move:", what)
	return 0
}

func cmdRegions() int {
	var rs regions
	if err := pdGet("/pd/api/v1/regions", &rs); err != nil {
		fmt.Println("regions:", err)
		return 2
	}
	sort.Slice(rs.Regions, func(i, j int) bool { return rs.Regions[i].StartKey < rs.Regions[j].StartKey })
	for _, g := range rs.Regions {
		fmt.Printf("region %d [%s, %s) leader store %d, %d peers\n", g.ID, g.StartKey, g.EndKey, g.Leader.StoreID, len(g.Peers))
	}
	return 0
}

// ---- init ----

func storesUp() (int, error) {
	var ss stores
	if err := pdGet("/pd/api/v1/stores", &ss); err != nil {
		return 0, err
	}
	up := 0
	for _, s := range ss.Stores {
		if s.Store.StateName == "Up" {
			up++
		}
	}
	return up, nil
}

func cmdInit() int {
	deadline := time.Now().Add(150 * time.Second)
	for {
		up, err := storesUp()
		if up == 3 {
			break
		}
		if time.Now().After(deadline) {
			fmt.Println("tkc: init: stores up:", up, err)
			return 1
		}
		time.Sleep(time.Second)
	}
	for try := 0; try < 60; try++ {
		if err := initAccounts(); err == nil {
			fmt.Println("tkc: init done")
			return 0
		} else {
			fmt.Fprintln(os.Stderr, "tkc: init:", err)
		}
		time.Sleep(2 * time.Second)
	}
	return 1
}

func initAccounts() error {
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		return err
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		return err
	}
	for a := 0; a < nAcct; a++ {
		txn.Set([]byte(fmt.Sprintf("acct/%d", a)), []byte(strconv.Itoa(start)))
	}
	if err := txn.Commit(ctx); err != nil {
		return err
	}
	rc, err := rawClient(ctx)
	if err != nil {
		return err
	}
	defer rc.Close()
	// a raw write and read, so that the raw path works before the workload
	if err := rc.Put(ctx, []byte("reg/init"), []byte("0")); err != nil {
		return err
	}
	_, err = rc.Get(ctx, []byte("reg/init"))
	return err
}

// ---- read-backs ----

// retry runs f until it succeeds or 120 s pass: after a power cut the stores elect their region
// leaders again, and PD its own leader.
func retry(what string, f func() error) error {
	deadline := time.Now().Add(time.Duration(atoiDef(os.Getenv("TK_READ_S"), 120)) * time.Second)
	var err error
	for {
		if err = f(); err == nil {
			return nil
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("%s: %w", what, err)
		}
		time.Sleep(time.Second)
	}
}

// snapshotRead reads keys in one transaction's snapshot.
func snapshotRead(keys []string) (map[string][]byte, error) {
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		return nil, err
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		return nil, err
	}
	defer txn.Rollback()
	bk := make([][]byte, len(keys))
	for i, k := range keys {
		bk[i] = []byte(k)
	}
	m, err := txn.BatchGet(ctx, bk)
	if err != nil {
		return nil, err
	}
	out := map[string][]byte{}
	for k, e := range m {
		out[k] = e.Value
	}
	return out, nil
}

func readback(hist string) int {
	h := hydohist.New(hist)
	proc := 1000000
	switch hist {
	case "la":
		var keys []string
		for i := 0; i < nLA; i++ {
			keys = append(keys, fmt.Sprintf("la/k%d", i))
		}
		var m map[string][]byte
		if err := retry("la read-back", func() (e error) { m, e = snapshotRead(keys); return }); err != nil {
			fmt.Println("the store did not answer the read-back:", err)
			return 2
		}
		for i := 0; i < nLA; i++ {
			k := fmt.Sprintf("k%d", i)
			op := h.Invoke(proc+i, "txn", hydohist.Read(k))
			op.Ok(hydohist.ReadResult(k, parseList(m["la/"+k])...))
		}
	case "reg":
		ctx := context.Background()
		var c *rawkv.Client
		if err := retry("raw client", func() (e error) { c, e = rawClient(ctx); return }); err != nil {
			fmt.Println("the store did not answer the read-back:", err)
			return 2
		}
		defer c.Close()
		for i := 0; i < nReg; i++ {
			k := fmt.Sprintf("k%d", i)
			var v []byte
			if err := retry("reg read-back", func() (e error) {
				cx, cancel := opCtx()
				defer cancel()
				v, e = c.Get(cx, []byte("reg/"+k))
				return
			}); err != nil {
				fmt.Println("the store did not answer the read-back:", err)
				return 2
			}
			op := h.Invoke(proc+i, "read", k)
			if v == nil {
				op.Ok(k, "nil")
			} else {
				op.Ok(k, string(v))
			}
		}
	default:
		fmt.Fprintln(os.Stderr, "tkc readback: la or reg")
		return 2
	}
	return 0
}

// ---- checks ----

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

func checkBank(final bool) int {
	var keys []string
	for a := 0; a < nAcct; a++ {
		keys = append(keys, fmt.Sprintf("acct/%d", a))
	}
	var m map[string][]byte
	var err error
	if final {
		err = retry("bank read", func() (e error) { m, e = snapshotRead(keys); return })
	} else {
		m, err = snapshotRead(keys)
	}
	if err != nil {
		fmt.Println("the store did not answer:", trunc(err.Error(), 300))
		if !final {
			// a snapshot that cannot be read now breaks nothing here: eventually_readable
			// judges whether the accounts become readable again
			fmt.Println("no snapshot of the accounts could be read now")
			return 0
		}
		return 2
	}
	sum, neg, missing := 0, 0, 0
	var parts []string
	for _, k := range keys {
		v, ok := m[k]
		if !ok {
			missing++
			parts = append(parts, k+"=missing")
			continue
		}
		n, _ := strconv.Atoi(string(v))
		sum += n
		if n < 0 {
			neg++
		}
		parts = append(parts, fmt.Sprintf("%s=%d", k, n))
	}
	if sum != nTotal || neg > 0 || missing > 0 {
		fmt.Println(strings.Join(parts, " "))
		fmt.Printf("one snapshot of the accounts holds %d in all, not %d (%d negative, %d missing)\n", sum, nTotal, neg, missing)
		return 1
	}
	fmt.Printf("the accounts hold %d\n", sum)
	return 0
}

// checkDurable reads every acknowledged put back.
func checkDurable() int {
	acked := readLines(os.Getenv("HYDO_ACKED"))
	var raw, txn []int
	for _, a := range acked {
		if len(a) < 2 {
			continue
		}
		n, err := strconv.Atoi(a[1:])
		if err != nil {
			continue
		}
		switch a[0] {
		case 'r':
			raw = append(raw, n)
		case 't':
			txn = append(txn, n)
		}
	}
	var lost, wrong []string
	ctx := context.Background()
	if len(raw) > 0 {
		var c *rawkv.Client
		if err := retry("raw client", func() (e error) { c, e = rawClient(ctx); return }); err != nil {
			fmt.Println("the store did not answer:", err)
			return 2
		}
		defer c.Close()
		for _, id := range raw {
			var v []byte
			if err := retry("raw read", func() (e error) {
				cx, cancel := opCtx()
				defer cancel()
				v, e = c.Get(cx, []byte(fmt.Sprintf("r/%d", id)))
				return
			}); err != nil {
				fmt.Println("the store did not answer:", err)
				return 2
			}
			if v == nil {
				lost = append(lost, fmt.Sprintf("r%d", id))
			} else if !bytes.Equal(v, pad(id)) {
				wrong = append(wrong, fmt.Sprintf("r%d", id))
			}
		}
	}
	if len(txn) > 0 {
		var keys []string
		for _, id := range txn {
			keys = append(keys, fmt.Sprintf("t/%d", id))
		}
		var m map[string][]byte
		if err := retry("txn read", func() (e error) { m, e = snapshotRead(keys); return }); err != nil {
			fmt.Println("the store did not answer:", err)
			return 2
		}
		for _, id := range txn {
			v, ok := m[fmt.Sprintf("t/%d", id)]
			if !ok {
				lost = append(lost, fmt.Sprintf("t%d", id))
			} else if !bytes.Equal(v, pad(id)) {
				wrong = append(wrong, fmt.Sprintf("t%d", id))
			}
		}
	}
	if len(lost)+len(wrong) > 0 {
		fmt.Println("lost:", strings.Join(lost, " "))
		fmt.Println("wrong value:", strings.Join(wrong, " "))
		fmt.Printf("%d of %d acknowledged writes are lost and %d hold another value (raw %d, transactional %d)\n",
			len(lost), len(raw)+len(txn), len(wrong), len(raw), len(txn))
		return 1
	}
	fmt.Printf("all %d acknowledged writes are there (raw %d, transactional %d)\n", len(raw)+len(txn), len(raw), len(txn))
	return 0
}

// checkReadable reads every transactional key of the workload in one snapshot, once. A key
// that a failed transaction left locked is readable again after the lock's time to live, when a
// reader resolves the lock (3 s for the workload's small transactions).
func checkReadable() int {
	var keys []string
	for i := 0; i < nLA; i++ {
		keys = append(keys, fmt.Sprintf("la/k%d", i))
	}
	for a := 0; a < nAcct; a++ {
		keys = append(keys, fmt.Sprintf("acct/%d", a))
	}
	if _, err := snapshotRead(keys); err != nil {
		fmt.Println("the lists and the accounts cannot be read in one snapshot:", trunc(err.Error(), 300))
		return 1
	}
	fmt.Println("the lists and the accounts can be read")
	return 0
}

// checkWritable writes every account back with its own value in one transaction, once. A write
// that meets the lock of a crashed transaction asks for the lock's status; once the lock's time
// to live has passed, TiKV rolls that transaction back and the write goes on.
func checkWritable() int {
	ctx, cancel := opCtx()
	defer cancel()
	c, err := txnClient()
	if err != nil {
		fmt.Println("no client:", trunc(err.Error(), 200))
		return 1
	}
	defer c.Close()
	txn, err := c.Begin()
	if err != nil {
		fmt.Println("no transaction:", trunc(err.Error(), 200))
		return 1
	}
	var keys [][]byte
	for a := 0; a < nAcct; a++ {
		keys = append(keys, []byte(fmt.Sprintf("acct/%d", a)))
	}
	m, err := txn.BatchGet(ctx, keys)
	if err != nil {
		txn.Rollback()
		fmt.Println("the accounts cannot be read:", trunc(err.Error(), 300))
		return 1
	}
	for _, k := range keys {
		if e, ok := m[string(k)]; ok {
			txn.Set(k, e.Value)
		}
	}
	if err := txn.Commit(ctx); err != nil {
		fmt.Println("the accounts cannot be written:", trunc(err.Error(), 300))
		return 1
	}
	fmt.Println("the accounts can be written")
	return 0
}

func cmdGet(k string) int {
	ctx, cancel := opCtx()
	defer cancel()
	if strings.HasPrefix(k, "r/") || strings.HasPrefix(k, "reg/") {
		c, err := rawClient(ctx)
		if err != nil {
			fmt.Println(err)
			return 2
		}
		defer c.Close()
		v, err := c.Get(ctx, []byte(k))
		fmt.Printf("%s = %q (%v)\n", k, trunc(string(v), 200), err)
		return 0
	}
	m, err := snapshotRead([]string{k})
	fmt.Printf("%s = %q (%v)\n", k, trunc(string(m[k]), 200), err)
	return 0
}

func main() {
	quietLogs()
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: tkc init|op <kind>|readback <hist>|check <rule>|get <key>|regions")
		os.Exit(2)
	}
	arg := ""
	if len(os.Args) > 2 {
		arg = os.Args[2]
	}
	switch os.Args[1] {
	case "init":
		os.Exit(cmdInit())
	case "op":
		switch arg {
		case "la":
			os.Exit(opLA())
		case "reg":
			os.Exit(opReg())
		case "bank":
			os.Exit(opBank())
		case "put":
			os.Exit(opPut())
		case "move":
			os.Exit(opMove())
		case "crash":
			os.Exit(opCrash())
		}
	case "readback":
		os.Exit(readback(arg))
	case "check":
		switch arg {
		case "bank":
			os.Exit(checkBank(true))
		case "bank-live":
			os.Exit(checkBank(false))
		case "durable":
			os.Exit(checkDurable())
		case "readable":
			os.Exit(checkReadable())
		case "writable":
			os.Exit(checkWritable())
		}
	case "get":
		os.Exit(cmdGet(arg))
	case "regions":
		os.Exit(cmdRegions())
	}
	fmt.Fprintln(os.Stderr, "tkc: unknown command")
	os.Exit(2)
}
