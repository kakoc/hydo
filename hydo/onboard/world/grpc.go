package main

// gRPC-level faults (MANIFEST.md, "gRPC faults"): a streaming HTTP/2 proxy for every proxy with
// protocol: grpc, and the gRPC-only rule keys of an H= token.
//
// The proxy forwards each call to the real service as it arrives, message by message, in both
// directions (unary, client-, server- and bidirectional streaming calls all pass), so a fault can
// act after the server has answered (drop, reset, status with effect=yes) or in the middle of a
// stream (cut=, hold=). The rules of hydo-init-fakes.sh keep their meaning; gRPC proxies add:
//
//	status=<NAME|code>  answer this status without forwarding (effect=yes: forward, let the server
//	                    finish, then answer the status anyway: the server acted, the client sees a
//	                    failure). Names: UNAVAILABLE DEADLINE_EXCEEDED RESOURCE_EXHAUSTED INTERNAL
//	                    ABORTED ... (every gRPC code).
//	drop                forward; the server's answer never reaches the client, which waits until
//	                    its deadline (or the fault ends: then the stream is reset)
//	delay=<ms>          forward; hold the server's answer for ms of guest time (past the caller's
//	                    deadline when ms exceeds it)
//	reset | reset=stream  forward; when the server answers, reset the call's HTTP/2 stream
//	reset=conn          forward; when the server answers, reset the client's TCP connection
//	                    (every call on it fails, the client reconnects)
//	cut=<n>             let n messages through (dir=), then reset the stream (dir=req: the
//	                    server's half is cancelled too)
//	hold=<n>            let n messages through (dir=), then forward nothing more in that direction
//	                    until the fault ends (then the stream continues) or the client gives up
//	dir=req|resp        the direction cut=/hold= count messages in (default resp: server to client)
//	rpc=<name>          only calls of this method: Method, Service/Method or /pkg.Service/Method
//	call=<a>[-<b>]      only the a-th (to b-th) call of a method on this proxy, counted from 1
//	                    since the world started (every call, faulted or not, counts)
//	seed=<n>            changes which calls a p= share hits (the decision is a hash of the token
//	                    text and the call's index under the rule)
//
// rpc=, call=, cut=, hold=, dir= and reset=conn act on gRPC proxies only; on any other endpoint
// the `on` command refuses them.
//
// Records (/tmp/hydo/world.log, same log as every fake and proxy): at arrival the usual kind=req
// record (fx=1: the call is forwarded) with call=<k>; at the end of the call a kind=call record:
//
//	kind=call ep=<proxy> route=<route> path=<method> ref=<ref> call=<k> req=<seq of the req record>
//	          status=<what the client got> server=<1|0|?> upstream=<server's status|-> msgs=<in>/<out>
//	          out=<rule outcome>
//
// status is the grpc-status the client received (a number), rst (the stream was reset), conn (the
// connection was reset) or none (the client gave up before any answer); server=1: the server
// answered (it acted), 0: the call never reached it, ?: it was forwarded and the server never
// answered. "Server acted, client saw failure" is status != 0 with server=1. `hydo-world check
// calls <proxy>[.<route>]` counts the four outcomes.

import (
	"bufio"
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// grpcExt: the gRPC-only keys of a rule (parseRuleExt), next to the Rule they belong to.
type grpcExt struct {
	rpc            string
	callLo, callHi int64 // 0 = any
	cut, hold      int   // -1 = none
	dirReq         bool
	conn           bool // reset=conn
	keys           []string
}

var (
	extMu    sync.Mutex
	ruleExts = map[*Rule]*grpcExt{}
	callN    = map[string]int64{} // "<proxy> <path>" -> calls so far (under w.mu)
)

func extOf(r *Rule) *grpcExt {
	extMu.Lock()
	defer extMu.Unlock()
	return ruleExts[r]
}

func newExt(r *Rule) *grpcExt {
	extMu.Lock()
	defer extMu.Unlock()
	x := ruleExts[r]
	if x == nil {
		x = &grpcExt{cut: -1, hold: -1}
		ruleExts[r] = x
	}
	return x
}

// parseRuleExt: a rule key parseRule does not know. reset=<v> is seen here too (parseRule sets
// Reset and passes the value on).
func parseRuleExt(r *Rule, k, v string) error {
	num := func() (int, error) {
		n, err := strconv.Atoi(v)
		if err != nil || n < 0 {
			return 0, fmt.Errorf("%s=%q is not a number", k, v)
		}
		return n, nil
	}
	x := newExt(r)
	x.keys = append(x.keys, k)
	var err error
	switch k {
	case "rpc":
		if v == "" || strings.ContainsAny(v, " ,") {
			return fmt.Errorf("rpc=%q: a method name", v)
		}
		x.rpc = v
	case "call":
		a, b, rng := strings.Cut(v, "-")
		lo, e1 := strconv.ParseInt(a, 10, 64)
		hi := lo
		var e2 error
		if rng {
			hi, e2 = strconv.ParseInt(b, 10, 64)
		}
		if e1 != nil || e2 != nil || lo < 1 || hi < lo {
			return fmt.Errorf("call=%q: <n> or <a>-<b>, counted from 1", v)
		}
		x.callLo, x.callHi = lo, hi
	case "cut":
		x.cut, err = num()
	case "hold":
		x.hold, err = num()
	case "dir":
		switch v {
		case "req":
			x.dirReq = true
		case "resp":
		default:
			err = fmt.Errorf("dir=%q: req or resp", v)
		}
	case "seed":
		if v == "" {
			err = fmt.Errorf("seed=: a value")
		}
	case "reset":
		switch v {
		case "", "stream":
		case "conn":
			x.conn = true
		default:
			err = fmt.Errorf("reset=%q: stream or conn", v)
		}
	default:
		err = fmt.Errorf("unknown rule %q", k)
	}
	return err
}

// grpcOnly: does the rule use a key only gRPC proxies implement?
func (x *grpcExt) grpcOnly() bool {
	return x != nil && (x.rpc != "" || x.callLo > 0 || x.cut >= 0 || x.hold >= 0 || x.dirReq || x.conn)
}

// checkRuleExt: refuse gRPC-only keys on an endpoint that is not a gRPC proxy (`on` command).
func (w *World) checkRuleExt(r *Rule) error {
	x := extOf(r)
	if !x.grpcOnly() {
		return nil
	}
	e := w.cfg.endpoint(r.Target)
	if e == nil || e.Kind != "proxy" || e.Protocol != "grpc" {
		return fmt.Errorf("rpc=, call=, cut=, hold=, dir= and reset=conn need a gRPC proxy (%s is not one)", r.Target)
	}
	return nil
}

func (x *grpcExt) streamRule() bool { return x != nil && (x.cut >= 0 || x.hold >= 0) }

func rpcMatch(want, path string) bool {
	if want == "" {
		return true
	}
	if strings.HasPrefix(want, "/") {
		return path == want
	}
	return strings.HasSuffix(path, "/"+want) || strings.HasSuffix(path, "."+want)
}

// grpcRule: the first active rule for this call (like requestRule, with the gRPC filters). Every
// rule whose target, route, rpc= and call= fit is rolled, in order. mu held.
func (w *World) grpcRule(ep, route, path string, call int64) (*Rule, *grpcExt) {
	var hit *Rule
	var hx *grpcExt
	for _, r := range w.rules {
		x := extOf(r)
		if !(r.requestRule() || x.streamRule()) || !r.matches(ep, route) {
			continue
		}
		if x != nil && (!rpcMatch(x.rpc, path) || (x.callLo > 0 && (call < x.callLo || call > x.callHi))) {
			continue
		}
		if r.fires() && hit == nil {
			hit, hx = r, x
		}
	}
	if hit != nil && hx == nil {
		hx = &grpcExt{cut: -1, hold: -1}
	}
	return hit, hx
}

func outcomeExt(r *Rule, x *grpcExt) string {
	o := r.outcome("grpc")
	add := func(s string) {
		if o != "" {
			o += "+"
		}
		o += s
	}
	if x.conn && r.Reset {
		o = strings.Replace(o, "reset", "resetconn", 1)
	}
	d := "resp"
	if x.dirReq {
		d = "req"
	}
	if x.cut >= 0 {
		add(fmt.Sprintf("cut%d%s", x.cut, d))
	}
	if x.hold >= 0 {
		add(fmt.Sprintf("hold%d%s", x.hold, d))
	}
	return o
}

// ---------------------------------------------------------------- framing

// readFrame: one gRPC length-prefixed message (5-byte header + payload), whole.
func readFrame(br *bufio.Reader) ([]byte, error) {
	var h [5]byte
	if _, err := io.ReadFull(br, h[:]); err != nil {
		return nil, err
	}
	n := binary.BigEndian.Uint32(h[1:5])
	if n > 64<<20 {
		return nil, fmt.Errorf("message of %d bytes", n)
	}
	b := make([]byte, 5+int(n))
	copy(b, h[:])
	if _, err := io.ReadFull(br, b[5:]); err != nil {
		return nil, err
	}
	return b, nil
}

// ---------------------------------------------------------------- the server

type connKey struct{}

func (w *World) serveGRPCProxy(e *Endpoint, ln net.Listener) {
	var p http.Protocols
	p.SetUnencryptedHTTP2(true)
	tr := &http.Transport{}
	var tp http.Protocols
	tp.SetUnencryptedHTTP2(true)
	tr.Protocols = &tp
	up := &http.Client{Transport: tr}
	srv := &http.Server{Handler: w.grpcHandler(e, up), Protocols: &p,
		ConnContext: func(ctx context.Context, c net.Conn) context.Context { return context.WithValue(ctx, connKey{}, c) }}
	fmt.Fprintf(os.Stderr, "hydo-world: %s %s %s on %s:%d\n", e.Kind, e.Name, e.Protocol, e.Addr, e.Port)
	srv.Serve(ln)
}

func grpcTrailersOnly(rw http.ResponseWriter, code int, msg string) {
	rw.Header().Set("Content-Type", "application/grpc")
	rw.Header().Set("Grpc-Status", strconv.Itoa(code))
	rw.Header().Set("Grpc-Message", msg)
	rw.WriteHeader(http.StatusOK)
}

// a call's end, for its kind=call record
type callEnd struct {
	status   string // number, rst, conn, none
	server   string // 1, 0, ?
	upstream string
	in       atomic.Int64 // messages client -> server (the request goroutine counts them)
	out      int
}

func upstreamCode(h, t http.Header) string {
	if s := t.Get("Grpc-Status"); s != "" {
		return s
	}
	if s := h.Get("Grpc-Status"); s != "" {
		return s
	}
	return "-"
}

func (w *World) grpcHandler(e *Endpoint, up *http.Client) http.Handler {
	return http.HandlerFunc(func(rw http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		br := bufio.NewReaderSize(r.Body, 32<<10)
		v := &reqView{method: r.Method, path: r.URL.Path, header: r.Header, query: r.URL.Query()}
		rt, vars := e.match(r.Method, r.URL.Path)
		v.vars = vars
		rname, refSpec := "any", e.Ref
		if rt != nil {
			rname = rt.Name
			if rt.Ref != "" {
				refSpec = rt.Ref
			}
		}
		// the first message is read before the decision only when a ref or key is taken from it
		// (a bidirectional call whose server speaks first would otherwise wait for it)
		var first []byte
		bodyH := "-"
		if strings.HasPrefix(refSpec, "pb:") || strings.HasPrefix(e.Idempotency, "pb:") ||
			refSpec == "body" || e.Idempotency == "body" {
			if f, err := readFrame(br); err == nil {
				first = f
				v.msg, v.body = f[5:], f[5:]
				bodyH = bodyHash(f[5:])
			}
		}
		ref, idem := v.extract(refSpec), v.extract(e.Idempotency)

		w.lockGate()
		key := e.Name + " " + r.URL.Path
		callN[key]++
		call := callN[key]
		rule, x := w.grpcRule(e.Name, rname, r.URL.Path, call)
		apply := rule == nil || rule.applied()
		out, extra := "fwd", " call="+strconv.FormatInt(call, 10)
		if !apply {
			out = "-"
		}
		if rule != nil {
			out += "+" + outcomeExt(rule, x)
			extra += " rule=" + strconv.Itoa(rule.ID)
		}
		fxs := map[bool]string{true: "1", false: "0"}[apply]
		reqSeq := w.record("req", kv("ep", e.Name), kv("route", rname), kv("from", clientIP(r)), kv("method", r.Method),
			kv("path", r.URL.Path), kv("ref", ref), kv("idem", idem), "body="+bodyH, "fx="+fxs, "out="+out+extra)
		healCh := w.healCh
		var release chan struct{}
		if rule != nil {
			release = rule.release
		}
		w.mu.Unlock()

		end := &callEnd{status: "none", server: "0", upstream: "-"}
		defer func() {
			w.mu.Lock()
			if !w.frozen {
				w.record("call", kv("ep", e.Name), kv("route", rname), kv("path", r.URL.Path), kv("ref", ref),
					"call="+strconv.FormatInt(call, 10), "req="+strconv.FormatInt(reqSeq, 10), "status="+end.status,
					"server="+end.server, "upstream="+end.upstream, fmt.Sprintf("msgs=%d/%d", end.in.Load(), end.out), "out="+out)
			}
			w.mu.Unlock()
		}()
		// wait: until the client gives up, the rule is switched off, or every fault heals
		wait := func(d time.Duration) bool { // true: the client is still there
			var tc <-chan time.Time
			if d > 0 {
				t := time.NewTimer(d)
				defer t.Stop()
				tc = t.C
			}
			select {
			case <-ctx.Done():
				return false
			case <-tc:
			case <-release:
			case <-healCh:
			}
			return ctx.Err() == nil
		}
		hang := func() { // forever (bounded by the client, the rule and the heal)
			if release == nil {
				<-ctx.Done()
				return
			}
			wait(0)
		}
		rstStream := func() {
			end.status = "rst"
			panic(http.ErrAbortHandler)
		}
		rstConn := func() {
			end.status = "conn"
			if c, ok := ctx.Value(connKey{}).(net.Conn); ok {
				if tc, ok := c.(*net.TCPConn); ok {
					tc.SetLinger(0)
				}
				c.Close()
			}
			panic(http.ErrAbortHandler)
		}
		answer := func(code int) {
			grpcTrailersOnly(rw, code, "injected by hydo")
			end.status = strconv.Itoa(code)
		}

		if !apply { // the server never sees the call
			if rule.DelayMs > 0 && !wait(time.Duration(rule.DelayMs)*time.Millisecond) {
				return
			}
			switch {
			case rule.Drop:
				hang()
				if ctx.Err() == nil {
					rstStream()
				}
			case rule.Reset && x.conn:
				rstConn()
			case rule.Reset:
				rstStream()
			case rule.Status > 0:
				answer(rule.Status)
			default:
				answer(14) // UNAVAILABLE: no answer from upstream
			}
			return
		}

		// forward: the client's messages go up as they arrive
		pr, pw := io.Pipe()
		reqCut := make(chan struct{})
		go func() {
			n := 0
			send := func(f []byte) bool {
				if x != nil && x.dirReq {
					if x.cut >= 0 && n >= x.cut {
						close(reqCut)
						return false
					}
					if x.hold >= 0 && n >= x.hold {
						wait(0) // held until the fault ends or the client gives up
						if ctx.Err() != nil {
							return false
						}
					}
				}
				if _, err := pw.Write(f); err != nil {
					return false
				}
				n++
				end.in.Store(int64(n))
				return true
			}
			if first != nil && !send(first) {
				pw.CloseWithError(io.ErrUnexpectedEOF)
				return
			}
			for {
				f, err := readFrame(br)
				if err != nil {
					if err == io.EOF {
						pw.Close()
					} else {
						pw.CloseWithError(err)
					}
					return
				}
				if !send(f) {
					pw.CloseWithError(io.ErrUnexpectedEOF)
					return
				}
			}
		}()
		uctx, ucancel := context.WithCancel(ctx)
		defer ucancel()
		ureq, err := http.NewRequestWithContext(uctx, http.MethodPost, "http://"+e.To+r.URL.RequestURI(), pr)
		if err != nil {
			answer(13)
			return
		}
		for k, vs := range r.Header {
			if !hopHeaders[k] {
				ureq.Header[k] = vs
			}
		}
		ureq.ContentLength = -1
		type res struct {
			resp *http.Response
			err  error
		}
		rc := make(chan res, 1)
		go func() { resp, err := up.Do(ureq); rc <- res{resp, err} }()
		var resp *http.Response
		select {
		case <-reqCut: // cut=, dir=req: the server's half is cancelled, the client's reset
			ucancel()
			end.server = "?"
			rstStream()
		case rr := <-rc:
			if rr.err != nil {
				end.server = "?"
				if ctx.Err() != nil {
					return
				}
				answer(14)
				return
			}
			resp = rr.resp
		}
		defer resp.Body.Close()
		end.server = "1"
		ub := bufio.NewReaderSize(resp.Body, 32<<10)
		drain := func() { // let the server finish: read its answer to the end, keep its status
			for {
				if _, err := readFrame(ub); err != nil {
					break
				}
			}
			end.upstream = upstreamCode(resp.Header, resp.Trailer)
		}
		if rule != nil && rule.DelayMs > 0 && !wait(time.Duration(rule.DelayMs)*time.Millisecond) {
			return
		}
		switch {
		case rule != nil && rule.Drop:
			drain()
			hang()
			if ctx.Err() == nil {
				rstStream()
			}
			return
		case rule != nil && rule.Reset && x.conn:
			rstConn()
		case rule != nil && rule.Reset:
			rstStream()
		case rule != nil && rule.Status > 0:
			drain()
			answer(rule.Status)
			return
		}
		// relay the answer, message by message
		for k, vs := range resp.Header {
			if !hopHeaders[k] && k != "Content-Length" {
				rw.Header()[k] = vs
			}
		}
		rw.WriteHeader(resp.StatusCode)
		fl, _ := rw.(http.Flusher)
		if fl != nil {
			fl.Flush()
		}
		n := 0
		for {
			if x != nil && !x.dirReq {
				if x.cut >= 0 && n >= x.cut {
					ucancel()
					rstStream()
				}
				if x.hold >= 0 && n >= x.hold {
					if !wait(0) {
						return
					}
				}
			}
			f, err := readFrame(ub)
			if err != nil {
				break
			}
			if _, err := rw.Write(f); err != nil {
				return
			}
			if fl != nil {
				fl.Flush()
			}
			n++
			end.out = n
		}
		end.upstream = upstreamCode(resp.Header, resp.Trailer)
		if end.upstream == "-" { // the server's half broke off without a status (or cut=, dir=req)
			if ctx.Err() != nil {
				return // the client gave up
			}
			rstStream()
		}
		for k, vs := range resp.Trailer {
			for _, val := range vs {
				rw.Header().Add(http.TrailerPrefix+k, val)
			}
		}
		end.status = end.upstream
	})
}

// callSummary: a kind=call record as one line of the console summary (World.summary)
func callSummary(r map[string]string) string {
	return fmt.Sprintf("%s call %s.%s %s ref=%s n=%s status=%s server=%s upstream=%s msgs=%s %s", r["seq"], r["ep"], r["route"],
		r["path"], esc(r["ref"]), r["call"], r["status"], r["server"], r["upstream"], r["msgs"], r["out"])
}

// ---------------------------------------------------------------- check calls

// checkExt: the gRPC check kinds; ok=false = not one of them.
//
//	check calls <proxy>[.<route>]   count the calls by outcome: answered OK; failed after the
//	                                server acted; failed before it was reached; failed, unknown
func checkExt(kind, target string, recs []rec) (int, bool) {
	if kind != "calls" {
		return 0, false
	}
	ep, route, _ := strings.Cut(target, ".")
	var n, ok, acted, before, unknown int
	for _, r := range recs {
		if r["kind"] != "call" || r["ep"] != ep || (route != "" && r["route"] != route) {
			continue
		}
		n++
		switch {
		case r["status"] == "0":
			ok++
		case r["server"] == "1":
			acted++
		case r["server"] == "0":
			before++
		default:
			unknown++
		}
	}
	return say(0, "%s: %d calls: %d answered OK, %d failed after the server acted, %d failed before reaching it, %d failed with the server's part unknown",
		target, n, ok, acted, before, unknown), true
}
