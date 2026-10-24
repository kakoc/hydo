// Package client is the guest side: it drives a project's RPCs with generated requests and
// records every call's outcome. One static binary (cmd/hydo-grpc) serves every project; the
// project's schema and hints come from grpc/desc.pb and grpc/plan.json.
package client

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/dynamicpb"

	"hydogrpc/internal/outcome"
	"hydogrpc/internal/record"
	"hydogrpc/internal/rng"
	"hydogrpc/internal/schema"
	"hydogrpc/internal/spec"
	"hydogrpc/internal/valuegen"
)

// Client drives one plan.
type Client struct {
	Plan  *spec.Plan
	Files *protoregistry.Files
	Rec   *record.Recorder
	State *State
	Seed  uint64

	mu       sync.Mutex
	conns    map[string]*grpc.ClientConn
	outcomes *os.File             // HYDO_OUTCOMES: "<id> ack|failed|unknown"
	acked    *os.File             // HYDO_ACKED: "<id>"
	captures map[string][]capture // source rpc -> pools it feeds
	asserted map[string]bool
	stderr   io.Writer
}

type capture struct {
	side, path, pool string
}

// Options for New.
type Options struct {
	Desc, Plan, StateDir, Calls string
	Seed                        *uint64
}

// New loads the schema and plan and opens the record sinks.
func New(o Options) (*Client, error) {
	files, err := schema.Read(o.Desc)
	if err != nil {
		return nil, err
	}
	p, err := spec.Load(o.Plan)
	if err != nil {
		return nil, err
	}
	st, err := NewState(o.StateDir)
	if err != nil {
		return nil, err
	}
	c := &Client{Plan: p, Files: files, State: st, conns: map[string]*grpc.ClientConn{},
		captures: map[string][]capture{}, asserted: map[string]bool{}, stderr: os.Stderr}
	c.Seed = p.Seed
	if o.Seed != nil {
		c.Seed = *o.Seed
	}
	calls := o.Calls
	if calls == "" {
		calls = o.StateDir + "/calls"
	}
	c.Rec = record.Open(calls, p.Record.Console)
	if v := os.Getenv("HYDO_OUTCOMES"); v != "" {
		c.outcomes, _ = os.OpenFile(v, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	}
	if v := os.Getenv("HYDO_ACKED"); v != "" {
		c.acked, _ = os.OpenFile(v, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	}
	for _, r := range p.RPCs {
		for _, path := range r.FieldPaths() {
			if f := r.Fields[path].From; f != nil {
				cs := c.captures[f.RPC]
				dup := false
				for _, x := range cs {
					dup = dup || x.pool == f.Pool()
				}
				if !dup {
					c.captures[f.RPC] = append(cs, capture{f.Side, f.Path, f.Pool()})
				}
			}
		}
	}
	return c, nil
}

// Close closes connections.
func (c *Client) Close() {
	c.mu.Lock()
	defer c.mu.Unlock()
	ts := make([]string, 0, len(c.conns))
	for t := range c.conns {
		ts = append(ts, t)
	}
	sort.Strings(ts)
	for _, t := range ts {
		c.conns[t].Close()
	}
}

func (c *Client) conn(target string) (*grpc.ClientConn, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if cc, ok := c.conns[target]; ok {
		return cc, nil
	}
	cc, err := grpc.NewClient("passthrough:///"+target, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, err
	}
	c.conns[target] = cc
	return cc, nil
}

func (c *Client) target(r *spec.RPC) string {
	if r.Target != "" {
		return r.Target
	}
	return c.Plan.Target
}

// worker env: unique values from the shared counter, `from` values from the shared pools,
// preferring what this worker captured itself during a scenario.
type env struct {
	c     *Client
	local map[string][]string
	scen  bool
}

func (e *env) Unique() uint64 { return e.c.State.Next("unique") }

func (e *env) Take(f *spec.From, r *rng.R) (string, bool) {
	if vs := e.local[f.Pool()]; len(vs) > 0 && (e.scen || f.Pick == "latest") {
		return vs[len(vs)-1], true
	}
	return e.c.State.Take(f, r)
}

// Generate builds n requests for an RPC (n = 1 unless client-streaming).
func (c *Client) Generate(r *spec.RPC, rnd *rng.R, e valuegen.Env) ([]proto.Message, error) {
	md, err := schema.Message(c.Files, r.Input)
	if err != nil {
		return nil, err
	}
	g := &valuegen.Gen{D: c.Plan.Defaults, Hints: r.Fields, Oneofs: r.Oneofs, Env: e}
	n := 1
	if r.ClientStreaming {
		n = int(rnd.Range(int64(r.StreamLen[0]), int64(r.StreamLen[1])))
	}
	var out []proto.Message
	for i := 0; i < n; i++ {
		m, err := g.Message(md, rnd)
		if err != nil {
			return nil, err
		}
		out = append(out, m)
	}
	return out, nil
}

// JSON renders messages as compact protobuf JSON (an array for a stream). protojson's own
// output varies its whitespace on purpose; json.Compact makes it canonical.
func JSON(ms []proto.Message, stream bool) string {
	var parts []string
	for _, m := range ms {
		b, err := protojson.MarshalOptions{UseProtoNames: true}.Marshal(m)
		if err != nil {
			parts = append(parts, `{"hydo_grpc_error":`+strconv.Quote(err.Error())+`}`)
			continue
		}
		var buf bytes.Buffer
		if json.Compact(&buf, b) != nil {
			buf.Reset()
			buf.Write(b)
		}
		parts = append(parts, buf.String())
	}
	if !stream && len(parts) == 1 {
		return parts[0]
	}
	return "[" + strings.Join(parts, ",") + "]"
}

// ParseJSON reads requests back from JSON (the inverse of JSON).
func (c *Client) ParseJSON(r *spec.RPC, s string) ([]proto.Message, error) {
	md, err := schema.Message(c.Files, r.Input)
	if err != nil {
		return nil, err
	}
	var raws []json.RawMessage
	if strings.HasPrefix(strings.TrimSpace(s), "[") {
		if err := json.Unmarshal([]byte(s), &raws); err != nil {
			return nil, err
		}
	} else {
		raws = []json.RawMessage{json.RawMessage(s)}
	}
	var out []proto.Message
	for _, raw := range raws {
		m := dynamicpb.NewMessage(md)
		if err := protojson.Unmarshal(raw, m); err != nil {
			return nil, err
		}
		out = append(out, m)
	}
	return out, nil
}

// Call makes one attempt and classifies it.
func (c *Client) Call(r *spec.RPC, reqs []proto.Message) (outcome.Result, []proto.Message) {
	omd, err := schema.Message(c.Files, r.Output)
	if err != nil {
		return outcome.Result{Kind: outcome.Unknown, Msg: err.Error()}, nil
	}
	cc, err := c.conn(c.target(r))
	if err != nil {
		return outcome.Classify(err, r.Classify), nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(r.DeadlineMs)*time.Millisecond)
	defer cancel()
	if !r.ClientStreaming && !r.ServerStreaming {
		resp := dynamicpb.NewMessage(omd)
		err := cc.Invoke(ctx, r.Path, reqs[0], resp)
		res := outcome.Classify(err, r.Classify)
		if err != nil {
			return res, nil
		}
		return res, []proto.Message{resp}
	}
	st, err := cc.NewStream(ctx, &grpc.StreamDesc{StreamName: r.Name, ClientStreams: r.ClientStreaming, ServerStreams: r.ServerStreaming}, r.Path)
	if err != nil {
		return outcome.Classify(err, r.Classify), nil
	}
	for _, q := range reqs {
		if err := st.SendMsg(q); err != nil {
			if err == io.EOF {
				break // the server ended the stream; RecvMsg returns its status
			}
			return outcome.Classify(err, r.Classify), nil
		}
	}
	if err := st.CloseSend(); err != nil {
		return outcome.Classify(err, r.Classify), nil
	}
	var resps []proto.Message
	for {
		resp := dynamicpb.NewMessage(omd)
		err := st.RecvMsg(resp)
		if err == io.EOF {
			break
		}
		if err != nil {
			return outcome.Classify(err, r.Classify), resps
		}
		resps = append(resps, resp)
		if !r.ServerStreaming {
			break
		}
	}
	return outcome.Classify(nil, r.Classify), resps
}

// Op runs one logical operation: an attempt, then up to Retry.Attempts more with the SAME
// request while the outcome is unknown. It records every attempt.
func (c *Client) Op(process int64, op uint64, r *spec.RPC, reqs []proto.Message, e *env) outcome.Result {
	body := JSON(reqs, r.ClientStreaming)
	var res outcome.Result
	for a := 0; a <= r.Retry.Attempts; a++ {
		if a > 0 && r.Retry.BackoffMs > 0 {
			time.Sleep(time.Duration(r.Retry.BackoffMs) * time.Millisecond)
		}
		res = c.attempt(process, op, r, reqs, body, e)
		if res.Kind != outcome.Unknown {
			break
		}
	}
	return res
}

func (c *Client) attempt(process int64, op uint64, r *spec.RPC, reqs []proto.Message, body string, e *env) outcome.Result {
	c.Rec.Invoke(process, int64(op), r.Name, body)
	res, resps := c.Call(r, reqs)
	out := res.Msg
	if res.Kind == outcome.OK {
		out = JSON(resps, r.ServerStreaming)
	}
	c.Rec.Complete(process, int64(op), string(res.Kind), r.Name, outcome.CodeName(res.Code), out)
	c.assertSeen(r, res.Kind)
	if id := c.outcomeID(r, reqs); id != "" {
		c.mu.Lock()
		if c.outcomes != nil {
			c.outcomes.Write([]byte(id + " " + res.Kind.Word() + "\n"))
		}
		if c.acked != nil && res.Kind == outcome.OK {
			c.acked.Write([]byte(id + "\n"))
		}
		c.mu.Unlock()
	}
	// feed the pools: request fields after an ok or unknown outcome (the entity may exist),
	// response fields after an ok
	for _, cp := range c.captures[r.Name] {
		var vals []string
		switch {
		case cp.side == "request" && res.Kind != outcome.Fail:
			for _, q := range reqs {
				vals = append(vals, Extract(q.ProtoReflect(), cp.path)...)
			}
		case cp.side == "response" && res.Kind == outcome.OK:
			for _, m := range resps {
				vals = append(vals, Extract(m.ProtoReflect(), cp.path)...)
			}
		}
		c.State.Put(cp.pool, vals)
		if e != nil {
			e.local[cp.pool] = append(e.local[cp.pool], vals...)
		}
	}
	return res
}

// assertSeen emits one HYDO-ASSERT record (type reachable) the first time in a run that an
// RPC ends ok / with a definite failure / with an unknown outcome, so the explorer's property
// table shows which outcomes the run exercised.
func (c *Client) assertSeen(r *spec.RPC, k outcome.Kind) {
	word := map[outcome.Kind]string{outcome.OK: "ok", outcome.Fail: "failed", outcome.Unknown: "unknown"}[k]
	id := "grpc." + r.Short + "." + word
	c.mu.Lock()
	seen := c.asserted[id]
	c.asserted[id] = true
	c.mu.Unlock()
	if seen {
		return
	}
	if c.State.Next("assert-"+id) != 1 {
		return // another process of this run already reported it
	}
	msg := fmt.Sprintf("%s ended %s", r.Name, word)
	rec, _ := json.Marshal(struct {
		T   string `json:"t"`
		ID  string `json:"id"`
		OK  bool   `json:"ok"`
		Msg string `json:"msg"`
	}{"reachable", id, true, msg})
	c.Rec.Raw("HYDO-ASSERT " + string(rec))
}

func (c *Client) outcomeID(r *spec.RPC, reqs []proto.Message) string {
	if r.OutcomeID == "" || len(reqs) == 0 {
		return ""
	}
	vs := Extract(reqs[0].ProtoReflect(), r.OutcomeID)
	if len(vs) == 0 {
		return ""
	}
	return strings.NewReplacer(" ", "_", "\n", "_").Replace(vs[0])
}

// Extract returns the canonical string form of every value at a field path (a repeated field
// on the way gives one value per element; an unset field gives none).
func Extract(m protoreflect.Message, path string) []string {
	segs := strings.Split(path, ".")
	var out []string
	var walk func(m protoreflect.Message, i int)
	walk = func(m protoreflect.Message, i int) {
		fd := m.Descriptor().Fields().ByName(protoreflect.Name(segs[i]))
		if fd == nil {
			return
		}
		last := i == len(segs)-1
		switch {
		case fd.IsMap():
			if i+1 >= len(segs) {
				return
			}
			sel := segs[i+1]
			m.Get(fd).Map().Range(func(k protoreflect.MapKey, v protoreflect.Value) bool {
				if sel == "key" {
					out = append(out, valuegen.Canonical(fd.MapKey(), k.Value()))
				} else if i+2 < len(segs) && fd.MapValue().Message() != nil {
					walk(v.Message(), i+2)
				} else if i+2 == len(segs) {
					out = append(out, valuegen.Canonical(fd.MapValue(), v))
				}
				return true
			})
			sort.Strings(out) // map order is random; the pool must not depend on it
		case fd.IsList():
			l := m.Get(fd).List()
			for j := 0; j < l.Len(); j++ {
				if last {
					out = append(out, valuegen.Canonical(fd, l.Get(j)))
				} else if fd.Message() != nil {
					walk(l.Get(j).Message(), i+1)
				}
			}
		case last:
			if fd.HasPresence() && !m.Has(fd) {
				return
			}
			out = append(out, valuegen.Canonical(fd, m.Get(fd)))
		case fd.Message() != nil && m.Has(fd):
			walk(m.Get(fd).Message(), i+1)
		}
	}
	walk(m, 0)
	return out
}

// item is what a worker does next: one RPC or a scenario.
type item struct {
	rpc  *spec.RPC
	scen *spec.Scenario
}

func (c *Client) items() ([]item, []int) {
	var its []item
	var ws []int
	for i := range c.Plan.RPCs {
		if r := &c.Plan.RPCs[i]; r.Weight > 0 {
			its, ws = append(its, item{rpc: r}), append(ws, r.Weight)
		}
	}
	for i := range c.Plan.Scenarios {
		if s := &c.Plan.Scenarios[i]; s.Weight > 0 {
			its, ws = append(its, item{scen: s}), append(ws, s.Weight)
		}
	}
	return its, ws
}

func pick(r *rng.R, ws []int) int {
	tot := 0
	for _, w := range ws {
		tot += w
	}
	x := r.Intn(tot)
	for i, w := range ws {
		if x < w {
			return i
		}
		x -= w
	}
	return len(ws) - 1
}

// maxSkips bounds how often a worker redraws because a `from` field has no value yet.
const maxSkips = 16

// one runs a single RPC operation with generated values; ran is false if it had to be
// skipped, ok whether it ended ok.
func (c *Client) one(process int64, r *spec.RPC, rnd *rng.R, e *env) (ran, ok bool) {
	reqs, err := c.Generate(r, rnd, e)
	if errors.Is(err, valuegen.ErrSkip) {
		return false, true
	}
	if err != nil {
		fmt.Fprintf(c.stderr, "hydo-grpc: %s: %v\n", r.Name, err)
		return false, true
	}
	return true, c.Op(process, c.State.Next("op"), r, reqs, e).Kind == outcome.OK
}

func (c *Client) runItem(process int64, it item, rnd *rng.R, e *env) (ran, ok bool) {
	if it.rpc != nil {
		return c.one(process, it.rpc, rnd, e)
	}
	e.scen = true
	defer func() { e.scen = false }()
	ok = true
	for _, name := range it.scen.Calls {
		r, err := c.Plan.Find(name)
		if err != nil {
			fmt.Fprintf(c.stderr, "hydo-grpc: scenario %s: %v\n", it.scen.Name, err)
			return ran, ok
		}
		did, good := c.one(process, r, rnd, e)
		if !did {
			return ran, ok // a later call needs a value an earlier one did not produce
		}
		ran, ok = true, ok && good
	}
	return ran, ok
}

// Steps runs workload steps [from, to): every step, Concurrency workers in parallel each
// run Calls operations drawn by weight. Worker w of step s draws from the stream
// Derive(seed, 1, s, w), so the requests of a step do not depend on how earlier steps went
// (except for values taken from earlier responses).
func (c *Client) Steps(from, to int) {
	its, ws := c.items()
	if len(its) == 0 {
		fmt.Fprintln(c.stderr, "hydo-grpc: no RPC has a weight > 0")
		return
	}
	for s := from; s < to; s++ {
		var wg sync.WaitGroup
		for w := 0; w < c.Plan.Steps.Concurrency; w++ {
			wg.Add(1)
			go func(w int) {
				defer wg.Done()
				rnd := rng.New(rng.Derive(c.Seed, 1, uint64(s), uint64(w)))
				e := &env{c: c, local: map[string][]string{}}
				for k := 0; k < c.Plan.Steps.Calls; k++ {
					for tries := 0; tries < maxSkips; tries++ {
						if ran, _ := c.runItem(int64(w), its[pick(rnd, ws)], rnd, e); ran {
							break
						}
					}
				}
			}(w)
		}
		wg.Wait()
	}
}

// Drive is one composer command: Calls operations of one RPC (or of a scenario). Under the
// test composer the stream is Derive(seed, 2, $HYDO_SEED) -- the composition (schedule token
// W=) gives every command entry its own HYDO_SEED, so the explorer varies the requests too;
// without it, Derive(seed, 3, n) for the run's n-th drive invocation (a shared counter). The
// process id is the OS process id. ok is whether every operation ended ok (the composer counts
// a command that exits non-zero as a failed operation).
func (c *Client) Drive(name string, calls int) (ok bool, err error) {
	var it item
	if r, err := c.Plan.Find(name); err == nil {
		it.rpc = r
	} else {
		for i := range c.Plan.Scenarios {
			if c.Plan.Scenarios[i].Name == name {
				it.scen = &c.Plan.Scenarios[i]
			}
		}
		if it.scen == nil {
			return false, err
		}
	}
	if calls <= 0 {
		calls = c.Plan.Drive.Calls
	}
	var stream uint64
	if v, perr := strconv.ParseUint(os.Getenv("HYDO_SEED"), 10, 64); perr == nil && os.Getenv("HYDO_SEED") != "" {
		stream = rng.Derive(c.Seed, 2, v)
	} else {
		stream = rng.Derive(c.Seed, 3, c.State.Next("drive"))
	}
	rnd := rng.New(stream)
	e := &env{c: c, local: map[string][]string{}}
	pid := int64(os.Getpid())
	ok = true
	for k := 0; k < calls; k++ {
		for tries := 0; tries < maxSkips; tries++ {
			ran, good := c.runItem(pid, it, rnd, e)
			if ran {
				ok = ok && good
				break
			}
		}
	}
	return ok, nil
}

// Retry re-sends, with the same request, every operation whose last outcome was unknown and
// whose RPC has retry.at_end (MANIFEST.md workload.retry: after the faults heal). Operations
// are retried in op order, each until its outcome is known or its attempts are used up.
func (c *Client) Retry(calls string) error {
	f, err := os.Open(calls)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	defer f.Close()
	type opState struct {
		process  int64
		rpc, req string
		last     string
	}
	ops := map[uint64]*opState{}
	var order []uint64
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 16<<20)
	for sc.Scan() {
		ev, ok := record.Parse(sc.Text())
		if !ok {
			continue
		}
		id, err := strconv.ParseUint(ev.Op, 10, 64)
		if err != nil {
			continue
		}
		o := ops[id]
		if o == nil {
			p, _ := strconv.ParseInt(ev.Process, 10, 64)
			o = &opState{process: p, rpc: ev.RPC}
			ops[id] = o
			order = append(order, id)
		}
		if ev.Type == "invoke" {
			o.req, o.last = ev.Body, "invoke"
		} else {
			o.last = ev.Type
		}
	}
	sort.Slice(order, func(i, j int) bool { return order[i] < order[j] })
	for _, id := range order {
		o := ops[id]
		if o.last != "info" && o.last != "invoke" {
			continue
		}
		r, err := c.Plan.Find(o.rpc)
		if err != nil || !r.Retry.AtEnd {
			continue
		}
		reqs, err := c.ParseJSON(r, o.req)
		if err != nil {
			fmt.Fprintf(c.stderr, "hydo-grpc: retry op %d: %v\n", id, err)
			continue
		}
		rr := *r
		if r.Retry.EndTries > 0 {
			rr.Retry.Attempts = r.Retry.EndTries - 1
		}
		c.Op(o.process, id, &rr, reqs, nil)
	}
	return nil
}
