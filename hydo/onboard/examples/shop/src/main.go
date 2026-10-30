// shop: a small order service, the demo project for hydo's fake outside services
// (hvx/onboard/examples/shop/hydo.yaml). It takes orders over HTTP, charges them through a
// payment provider, refunds cancelled orders, and sends a receipt email -- the provider and the
// email service are hydo's fakes. Its state is a journal on the data disk (fsync per entry).
//
//	shop server            the service, on 127.0.0.1:8080
//	shop place <id>        client: place order <id> (exit 0 = acknowledged)
//	shop cancel <id>       client: cancel order <id> (exit 0 = acknowledged)
//
// One planted bug, in charge_buggy.go (built without the "fixed" tag): every retry of a charge
// that timed out or failed gets a NEW idempotency key, so the provider cannot tell it is the same
// charge and takes the money twice. charge_fixed.go derives the key from the order, once.
package main

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

const (
	listen   = "127.0.0.1:8080"
	payments = "http://127.0.0.1:9001"
	email    = "http://127.0.0.1:9002"
)

// the journal on the data disk (SHOP_DATA overrides the directory, for a run outside hydo)
var dataDir = func() string {
	if d := os.Getenv("SHOP_DATA"); d != "" {
		return d
	}
	return "/data/shop"
}()
var journalP = dataDir + "/orders.log"

type order struct {
	mu      sync.Mutex
	id      int
	amount  int
	status  string // placed | paid | cancelled
	charge  string
	receipt bool
	refund  bool
}

var (
	mu      sync.Mutex
	orders  = map[int]*order{}
	journal *os.File
	jmu     sync.Mutex
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: shop server | place <id> | cancel <id>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "server":
		server()
	case "place", "cancel":
		os.Exit(client(os.Args[1], os.Args[2]))
	default:
		os.Exit(2)
	}
}

// ---------------------------------------------------------------- the journal

func note(format string, a ...any) error {
	jmu.Lock()
	defer jmu.Unlock()
	if _, err := fmt.Fprintf(journal, format+"\n", a...); err != nil {
		return err
	}
	return journal.Sync()
}

func replay() {
	f, err := os.Open(journalP)
	if err != nil {
		return
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		p := strings.Fields(sc.Text())
		if len(p) < 2 {
			continue
		}
		id, _ := strconv.Atoi(p[1])
		o := get(id)
		switch p[0] {
		case "placed":
			if len(p) > 2 {
				o.amount, _ = strconv.Atoi(p[2])
			}
			if o.status == "" {
				o.status = "placed"
			}
		case "charged":
			if o.status != "cancelled" {
				o.status = "paid"
			}
			if len(p) > 2 {
				o.charge = p[2]
			}
		case "cancelled":
			o.status = "cancelled"
		case "refunded":
			o.refund = true
		case "receipt":
			o.receipt = true
		}
	}
}

func get(id int) *order {
	mu.Lock()
	defer mu.Unlock()
	o := orders[id]
	if o == nil {
		o = &order{id: id}
		orders[id] = o
	}
	return o
}

// ---------------------------------------------------------------- the outside world

var client1s = &http.Client{Timeout: time.Second, Transport: &http.Transport{DisableKeepAlives: true}}

type answer struct {
	status     int
	body       map[string]any
	retryAfter time.Duration
}

// call: one POST with a JSON body and an idempotency key; err = no answer (timeout, reset)
func call(url, key string, body map[string]any) (*answer, error) {
	b, _ := json.Marshal(body)
	req, _ := http.NewRequest("POST", url, bytes.NewReader(b))
	req.Header.Set("Content-Type", "application/json")
	if key != "" {
		req.Header.Set("Idempotency-Key", key)
	}
	resp, err := client1s.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	a := &answer{status: resp.StatusCode}
	rb, _ := io.ReadAll(resp.Body)
	_ = json.Unmarshal(rb, &a.body)
	if s, err := strconv.Atoi(resp.Header.Get("Retry-After")); err == nil {
		a.retryAfter = time.Duration(s) * time.Second
	}
	return a, nil
}

// withRetries: up to 3 attempts; key(attempt) gives the idempotency key of each attempt
func withRetries(url string, key func(int) string, body map[string]any) (*answer, error) {
	var last error
	for attempt := 1; attempt <= 3; attempt++ {
		a, err := call(url, key(attempt), body)
		if err == nil && a.status/100 == 2 {
			return a, nil
		}
		wait := 200 * time.Millisecond
		if err != nil {
			last = err
		} else {
			last = fmt.Errorf("status %d", a.status)
			if a.status/100 == 4 && a.status != 429 && a.status != 409 {
				return a, last // the provider refused it: retrying cannot help
			}
			if a.retryAfter > 0 {
				wait = min(a.retryAfter, 2*time.Second)
			}
		}
		time.Sleep(wait)
	}
	return nil, last
}

func ref(o *order) string { return "o" + strconv.Itoa(o.id) }

func sendReceipt(o *order) {
	if o.receipt || o.status != "paid" {
		return
	}
	_, err := withRetries(email+"/v1/send", func(int) string { return "receipt-" + ref(o) },
		map[string]any{"to": "customer-" + strconv.Itoa(o.id), "template": "receipt", "order": ref(o)})
	if err == nil && note("receipt %d", o.id) == nil {
		o.receipt = true
	}
}

func refund(o *order) error {
	if o.refund {
		return nil
	}
	_, err := withRetries(payments+"/v1/refunds", func(int) string { return "refund-" + ref(o) },
		map[string]any{"order": ref(o), "amount": o.amount})
	if err != nil {
		return err
	}
	if err := note("refunded %d", o.id); err != nil {
		return err
	}
	o.refund = true
	return nil
}

// ---------------------------------------------------------------- the HTTP API

func reply(w http.ResponseWriter, code int, v map[string]any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	json.NewEncoder(w).Encode(v)
}

func server() {
	if err := os.MkdirAll(dataDir, 0755); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	replay()
	var err error
	journal, err = os.OpenFile(journalP, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	go receipts()
	http.HandleFunc("POST /orders/{id}", place)
	http.HandleFunc("POST /orders/{id}/cancel", cancel)
	http.HandleFunc("POST /webhooks/payments", webhook)
	http.HandleFunc("GET /health", func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, "ok\n") })
	fmt.Fprintln(os.Stderr, "shop: listening on", listen)
	fmt.Fprintln(os.Stderr, http.ListenAndServe(listen, nil))
	os.Exit(1)
}

func place(w http.ResponseWriter, r *http.Request) {
	id, err := strconv.Atoi(r.PathValue("id"))
	if err != nil {
		reply(w, 400, map[string]any{"error": "bad id"})
		return
	}
	amount, _ := strconv.Atoi(r.URL.Query().Get("amount"))
	o := get(id)
	o.mu.Lock()
	defer o.mu.Unlock()
	switch o.status {
	case "paid":
		reply(w, 200, map[string]any{"status": "paid", "charge": o.charge})
		return
	case "cancelled":
		reply(w, 409, map[string]any{"status": "cancelled"})
		return
	case "":
		o.amount, o.status = amount, "placed"
		if err := note("placed %d %d", id, amount); err != nil {
			reply(w, 500, map[string]any{"error": err.Error()})
			return
		}
	}
	a, err := charge(o)
	if err != nil {
		reply(w, 502, map[string]any{"status": "payment_failed", "error": err.Error()})
		return
	}
	ch, _ := a.body["id"].(string)
	if err := note("charged %d %s", id, ch); err != nil {
		reply(w, 500, map[string]any{"error": err.Error()})
		return
	}
	o.status, o.charge = "paid", ch
	sendReceipt(o)
	reply(w, 200, map[string]any{"status": "paid", "charge": ch})
}

func cancel(w http.ResponseWriter, r *http.Request) {
	id, _ := strconv.Atoi(r.PathValue("id"))
	o := get(id)
	o.mu.Lock()
	defer o.mu.Unlock()
	if o.status == "" {
		reply(w, 404, map[string]any{"error": "no such order"})
		return
	}
	if o.status == "paid" {
		if err := refund(o); err != nil {
			reply(w, 502, map[string]any{"status": "refund_failed", "error": err.Error()})
			return
		}
	}
	if o.status != "cancelled" {
		if err := note("cancelled %d", id); err != nil {
			reply(w, 500, map[string]any{"error": err.Error()})
			return
		}
		o.status = "cancelled"
	}
	reply(w, 200, map[string]any{"status": "cancelled"})
}

// webhook: the provider confirms a charge. It may arrive late, twice, or out of order, and for a
// charge whose answer the shop never got (a timeout): then it is how the shop learns it was paid.
func webhook(w http.ResponseWriter, r *http.Request) {
	var ev struct {
		Type, Order, ID string
	}
	if err := json.NewDecoder(r.Body).Decode(&ev); err != nil || !strings.HasPrefix(ev.Order, "o") {
		reply(w, 400, map[string]any{"error": "bad event"})
		return
	}
	id, _ := strconv.Atoi(ev.Order[1:])
	mu.Lock()
	o := orders[id]
	mu.Unlock()
	if o == nil || ev.Type != "charge.succeeded" {
		reply(w, 200, map[string]any{"ignored": true})
		return
	}
	o.mu.Lock()
	defer o.mu.Unlock()
	switch o.status {
	case "cancelled": // charged after all, but the order is cancelled: give the money back
		if err := refund(o); err != nil {
			reply(w, 500, map[string]any{"error": err.Error()})
			return
		}
	case "placed":
		if err := note("charged %d %s", id, ev.ID); err != nil {
			reply(w, 500, map[string]any{"error": err.Error()})
			return
		}
		o.status, o.charge = "paid", ev.ID
		fallthrough
	case "paid":
		sendReceipt(o)
	}
	reply(w, 200, map[string]any{"ok": true})
}

// receipts: a paid order whose receipt could not be sent yet is retried every 2 s
func receipts() {
	for {
		time.Sleep(2 * time.Second)
		mu.Lock()
		var todo []*order
		for _, o := range orders {
			todo = append(todo, o)
		}
		mu.Unlock()
		for id := 0; id < 1000; id++ { // fixed order (map iteration order is random)
			for _, o := range todo {
				if o.id == id {
					o.mu.Lock()
					sendReceipt(o)
					o.mu.Unlock()
				}
			}
		}
	}
}

// ---------------------------------------------------------------- the client

func client(op, sid string) int {
	id, err := strconv.Atoi(sid)
	if err != nil {
		return 2
	}
	url := fmt.Sprintf("http://%s/orders/%d", listen, id)
	if op == "cancel" {
		url += "/cancel"
	} else {
		url += "?amount=" + strconv.Itoa(100+id)
	}
	c := &http.Client{Timeout: 10 * time.Second}
	resp, err := c.Post(url, "application/json", nil)
	if err != nil {
		fmt.Fprintf(os.Stderr, "shop %s %d: %v\n", op, id, err)
		return 1
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	fmt.Fprintf(os.Stderr, "shop %s %d: %d %s", op, id, resp.StatusCode, b)
	if resp.StatusCode == 200 {
		return 0
	}
	if resp.StatusCode == 409 || resp.StatusCode == 404 {
		return 3 // definitely not done
	}
	return 1 // outcome unknown
}
