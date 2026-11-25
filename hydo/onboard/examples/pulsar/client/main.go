// pulsarc: the test client of the Pulsar example. One static program with one subcommand per
// client operation. Each operation writes HYDO-HIST records (hydo guide messages): one record
// before the request and one after the reply. The history `p` has every send and the deliveries
// and acknowledgements of the subscriptions s (shared) and f (failover); the history `o` has
// every send with its key and the deliveries of f, for the order of each key.
//
//	pulsarc provision               create the topic t (3 partitions) and the subscriptions s, f, a
//	pulsarc produce <id> <seed>     two messages from the producer p<id>, with explicit sequence ids
//	pulsarc resend <id> <seed>      the producer r: first the message whose send got no answer, again
//	pulsarc consume <id> <seed>     a consumer of the shared subscription s: receive, ack each
//	pulsarc failover <id> <seed>    a consumer of the failover subscription f: receive, ack each
//	pulsarc txn <id> <seed>         one transaction: two messages, and the ack of one message of s
//	pulsarc unload <id> <seed>      the admin API unloads one partition: its broker hands it over
//	pulsarc drain                   the final read of s and f until each is empty
//	pulsarc txncheck                the transactions' messages in the subscription a: all or nothing
//	pulsarc hanging                 exit 1 while a partition's transaction buffer has an open transaction
//
// hvx keeps 64 KiB of the guest's console per boot, so the records are short: a receive and an
// acknowledgement carry every message of one batch, and empty receives are not recorded.
//
// Every choice comes from the seed hydo gives. Deadlines read /proc/uptime, which a clock step
// (T=) does not move.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/apache/pulsar-client-go/pulsar"
	plog "github.com/apache/pulsar-client-go/pulsar/log"
)

const (
	topic      = "persistent://public/default/t"
	partitions = 3
	serviceURL = "pulsar://broker1:6650,broker2:6650"
	keys       = 8
	// the file of the producer r's last message without an answer (resend), in the guest's /tmp
	pendingFile = "/tmp/pulsarc-resend-pending"
	// the most messages in one receive record (the console takes short lines)
	batchMax = 30
)

var webURLs = []string{"http://broker1:8080", "http://broker2:8080"}

// ---- the history recorder (the wire format of hydo_hist.sh) ----

var histOut *os.File

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

// msg is one message as the records name it: its id, its key, where it is stored, and its outcome
// in a batch (s=).
type msg struct {
	id, key string
	mid     pulsar.MessageID
	s       string
}

// format writes an item for the history h: the history p names where the message is stored
// (o=<ledger>.<entry>), the history o names its key (k=).
func (m msg) format(h string) string {
	s := m.id
	if h == "o" {
		if m.key != "" {
			s += ";k=" + m.key
		}
	} else if m.mid != nil && m.mid.LedgerID() >= 0 && m.mid.EntryID() >= 0 {
		s += fmt.Sprintf(";o=%d.%d", m.mid.LedgerID(), m.mid.EntryID())
	}
	if m.s != "" {
		s += ";s=" + m.s
	}
	return s
}

// rec writes one record to each history: HYDO-HIST <history> <process> <type> <time> <op> t [<group>] <items>
func rec(hists []string, proc int64, typ, op, group string, items []msg) {
	if histOut == nil {
		return
	}
	t := uptimeNs()
	var b strings.Builder
	for _, h := range hists {
		fmt.Fprintf(&b, "HYDO-HIST %s %d %s %s %s t", h, proc, typ, t, op)
		if group != "" {
			b.WriteString(" " + group)
		}
		for _, m := range items {
			b.WriteString(" " + m.format(h))
		}
		b.WriteString("\n")
	}
	histOut.WriteString(b.String())
}

// event writes an assertion record that reactions (A=<id>...) and hydo explore --reactive-at aim
// at: pulsar.sent after a send's answer, pulsar.acked after the broker answered an
// acknowledgement, pulsar.txn_end just before a transaction's commit or abort.
func event(id, what string) {
	if histOut != nil {
		histOut.WriteString(`HYDO-ASSERT {"t":"sometimes","id":"` + id + `","ok":true,"msg":"` + what + `"}` + "\n")
	}
}

var (
	both  = []string{"p", "o"}
	plain = []string{"p"}
)

// the histories of a subscription's records: f's deliveries are judged for order too
func histsOf(sub string) []string {
	if sub == "f" {
		return both
	}
	return plain
}

// ---- helpers ----

func uptime() float64 {
	b, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return 0
	}
	f := strings.Fields(string(b))
	v, _ := strconv.ParseFloat(f[0], 64)
	return v
}

func die(code int, f string, a ...any) {
	fmt.Fprintf(os.Stderr, f+"\n", a...)
	os.Exit(code)
}

func atoi(s string) int64 {
	v, err := strconv.ParseInt(s, 10, 64)
	if err != nil {
		die(2, "not a number: %s", s)
	}
	return v
}

func newClient(txn bool) pulsar.Client {
	cl, err := pulsar.NewClient(pulsar.ClientOptions{
		URL:               serviceURL,
		ConnectionTimeout: 3 * time.Second,
		OperationTimeout:  8 * time.Second,
		EnableTransaction: txn,
		Logger:            plog.DefaultNopLogger(),
	})
	if err != nil {
		die(2, "client: %v", err)
	}
	return cl
}

func keyOf(seed int64) string { return fmt.Sprintf("k%d", seed%keys) }

// ---- provision ----

// httpOne sends one request to one URL.
func httpOne(method, url string) (int, string, error) {
	req, err := http.NewRequest(method, url, nil)
	if err != nil {
		return 0, "", err
	}
	c := &http.Client{Timeout: 8 * time.Second}
	resp, err := c.Do(req)
	if err != nil {
		return 0, "", err
	}
	b, _ := io.ReadAll(resp.Body)
	resp.Body.Close()
	return resp.StatusCode, string(b), nil
}

func httpDo(method, path, body string) (int, string, error) {
	var last error
	for _, u := range webURLs {
		req, err := http.NewRequest(method, u+path, strings.NewReader(body))
		if err != nil {
			return 0, "", err
		}
		req.Header.Set("Content-Type", "application/json")
		c := &http.Client{Timeout: 8 * time.Second}
		resp, err := c.Do(req)
		if err != nil {
			last = err
			continue
		}
		b, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		return resp.StatusCode, string(b), nil
	}
	return 0, "", last
}

func provision() {
	end := uptime() + 300
	step := func(what, method, path, body string, okCodes ...int) {
		for uptime() < end {
			code, out, err := httpDo(method, path, body)
			if err == nil {
				for _, c := range okCodes {
					if code == c {
						fmt.Printf("provision: %s: %d\n", what, code)
						return
					}
				}
				fmt.Fprintf(os.Stderr, "provision: %s: %d %s\n", what, code, strings.TrimSpace(out))
			} else {
				fmt.Fprintf(os.Stderr, "provision: %s: %v\n", what, err)
			}
			time.Sleep(2 * time.Second)
		}
		die(1, "provision: %s: no success within 300 s", what)
	}
	step("the partitioned topic t", "PUT", "/admin/v2/persistent/public/default/t/partitions", strconv.Itoa(partitions), 204, 409)
	for _, s := range []string{"s", "f", "a"} {
		// every subscription starts at the earliest message, before the first send
		step("the subscription "+s, "PUT", "/admin/v2/persistent/public/default/t/subscription/"+s,
			`{"ledgerId":-1,"entryId":-1,"partitionIndex":-1}`, 204, 409)
	}
	// a transaction that commits nothing: the coordinators are ready
	for uptime() < end {
		cl := newClient(true)
		txn, err := cl.NewTransaction(10 * time.Second)
		if err == nil {
			ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
			err = txn.Abort(ctx)
			cancel()
		}
		cl.Close()
		if err == nil {
			fmt.Println("provision: the transaction coordinators answer")
			break
		}
		fmt.Fprintf(os.Stderr, "provision: a transaction: %v\n", err)
		time.Sleep(2 * time.Second)
	}
	// one producer per partition: every partition is loaded by its broker
	cl := newClient(false)
	defer cl.Close()
	for uptime() < end {
		p, err := cl.CreateProducer(pulsar.ProducerOptions{Topic: topic, Name: "provision", DisableBatching: true})
		if err == nil {
			p.Close()
			fmt.Println("provision: topic t ready")
			return
		}
		fmt.Fprintf(os.Stderr, "provision: a producer: %v\n", err)
		time.Sleep(2 * time.Second)
	}
	die(1, "provision: no producer within 300 s")
}

// ---- produce ----

// sendOne sends one message with an explicit sequence id from the producer of name `name`. A send
// that fails is sent once more from a new producer of the same name with the same sequence id,
// which deduplication must drop when the first one was stored. It returns the message id of the
// stored message (ledger -1 for a duplicate the broker dropped) or an error.
func sendOne(cl pulsar.Client, name, id, key string, seq int64) (pulsar.MessageID, error) {
	var lastErr error
	for attempt := 0; attempt < 2; attempt++ {
		p, err := cl.CreateProducer(pulsar.ProducerOptions{
			Topic:           topic,
			Name:            name,
			DisableBatching: true,
			SendTimeout:     8 * time.Second,
		})
		if err != nil {
			lastErr = err
			time.Sleep(time.Second)
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		s := seq
		mid, err := p.Send(ctx, &pulsar.ProducerMessage{Payload: []byte(id), Key: key, SequenceID: &s})
		cancel()
		p.Close()
		if err == nil {
			return mid, nil
		}
		lastErr = err
	}
	return nil, lastErr
}

// sendAll sends the messages one after another from one producer name, as one batch record: each
// item ends ok (with where it is stored), info (no answer: it may be stored) or fail (never sent,
// because a send before it got no answer). It returns false when a send got no answer.
func sendAll(cl pulsar.Client, proc int64, name string, ms []msg, seqs []int64) bool {
	rec(both, proc, "invoke", "send", "", ms)
	ok := true
	for i := range ms {
		if !ok {
			ms[i].s = "fail"
			continue
		}
		mid, err := sendOne(cl, name, ms[i].id, ms[i].key, seqs[i])
		if err != nil {
			fmt.Fprintf(os.Stderr, "send %s: %v\n", ms[i].id, err)
			ms[i].s = "info"
			ok = false
			continue
		}
		ms[i].mid, ms[i].s = mid, "ok"
		event("pulsar.sent", "the broker answered a send")
		fmt.Printf("sent %s %s at %v\n", ms[i].id, ms[i].key, mid)
	}
	typ := "ok"
	if !ok {
		typ = "info"
	}
	rec(both, proc, typ, "send", "", ms)
	return ok
}

func produce(id, seed int64) {
	cl := newClient(false)
	defer cl.Close()
	ms := []msg{
		{id: fmt.Sprintf("m%d.1", id), key: keyOf(seed)},
		{id: fmt.Sprintf("m%d.2", id), key: keyOf(seed/8 + 1)},
	}
	if !sendAll(cl, id, fmt.Sprintf("p%d", id), ms, []int64{1, 2}) {
		os.Exit(1)
	}
}

// ---- resend: deduplication across producer restarts ----

// resend is a serial driver, so the producer r has one process at a time. The sequence ids grow
// with the command ids. A message whose send got no answer is kept in a file and sent again with
// its sequence id by the next command, as a client that restarts after a crash does.
func resend(id, seed int64) {
	cl := newClient(false)
	defer cl.Close()
	var ms []msg
	var seqs []int64
	if b, err := os.ReadFile(pendingFile); err == nil {
		if f := strings.Fields(string(b)); len(f) == 3 {
			ms = append(ms, msg{id: f[0], key: f[1]})
			seqs = append(seqs, atoi(f[2]))
		}
	}
	m := msg{id: fmt.Sprintf("m%d.r", id), key: keyOf(seed)}
	seq := id * 10
	ms = append(ms, m)
	seqs = append(seqs, seq)
	// the new message is pending until its send is answered
	os.WriteFile(pendingFile, []byte(fmt.Sprintf("%s %s %d\n", m.id, m.key, seq)), 0o644)
	ok := sendAll(cl, id, "r", ms, seqs)
	if ms[len(ms)-1].s == "ok" {
		os.Remove(pendingFile)
	} else if len(ms) == 2 && ms[0].s != "ok" {
		// the old message is still without an answer: keep it, the new one was never sent
		os.WriteFile(pendingFile, []byte(fmt.Sprintf("%s %s %d\n", ms[0].id, ms[0].key, seqs[0])), 0o644)
	}
	if !ok {
		os.Exit(1)
	}
}

// ---- consume ----

func subType(sub string) pulsar.SubscriptionType {
	switch sub {
	case "s", "a":
		return pulsar.Shared
	case "f":
		return pulsar.Failover
	}
	return pulsar.Exclusive
}

func subscribe(cl pulsar.Client, sub, name string) (pulsar.Consumer, error) {
	return cl.Subscribe(pulsar.ConsumerOptions{
		Topic:                       topic,
		SubscriptionName:            sub,
		Name:                        name,
		Type:                        subType(sub),
		SubscriptionInitialPosition: pulsar.SubscriptionPositionEarliest,
		AckWithResponse:             true,
		ReceiverQueueSize:           20,
	})
}

func msgOf(m pulsar.Message) msg {
	return msg{id: string(m.Payload()), key: m.Key(), mid: m.ID()}
}

// receiveBatch receives until max messages, or until nothing arrives within wait. It records one
// receive with every message, or nothing when it received none. A receive error ends the batch.
func receiveBatch(c pulsar.Consumer, h []string, proc int64, sub string, max int, wait time.Duration) ([]pulsar.Message, error) {
	t0 := uptimeNs()
	var got []pulsar.Message
	var rerr error
	for len(got) < max {
		ctx, cancel := context.WithTimeout(context.Background(), wait)
		m, err := c.Receive(ctx)
		cancel()
		if err != nil {
			if !errors.Is(err, context.DeadlineExceeded) {
				rerr = err
			}
			break
		}
		got = append(got, m)
	}
	if len(got) > 0 && histOut != nil {
		items := make([]msg, len(got))
		for i, m := range got {
			items[i] = msgOf(m)
		}
		var b strings.Builder
		for _, hh := range h {
			fmt.Fprintf(&b, "HYDO-HIST %s %d invoke %s recv t %s\n", hh, proc, t0, sub)
		}
		histOut.WriteString(b.String())
		rec(h, proc, "ok", "recv", sub, items)
	}
	return got, rerr
}

// ackBatch acknowledges every message and waits for the broker's answers (AckWithResponse). The
// record is ok when every answer came, else info.
func ackBatch(c pulsar.Consumer, proc int64, sub string, got []pulsar.Message) error {
	if len(got) == 0 {
		return nil
	}
	items := make([]msg, len(got))
	for i, m := range got {
		items[i] = msgOf(m)
	}
	rec(plain, proc, "invoke", "ack", sub, items)
	var first error
	for _, m := range got {
		if err := c.Ack(m); err != nil && first == nil {
			first = err
		}
	}
	if first != nil {
		rec(plain, proc, "info", "ack", sub, items)
		return first
	}
	rec(plain, proc, "ok", "ack", sub, items)
	event("pulsar.acked", "the broker answered an acknowledgement")
	return nil
}

// consumeSub receives up to max messages within 6 s and acknowledges them. A consumer of f is a
// serial driver: one process holds f at a time.
func consumeSub(sub string, id, seed int64) {
	h := histsOf(sub)
	max := int(5 + seed%20)
	cl := newClient(false)
	defer cl.Close()
	c, err := subscribe(cl, sub, fmt.Sprintf("%s%d", sub, id))
	if err != nil {
		die(1, "subscribe %s: %v", sub, err)
	}
	defer c.Close()
	got, rerr := receiveBatch(c, h, id, sub, max, 1500*time.Millisecond)
	if err := ackBatch(c, id, sub, got); err != nil {
		die(1, "%s: ack: %v", sub, err)
	}
	if rerr != nil {
		die(1, "%s: receive: %v", sub, rerr)
	}
	fmt.Printf("%s: received and acknowledged %d\n", sub, len(got))
}

// ---- txn ----

// txn sends two messages and acknowledges one message of s inside one transaction, then commits
// (3 in 4) or aborts. In 1 of 8 it exits instead, as a client that crashes in the middle. A commit that completed makes both sends ok and the ack ok; an abort that
// completed makes them fail; anything else is info. The outcome also goes to $HYDO_OUTCOMES as
// x<id>, which the check final_txn reads.
func txn(id, seed int64) {
	commit := (seed/5)%4 != 0
	crash := (seed/7)%8 == 0 // 1 in 8: the client exits before the commit or the abort
	cl := newClient(true)
	defer cl.Close()
	ms := []msg{{id: fmt.Sprintf("m%d.a", id), key: keyOf(seed)}, {id: fmt.Sprintf("m%d.b", id), key: keyOf(seed/3 + 1)}}
	outcome := func(o string) {
		if f := os.Getenv("HYDO_OUTCOMES"); f != "" {
			if fh, err := os.OpenFile(f, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644); err == nil {
				fmt.Fprintf(fh, "x%d %s\n", id, o)
				fh.Close()
			}
		}
	}
	tx, err := cl.NewTransaction(10 * time.Second)
	if err != nil {
		die(1, "txn %d: begin: %v", id, err)
	}
	outcome("unknown")
	p, err := cl.CreateProducer(pulsar.ProducerOptions{Topic: topic, Name: fmt.Sprintf("x%d", id),
		DisableBatching: true, SendTimeout: -1})
	if err != nil {
		die(1, "txn %d: producer: %v", id, err)
	}
	defer p.Close()
	rec(both, id, "invoke", "send", "", ms)
	setAll := func(s string) {
		for i := range ms {
			ms[i].s = s
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	for i := range ms {
		mid, err := p.Send(ctx, &pulsar.ProducerMessage{Payload: []byte(ms[i].id), Key: ms[i].key, Transaction: tx})
		if err != nil {
			fmt.Fprintf(os.Stderr, "txn %d: send %s: %v\n", id, ms[i].id, err)
			commit = false
			break
		}
		ms[i].mid = mid
	}
	// one message of the shared subscription, acknowledged inside the transaction. Its receive and
	// acknowledgement are records of their own process, 1000000 + id: a process has one operation
	// in flight, and the sends are still in flight until the commit.
	ap := 1000000 + id
	var acked []msg
	if c, err := subscribe(cl, "s", fmt.Sprintf("x%d", id)); err == nil {
		defer c.Close()
		got, _ := receiveBatch(c, plain, ap, "s", 1, 1500*time.Millisecond)
		if len(got) == 1 {
			acked = []msg{msgOf(got[0])}
			if err := c.AckWithTxn(got[0], tx); err != nil {
				fmt.Fprintf(os.Stderr, "txn %d: ack in the transaction: %v\n", id, err)
				commit = false
			}
		}
	}
	if len(acked) > 0 {
		rec(plain, ap, "invoke", "ack", "s", acked)
	}
	end := func(typ string) {
		rec(both, id, typ, "send", "", ms)
		if len(acked) > 0 {
			rec(plain, ap, typ, "ack", "s", acked)
		}
	}
	if crash {
		// the client stops here as a crashed process does: the transaction stays open until the
		// coordinator aborts it after its timeout of 10 s, and the partitions hold back every
		// later message until then (consumers read up to the first open transaction)
		setAll("info")
		end("info")
		fmt.Printf("left %s %s in an open transaction\n", ms[0].id, ms[1].id)
		os.Exit(3)
	}
	event("pulsar.txn_end", "a transaction ends")
	if commit {
		if err := tx.Commit(ctx); err != nil {
			setAll("info")
			end("info")
			die(1, "txn %d: commit: %v", id, err)
		}
		setAll("ok")
		end("ok")
		outcome("ack")
		fmt.Printf("committed %s %s\n", ms[0].id, ms[1].id)
		return
	}
	if err := tx.Abort(ctx); err != nil {
		setAll("info")
		end("info")
		die(1, "txn %d: abort: %v", id, err)
	}
	for i := range ms {
		ms[i].mid = nil
	}
	setAll("fail")
	end("fail")
	outcome("failed")
	fmt.Printf("aborted %s %s\n", ms[0].id, ms[1].id)
}

// ---- unload ----

// unload asks the admin API to unload one partition of t, chosen by the seed. Its broker closes
// the partition and the next lookup places it again, as an operator or the load balancer does.
func unload(id, seed int64) {
	part := seed % partitions
	code, out, err := httpDo("PUT", fmt.Sprintf("/admin/v2/persistent/public/default/t-partition-%d/unload", part), "")
	if err != nil {
		die(1, "unload t-partition-%d: %v", part, err)
	}
	if code != 204 {
		die(1, "unload t-partition-%d: %d %s", part, code, strings.TrimSpace(out))
	}
	fmt.Printf("unloaded t-partition-%d\n", part)
}

// ---- drain ----

// drainSub receives from one subscription until nothing arrives for a quiet window after the last
// message or error, and acknowledges what it receives. An error is "not yet". Without records
// (h nil), it only collects the message ids into seen.
func drainSub(cl pulsar.Client, sub string, h []string, proc int64, deadline float64, seen map[string]bool) bool {
	const quiet = 12.0
	var c pulsar.Consumer
	last := uptime()
	for uptime() < deadline {
		if c == nil {
			var err error
			c, err = subscribe(cl, sub, fmt.Sprintf("drain-%s", sub))
			if err != nil {
				fmt.Fprintf(os.Stderr, "drain %s: subscribe: %v\n", sub, err)
				last = uptime()
				time.Sleep(time.Second)
				continue
			}
			rec(h, proc, "invoke", "sub", sub, []msg{{id: "from=start"}})
			rec(h, proc, "ok", "sub", sub, []msg{{id: "from=start"}})
		}
		got, rerr := receiveBatch(c, h, proc, sub, batchMax, time.Second)
		for _, m := range got {
			if seen != nil {
				seen[string(m.Payload())] = true
			}
		}
		if len(got) > 0 {
			ackBatch(c, proc, sub, got)
			last = uptime()
		}
		if rerr != nil {
			fmt.Fprintf(os.Stderr, "drain %s: receive: %v\n", sub, rerr)
			last = uptime()
			time.Sleep(time.Second)
			continue
		}
		if len(got) == 0 && uptime()-last >= quiet {
			rec(h, proc, "ok", "drained", sub, nil)
			c.Close()
			return true
		}
	}
	fmt.Fprintf(os.Stderr, "drain %s: no quiet window of %.0f s before the deadline\n", sub, quiet)
	if c != nil {
		c.Close()
	}
	return false
}

func drain() {
	cl := newClient(false)
	defer cl.Close()
	deadline := uptime() + 230
	ok := true
	for i, sub := range []string{"s", "f"} {
		if !drainSub(cl, sub, histsOf(sub), int64(100001+i), deadline, nil) {
			ok = false
		}
	}
	if !ok {
		os.Exit(1)
	}
}

// ---- txncheck ----

// txncheck drains the subscription a, which nothing else reads, and judges every transaction of
// $HYDO_OUTCOMES: a committed one has both of its messages, an aborted one neither, and one
// without a known outcome both or neither. Exit 0 holds, 1 violated, 2 cannot be judged now.
func txncheck() {
	outcomes := map[string]string{}
	if f := os.Getenv("HYDO_OUTCOMES"); f != "" {
		if fh, err := os.Open(f); err == nil {
			sc := bufio.NewScanner(fh)
			for sc.Scan() {
				fs := strings.Fields(sc.Text())
				if len(fs) == 2 && strings.HasPrefix(fs[0], "x") {
					outcomes[fs[0][1:]] = fs[1]
				}
			}
			fh.Close()
		}
	}
	histOut = nil // the check's reads are not part of the histories
	cl := newClient(false)
	defer cl.Close()
	seen := map[string]bool{}
	if !drainSub(cl, "a", nil, 100010, uptime()+100, seen) {
		fmt.Println("the subscription a gave no quiet window")
		os.Exit(2)
	}
	var bad []string
	counts := map[string]int{}
	n := 0
	for id, o := range outcomes {
		a, b := seen["m"+id+".a"], seen["m"+id+".b"]
		n++
		switch {
		case o == "ack" && !(a && b):
			bad = append(bad, fmt.Sprintf("x%d committed, delivered a=%v b=%v", atoi(id), a, b))
			counts["committed"]++
		case o == "failed" && (a || b):
			bad = append(bad, fmt.Sprintf("x%d aborted, delivered a=%v b=%v", atoi(id), a, b))
			counts["aborted"]++
		case o == "unknown" && a != b:
			bad = append(bad, fmt.Sprintf("x%d unknown, delivered a=%v b=%v", atoi(id), a, b))
			counts["unknown"]++
		}
	}
	sort.Strings(bad)
	for _, l := range bad {
		fmt.Println(l)
	}
	if len(bad) > 0 {
		fmt.Printf("%d of %d transactions not all or nothing: %d committed, %d aborted, %d unknown\n",
			len(bad), n, counts["committed"], counts["aborted"], counts["unknown"])
		os.Exit(1)
	}
	fmt.Printf("all %d transactions all or nothing\n", n)
}

// ---- hanging ----

// hanging asks each partition's transaction buffer for its open transactions. A transaction that
// its client left open must end after its timeout of 10 s; until it ends, the partition delivers
// nothing stored after the transaction's first message. Exit 0: no partition has one; 1: one has;
// 2: no broker answered. The admin API answers 404 for a partition that no broker has loaded, as
// in a recovery boot before any client came, so the check first loads every partition with a
// producer that sends nothing (no transaction: it does not load a coordinator). A partition that
// both brokers still answer with 404 is left out.
func hanging() {
	cl := newClient(false)
	end := uptime() + 30
	for uptime() < end {
		p, err := cl.CreateProducer(pulsar.ProducerOptions{Topic: topic, Name: "hanging", DisableBatching: true})
		if err == nil {
			p.Close()
			break
		}
		time.Sleep(time.Second)
	}
	cl.Close()
	open, answered := 0, 0
	for p := 0; p < partitions; p++ {
		path := fmt.Sprintf("/admin/v3/transactions/transactionBufferStats/public/default/t-partition-%d", p)
		code, out, err := 0, "", error(nil)
		notFound := 0
		for _, u := range webURLs {
			code, out, err = httpOne("GET", u+path)
			if err == nil && code == 200 {
				break
			}
			if err == nil && code == 404 {
				notFound++
			}
		}
		if notFound == len(webURLs) {
			fmt.Printf("t-partition-%d: no transaction buffer loaded (404)\n", p)
			continue
		}
		if err != nil || code != 200 {
			fmt.Printf("t-partition-%d: no answer (%d %v)\n", p, code, err)
			os.Exit(2)
		}
		answered++
		var st struct {
			MaxReadPosition string `json:"maxReadPosition"`
			OngoingTxnSize  int64  `json:"ongoingTxnSize"`
		}
		if err := json.Unmarshal([]byte(out), &st); err != nil {
			fmt.Printf("t-partition-%d: %v\n", p, err)
			os.Exit(2)
		}
		if st.OngoingTxnSize > 0 {
			fmt.Printf("t-partition-%d: %d open transactions, reads end at %s\n", p, st.OngoingTxnSize, st.MaxReadPosition)
			open++
		}
	}
	if open > 0 {
		fmt.Printf("%d of %d partitions hold back their messages behind an open transaction\n", open, partitions)
		os.Exit(1)
	}
	if answered == 0 {
		fmt.Println("no partition's transaction buffer answered")
		os.Exit(2)
	}
	fmt.Println("no open transaction on any partition")
}

func main() {
	if os.Getenv("HYDO_HIST_OUT") != "off" {
		path := os.Getenv("HYDO_HIST_OUT")
		if path == "" {
			path = "/dev/console"
		}
		f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
		if err == nil {
			histOut = f
		}
	}
	if len(os.Args) < 2 {
		die(2, "usage: pulsarc provision|produce|resend|consume|failover|txn|drain|txncheck ...")
	}
	arg := func(i int) int64 {
		if len(os.Args) <= i {
			die(2, "missing argument")
		}
		return atoi(os.Args[i])
	}
	switch os.Args[1] {
	case "provision":
		provision()
	case "produce":
		produce(arg(2), arg(3))
	case "resend":
		resend(arg(2), arg(3))
	case "consume":
		consumeSub("s", arg(2), arg(3))
	case "failover":
		consumeSub("f", arg(2), arg(3))
	case "txn":
		txn(arg(2), arg(3))
	case "unload":
		unload(arg(2), arg(3))
	case "drain":
		drain()
	case "txncheck":
		txncheck()
	case "hanging":
		hanging()
	default:
		die(2, "unknown command %s", os.Args[1])
	}
}
