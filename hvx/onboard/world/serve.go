package main

import (
	"bytes"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strconv"
	"time"
)

func serveMain(args []string) int {
	if len(args) < 1 {
		usage()
	}
	cfg, err := loadConfig(args[0])
	if err != nil {
		fmt.Fprintln(os.Stderr, "hydo-world:", err)
		return 1
	}
	carry := ""
	for i := 1; i+1 < len(args); i += 2 {
		if args[i] == "--carry" {
			carry = args[i+1]
		}
	}
	lp, sp := or(cfg.Log, logPath()), or(cfg.Sock, sockPath())
	f, err := os.OpenFile(lp, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		fmt.Fprintln(os.Stderr, "hydo-world:", err)
		return 1
	}
	w := &World{cfg: cfg, logf: f, idem: map[string]int64{}, pending: map[int64]*hook{},
		held: map[string]*hook{}, healCh: make(chan struct{})}
	if carry != "" {
		if err := w.loadCarry(carry); err != nil {
			fmt.Fprintln(os.Stderr, "hydo-world: carry:", err)
			return 1
		}
	}
	// every listener is up before the control socket answers, so "ctl ping" = ready
	for _, e := range cfg.all() {
		ln, err := net.Listen("tcp", net.JoinHostPort(e.Addr, strconv.Itoa(e.Port)))
		if err != nil {
			fmt.Fprintf(os.Stderr, "hydo-world: %s: %v\n", e.Name, err)
			return 1
		}
		if e.Protocol == "tcp" {
			go w.serveTCP(e, ln)
			continue
		}
		if e.Kind == "proxy" && e.Protocol == "grpc" { // the streaming gRPC proxy (grpc.go)
			go w.serveGRPCProxy(e, ln)
			continue
		}
		var p http.Protocols
		p.SetHTTP1(true)
		p.SetUnencryptedHTTP2(true) // cleartext HTTP/2 with prior knowledge: gRPC clients
		srv := &http.Server{Handler: w.handler(e), Protocols: &p, ErrorLog: nil}
		go srv.Serve(ln)
		fmt.Fprintf(os.Stderr, "hydo-world: %s %s %s on %s:%d\n", e.Kind, e.Name, e.Protocol, e.Addr, e.Port)
	}
	os.Remove(sp)
	cl, err := net.Listen("unix", sp)
	if err != nil {
		fmt.Fprintln(os.Stderr, "hydo-world: control socket:", err)
		return 1
	}
	w.control(cl)
	return 0
}

func or(s, d string) string {
	if s == "" {
		return d
	}
	return s
}

func clientIP(r *http.Request) string {
	h, _, _ := net.SplitHostPort(r.RemoteAddr)
	return h
}

// abort: end the exchange without an answer. HTTP/1: close the TCP connection (rst: with a
// reset, SO_LINGER 0); HTTP/2: reset the stream.
func abort(rw http.ResponseWriter, rst bool) {
	if hj, ok := rw.(http.Hijacker); ok {
		if c, _, err := hj.Hijack(); err == nil {
			if tc, ok := c.(*net.TCPConn); ok && rst {
				tc.SetLinger(0)
			}
			c.Close()
			return
		}
	}
	panic(http.ErrAbortHandler)
}

func grpcFrame(msg []byte) []byte {
	b := make([]byte, 5+len(msg))
	binary.BigEndian.PutUint32(b[1:5], uint32(len(msg)))
	copy(b[5:], msg)
	return b
}

func grpcFirst(body []byte) []byte {
	if len(body) < 5 {
		return nil
	}
	n := int(binary.BigEndian.Uint32(body[1:5]))
	if 5+n > len(body) {
		return nil
	}
	return body[5 : 5+n]
}

func grpcStatus(rw http.ResponseWriter, code int, msg string) {
	rw.Header().Set("Content-Type", "application/grpc")
	rw.Header().Set("Grpc-Status", strconv.Itoa(code))
	rw.Header().Set("Grpc-Message", msg)
	rw.WriteHeader(http.StatusOK) // trailers-only response
}

func (w *World) handler(e *Endpoint) http.Handler {
	var up *http.Client
	if e.Kind == "proxy" {
		tr := &http.Transport{DisableKeepAlives: e.Protocol != "grpc"}
		var p http.Protocols
		if e.Protocol == "grpc" {
			p.SetUnencryptedHTTP2(true)
		} else {
			p.SetHTTP1(true)
		}
		tr.Protocols = &p
		up = &http.Client{Transport: tr}
	}
	return http.HandlerFunc(func(rw http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(io.LimitReader(r.Body, 1<<20))
		v := &reqView{method: r.Method, path: r.URL.Path, header: r.Header, query: r.URL.Query(), body: body}
		grpc := e.Protocol == "grpc"
		if grpc {
			v.msg = grpcFirst(body)
		}
		rt, vars := e.match(r.Method, r.URL.Path)
		v.vars = vars
		rname, refSpec := "-", e.Ref
		if rt != nil {
			rname = rt.Name
			if rt.Ref != "" {
				refSpec = rt.Ref
			}
		} else if e.Kind == "proxy" {
			rname = "any"
		}
		ref, idem := v.extract(refSpec), v.extract(e.Idempotency)

		w.lockGate()
		rule := w.requestRule(e.Name, rname)
		apply := rule == nil || rule.applied()
		fx := apply && (e.Kind == "proxy" || (rt != nil && rt.effectful(r.Method, e.Protocol)))
		seq := w.seq + 1
		ansSeq := seq
		out := map[bool]string{true: "ok", false: "fwd"}[e.Kind == "fake"]
		if !apply {
			out = "-" // the outside world never acted on it
		}
		key := e.Name + "/" + rname + "/" + idem
		extra := ""
		if e.Kind == "fake" && fx && rt.Idempotent && idem != "" {
			if s, ok := w.idem[key]; ok { // a repeat of an idempotent request: same answer, no new effect
				fx, ansSeq, out = false, s, "replay"
				extra = " orig=" + strconv.FormatInt(s, 10)
			}
		}
		if rule != nil {
			out += "+" + rule.outcome(e.Protocol)
			extra += " rule=" + strconv.Itoa(rule.ID)
		}
		fxs := "0"
		if fx {
			fxs = "1"
			if idem != "" {
				w.idem[key] = seq
			}
		}
		w.record("req", kv("ep", e.Name), kv("route", rname), kv("from", clientIP(r)), kv("method", r.Method),
			kv("path", r.URL.Path), kv("ref", ref), kv("idem", idem), "body="+bodyHash(body), "fx="+fxs, "out="+out+extra)
		if fx && e.Kind == "fake" && rt.Webhook != nil {
			w.newHooks(e, rt, ref, idem, seq)
		}
		healCh := w.healCh
		w.mu.Unlock()

		ctx := map[string]string{"seq": strconv.FormatInt(ansSeq, 10), "ref": ref, "idem": idem, "fake": e.Name, "route": rname}
		// the real service's answer (proxy) comes first: the fault acts on what it answered
		var upResp *http.Response
		var upBody []byte
		var upErr error
		if e.Kind == "proxy" && apply {
			upResp, upBody, upErr = forward(up, e, r, body)
		}
		if rule != nil && rule.DelayMs > 0 {
			select {
			case <-time.After(time.Duration(rule.DelayMs) * time.Millisecond):
			case <-r.Context().Done():
			}
		}
		if rule != nil && rule.Drop { // accept, never answer (until the client gives up or the fault heals)
			select {
			case <-r.Context().Done():
			case <-rule.release:
			case <-healCh:
			}
			abort(rw, false)
			return
		}
		if rule != nil && rule.Reset {
			abort(rw, true)
			return
		}
		if rule != nil && rule.Status > 0 {
			if grpc {
				grpcStatus(rw, rule.Status, "injected by hydo")
				return
			}
			rw.Header().Set("Content-Type", "application/json")
			if rule.RetryAfter != "" {
				rw.Header().Set("Retry-After", rule.RetryAfter)
			}
			rw.WriteHeader(rule.Status)
			fmt.Fprintf(rw, `{"error":"injected by hydo","status":%d}`, rule.Status)
			return
		}
		if e.Kind == "proxy" {
			if upErr != nil {
				if grpc {
					grpcStatus(rw, 14, "hydo proxy: upstream: "+upErr.Error())
				} else {
					http.Error(rw, "hydo proxy: upstream: "+upErr.Error(), http.StatusBadGateway)
				}
				return
			}
			copyResponse(rw, upResp, upBody)
			return
		}
		if rt == nil {
			if grpc {
				grpcStatus(rw, 12, "hydo fake: no route for "+r.URL.Path)
			} else {
				http.Error(rw, `{"error":"hydo fake: no route"}`, http.StatusNotFound)
			}
			return
		}
		for k, val := range rt.Headers {
			rw.Header().Set(k, render(val, ctx, v))
		}
		if grpc {
			msg, _ := hex.DecodeString(rt.Message)
			rw.Header().Set("Content-Type", "application/grpc")
			if rt.Code != 0 {
				grpcStatus(rw, rt.Code, "hydo fake")
				return
			}
			rw.WriteHeader(http.StatusOK)
			rw.Write(grpcFrame(msg))
			rw.Header().Set(http.TrailerPrefix+"Grpc-Status", "0")
			rw.Header().Set(http.TrailerPrefix+"Grpc-Message", "")
			return
		}
		if rw.Header().Get("Content-Type") == "" {
			rw.Header().Set("Content-Type", "application/json")
		}
		rw.WriteHeader(rt.Status)
		io.WriteString(rw, render(rt.Body, ctx, v))
	})
}

var hopHeaders = map[string]bool{"Connection": true, "Keep-Alive": true, "Proxy-Connection": true,
	"Transfer-Encoding": true, "Upgrade": true, "Proxy-Authenticate": true, "Proxy-Authorization": true}

func forward(c *http.Client, e *Endpoint, r *http.Request, body []byte) (*http.Response, []byte, error) {
	req, err := http.NewRequestWithContext(r.Context(), r.Method, "http://"+e.To+r.URL.RequestURI(), bytes.NewReader(body))
	if err != nil {
		return nil, nil, err
	}
	for k, vs := range r.Header {
		if !hopHeaders[k] && (k != "Te" || e.Protocol == "grpc") {
			req.Header[k] = vs
		}
	}
	resp, err := c.Do(req)
	if err != nil {
		return nil, nil, err
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, nil, err
	}
	return resp, b, nil
}

func copyResponse(rw http.ResponseWriter, resp *http.Response, body []byte) {
	for k, vs := range resp.Header {
		if !hopHeaders[k] && k != "Content-Length" {
			rw.Header()[k] = vs
		}
	}
	rw.WriteHeader(resp.StatusCode)
	rw.Write(body)
	for k, vs := range resp.Trailer {
		for _, v := range vs {
			rw.Header().Add(http.TrailerPrefix+k, v)
		}
	}
}

// ---------------------------------------------------------------- the TCP proxy (any protocol)

// serveTCP: a fault proxy for a service that is not HTTP (Redis, a database). One connection is
// one request in the records (route "conn"); delay = every chunk the service sends back is held
// that long; drop = the client's bytes reach the service, its answer never comes back; reset =
// the client's bytes reach the service, the connection is reset when the answer arrives.
// effect=no: nothing reaches the service (drop holds the connection, reset resets it at once).
func (w *World) serveTCP(e *Endpoint, ln net.Listener) {
	fmt.Fprintf(os.Stderr, "hydo-world: proxy %s tcp on %s:%d -> %s\n", e.Name, e.Addr, e.Port, e.To)
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		go w.tcpConn(e, c)
	}
}

func rst(c net.Conn) {
	if tc, ok := c.(*net.TCPConn); ok {
		tc.SetLinger(0)
	}
	c.Close()
}

func (w *World) tcpConn(e *Endpoint, c net.Conn) {
	w.lockGate()
	rule := w.requestRule(e.Name, "conn")
	apply := rule == nil || rule.applied() || rule.Status > 0 // status= has no meaning on a byte stream
	out := "fwd"
	if !apply {
		out = "-"
	}
	extra := ""
	if rule != nil {
		out += "+" + rule.outcome("tcp")
		extra = " rule=" + strconv.Itoa(rule.ID)
	}
	fxs := "0"
	if apply {
		fxs = "1"
	}
	host, _, _ := net.SplitHostPort(c.RemoteAddr().String())
	w.record("req", kv("ep", e.Name), "route=conn", kv("from", host), "fx="+fxs, "out="+out+extra)
	healCh := w.healCh
	w.mu.Unlock()
	hold := func() {
		if rule == nil {
			return
		}
		select {
		case <-rule.release:
		case <-healCh:
		}
	}
	if !apply {
		if rule.Reset {
			rst(c)
			return
		}
		go func() { io.Copy(io.Discard, c) }()
		hold()
		c.Close()
		return
	}
	u, err := net.Dial("tcp", e.To)
	if err != nil {
		c.Close()
		return
	}
	defer u.Close()
	go func() {
		io.Copy(u, c)
		if tc, ok := u.(*net.TCPConn); ok {
			tc.CloseWrite()
		}
	}()
	buf := make([]byte, 32<<10)
	for {
		n, err := u.Read(buf)
		if n > 0 && rule != nil {
			if rule.Reset {
				rst(c)
				return
			}
			if rule.Drop {
				continue
			}
			if rule.DelayMs > 0 {
				time.Sleep(time.Duration(rule.DelayMs) * time.Millisecond)
			}
		}
		if n > 0 {
			if _, werr := c.Write(buf[:n]); werr != nil {
				return
			}
		}
		if err != nil {
			break
		}
	}
	if rule != nil && rule.Drop {
		hold()
	}
	if tc, ok := c.(*net.TCPConn); ok {
		tc.CloseWrite()
	}
	c.Close()
}
