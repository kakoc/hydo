package main

import (
	"bufio"
	"fmt"
	"net"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"
)

// World: the shared state of every fake and proxy. All decisions and records happen under mu,
// in the order requests arrive, so a deterministic guest gives a deterministic record log.
type World struct {
	mu      sync.Mutex
	cfg     *Config
	seq     int64
	logf    *os.File
	rules   []*Rule
	idem    map[string]int64 // "<endpoint>/<route>/<idempotency key>" -> seq of the first effect
	pending map[int64]*hook  // webhooks not delivered yet (carried across a power cut)
	held    map[string]*hook // reorder: the webhook held back per "<fake>.<route>"
	hookN   int64
	frozen  bool
	healCh  chan struct{} // closed (and replaced) at every heal: ends held (dropped) requests
	trunc   bool          // the carried log of a power cut did not fit (checks cannot decide)
}

type hook struct {
	id        int64
	ep        *Endpoint
	rt        *Route
	ref, idem string
	src       int64 // seq of the request that caused it
	lateMs    int
	dup, lost bool
}

func (w *World) record(kind string, fields ...string) int64 {
	w.seq++
	line := fmt.Sprintf("seq=%d t=%d kind=%s %s\n", w.seq, time.Now().UnixMilli(), kind, strings.Join(fields, " "))
	w.logf.WriteString(line)
	return w.seq
}

func kv(k, v string) string { return k + "=" + esc(v) }

// gate: a request that arrives after the world was frozen (the system under test lost power)
// never gets anywhere. Returns with mu held.
func (w *World) lockGate() {
	w.mu.Lock()
	if w.frozen {
		w.mu.Unlock()
		select {}
	}
}

// requestRule: the first active request rule for (endpoint, route) that fires for this request.
// Every matching rule is rolled (in order), so one rule's decisions never depend on another's.
func (w *World) requestRule(ep, route string) *Rule {
	var hit *Rule
	for _, r := range w.rules {
		if r.requestRule() && r.matches(ep, route) {
			if r.fires() && hit == nil {
				hit = r
			}
		}
	}
	return hit
}

// newHooks: the webhook(s) an effect on rt triggers, with the webhook rules applied. mu held.
func (w *World) newHooks(ep *Endpoint, rt *Route, ref, idem string, src int64) {
	h := &hook{ep: ep, rt: rt, ref: ref, idem: idem, src: src}
	reorder := false
	for _, r := range w.rules {
		if r.hookRule() && r.matches(ep.Name, rt.Name) && r.fires() {
			h.dup = h.dup || r.Dup
			h.lost = h.lost || r.Lost
			reorder = reorder || r.Reorder
			if r.LateMs > h.lateMs {
				h.lateMs = r.LateMs
			}
		}
	}
	w.hookN++
	h.id = w.hookN
	if h.lost {
		w.record("hook", kv("ep", ep.Name), kv("route", rt.Name), kv("ref", ref), kv("src", strconv.FormatInt(src, 10)), "out=lost")
		return
	}
	w.pending[h.id] = h
	key := ep.Name + "." + rt.Name
	if reorder {
		if prev := w.held[key]; prev != nil { // deliver this one first, then the one held back
			delete(w.held, key)
			go func() { w.deliver(h, 0); w.deliver(prev, 0) }()
		} else {
			w.held[key] = h
		}
		return
	}
	go w.deliver(h, 0)
	if h.dup {
		w.hookN++
		d := *h
		d.id = w.hookN
		w.pending[d.id] = &d
		go w.deliver(&d, 0)
	}
}

func (w *World) flushHeld(target string) {
	keys := make([]string, 0, len(w.held))
	for k := range w.held {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if target == "" || k == target || strings.HasPrefix(k, target+".") {
			h := w.held[k]
			delete(w.held, k)
			go w.deliver(h, 0)
		}
	}
}

var hookClient = &http.Client{Timeout: 5 * time.Second, Transport: &http.Transport{DisableKeepAlives: true}}

// deliver: POST the webhook (after its delay), retrying a failed delivery with backoff as a
// payment provider does. A frozen world (power cut) leaves it pending: it is carried.
func (w *World) deliver(h *hook, extra time.Duration) {
	wh := h.rt.Webhook
	time.Sleep(time.Duration(wh.DelayMs+h.lateMs)*time.Millisecond + extra)
	ctx := map[string]string{"seq": strconv.FormatInt(h.src, 10), "ref": h.ref, "idem": h.idem, "fake": h.ep.Name, "route": h.rt.Name}
	body := render(wh.Body, ctx, nil)
	back := 500 * time.Millisecond
	for attempt := 1; ; attempt++ {
		w.mu.Lock()
		fz := w.frozen
		w.mu.Unlock()
		if fz {
			return
		}
		req, err := http.NewRequest(wh.Method, render(wh.URL, ctx, nil), strings.NewReader(body))
		out := "err"
		ok := false
		if err == nil {
			for k, v := range wh.Headers {
				req.Header.Set(k, render(v, ctx, nil))
			}
			if req.Header.Get("Content-Type") == "" {
				req.Header.Set("Content-Type", "application/json")
			}
			resp, err := hookClient.Do(req)
			if err == nil {
				resp.Body.Close()
				out = strconv.Itoa(resp.StatusCode)
				ok = resp.StatusCode/100 == 2
			}
		}
		w.mu.Lock()
		if w.frozen {
			w.mu.Unlock()
			return
		}
		dup := "0"
		if h.dup {
			dup = "1"
		}
		w.record("hook", kv("ep", h.ep.Name), kv("route", h.rt.Name), kv("ref", h.ref), kv("idem", h.idem),
			"src="+strconv.FormatInt(h.src, 10), "n="+strconv.Itoa(attempt), "dup="+dup, "out="+out)
		done := ok || attempt > wh.Retries
		if done {
			delete(w.pending, h.id)
			if !ok {
				w.record("hook", kv("ep", h.ep.Name), kv("route", h.rt.Name), kv("ref", h.ref), "src="+strconv.FormatInt(h.src, 10), "out=gave-up")
			}
		}
		w.mu.Unlock()
		if done {
			return
		}
		time.Sleep(back)
		back *= 2
	}
}

// ---------------------------------------------------------------- control socket

func (w *World) control(ln net.Listener) {
	for {
		c, err := ln.Accept()
		if err != nil {
			return
		}
		go func(c net.Conn) {
			defer c.Close()
			line, _ := bufio.NewReader(c).ReadString('\n')
			f := strings.Fields(line)
			if len(f) == 0 {
				return
			}
			fmt.Fprint(c, w.command(f))
		}(c)
	}
}

func (w *World) command(f []string) string {
	w.mu.Lock()
	defer w.mu.Unlock()
	switch f[0] {
	case "ping":
		return "ok\n"
	case "on":
		if len(f) != 3 {
			return "error: on <id> <token>\n"
		}
		id, _ := strconv.Atoi(f[1])
		r, err := parseRule(id, f[2])
		if err != nil {
			return "error: " + err.Error() + "\n"
		}
		if w.cfg.endpoint(r.Target) == nil {
			return "error: no fake or proxy " + r.Target + "\n"
		}
		w.rules = append(w.rules, r)
		return "ok\n"
	case "off":
		if len(f) != 2 {
			return "error: off <id>\n"
		}
		id, _ := strconv.Atoi(f[1])
		keep := w.rules[:0]
		for _, r := range w.rules {
			if r.ID == id {
				close(r.release)
				if r.Reorder {
					t := r.Target
					if r.Route != "" {
						t += "." + r.Route
					}
					w.flushHeld(t)
				}
			} else {
				keep = append(keep, r)
			}
		}
		w.rules = keep
		return "ok\n"
	case "heal":
		for _, r := range w.rules {
			close(r.release)
		}
		w.rules = nil
		w.flushHeld("")
		close(w.healCh)
		w.healCh = make(chan struct{})
		return "ok\n"
	case "event":
		if len(f) != 3 {
			return "error: event <name> <ref>\n"
		}
		return fmt.Sprintf("ok %d\n", w.record("event", kv("ep", "-"), kv("route", unesc(f[1])), kv("ref", unesc(f[2])), "fx=1"))
	case "dump":
		w.frozen = true
		return w.carry()
	}
	return "error: unknown command " + f[0] + "\n"
}

// ---------------------------------------------------------------- power cut: carry the world

// carryMax: what a power cut may carry (the recovery boot gets it on its command line, which
// shares the 4 KiB serial input with the acknowledged ids).
const carryMax = 3000

// carry: the records the side-effect checks and the recovery world need, as w: tokens in log
// order -- effects (e), events (v), webhooks not delivered yet (p).
func (w *World) carry() string {
	b, err := os.ReadFile(w.logf.Name())
	if err != nil {
		return "w:trunc\nEND\n"
	}
	var out []string
	n := 0
	for _, ln := range strings.Split(string(b), "\n") {
		r := parseRecord(ln)
		if r == nil || r["fx"] != "1" || (r["kind"] != "req" && r["kind"] != "event") {
			continue
		}
		k := "e"
		if r["kind"] == "event" {
			k = "v"
		}
		out = append(out, fmt.Sprintf("w:%s,%s,%s,%s,%s,%s", r["seq"], k, esc(r["ep"]), esc(r["route"]), esc(r["ref"]), esc(r["idem"])))
	}
	ids := make([]int64, 0, len(w.pending))
	for id := range w.pending {
		ids = append(ids, id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	for _, id := range ids {
		h := w.pending[id]
		out = append(out, fmt.Sprintf("w:%d,p,%s,%s,%s,%s", h.src, esc(h.ep.Name), esc(h.rt.Name), esc(h.ref), esc(h.idem)))
	}
	for _, s := range out {
		n += len(s) + 1
	}
	if n > carryMax || w.trunc {
		return "w:trunc\nEND\n"
	}
	return strings.Join(append(out, "END"), "\n") + "\n"
}

// loadCarry: the recovery boot's world starts from what the first boot carried: its effects and
// events (so the checks see both boots), its idempotency memory, and its pending webhooks.
func (w *World) loadCarry(path string) error {
	b, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var hooks []*hook
	for _, tok := range strings.Fields(string(b)) {
		if tok == "w:trunc" {
			w.trunc = true
			continue
		}
		f := strings.Split(strings.TrimPrefix(tok, "w:"), ",")
		if !strings.HasPrefix(tok, "w:") || len(f) != 6 {
			continue
		}
		seq, _ := strconv.ParseInt(f[0], 10, 64)
		ep, route, ref, idem := unesc(f[2]), unesc(f[3]), unesc(f[4]), unesc(f[5])
		if seq > w.seq {
			w.seq = seq
		}
		switch f[1] {
		case "e":
			fmt.Fprintf(w.logf, "seq=%d t=0 kind=req %s %s %s %s fx=1 out=carried\n", seq, kv("ep", ep), kv("route", route), kv("ref", ref), kv("idem", idem))
			if idem != "" {
				w.idem[ep+"/"+route+"/"+idem] = seq
			}
		case "v":
			fmt.Fprintf(w.logf, "seq=%d t=0 kind=event ep=- %s %s fx=1 out=carried\n", seq, kv("route", route), kv("ref", ref))
		case "p":
			e := w.cfg.endpoint(ep)
			if e == nil || e.route(route) == nil || e.route(route).Webhook == nil {
				continue
			}
			w.hookN++
			hooks = append(hooks, &hook{id: w.hookN, ep: e, rt: e.route(route), ref: ref, idem: idem, src: seq})
		}
	}
	if w.trunc {
		w.logf.WriteString("seq=0 t=0 kind=meta trunc=1\n")
	}
	for _, h := range hooks {
		w.pending[h.id] = h
		go w.deliver(h, time.Second) // the provider retries after the outage
	}
	return nil
}

// parseRecord: "k=v k=v ..." -> map (values unescaped)
func parseRecord(ln string) map[string]string {
	if !strings.HasPrefix(ln, "seq=") {
		return nil
	}
	m := map[string]string{}
	for _, f := range strings.Fields(ln) {
		k, v, _ := strings.Cut(f, "=")
		m[k] = unesc(v)
	}
	return m
}
