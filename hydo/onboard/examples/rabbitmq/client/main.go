// rmqc: the test client of the RabbitMQ example, for hydo's workload. It records every operation
// as a HYDO-HIST record and prints events for reactions. Three histories, one per check:
//
//	q  the quorum queue j (group w, competing consumers) and its dead-letter queue jd (group -)
//	o  the quorum queue k with a single active consumer (group -), keyed messages
//	s  the stream s: the reader r with offsets kept by the server, and the drain's group all
//
// Commands:
//
//	rmqc provision        declare j, jd, k and s (once, before the workload)
//	rmqc pub <id>         publish one message to j with a publisher confirm
//	rmqc consume <proc>   take one message from j, hold it 1 s, acknowledge it; crash on a poison
//	                      message (an id that starts with x), so that RabbitMQ returns it
//	rmqc kpub <id>        publish one keyed message to k with a publisher confirm
//	rmqc sac <proc>       consume k for up to 3 s as one of its consumers, acknowledging each
//	rmqc spub <id>        publish one message to the stream s with a confirm (stream protocol)
//	rmqc sread <proc>     read up to 4 messages of s as the consumer r from its stored offset,
//	                      then store the offset of the last one
//	rmqc drain            take every message left of every group, then read back j's final states
//	rmqc probe            publish to j and to s, retrying for 60 s (after the faults; the drain
//	                      starts with one recorded publish to each)
//	rmqc ready <node>     exit 0 when the node takes an AMQP connection and a passive declare
//	rmqc leader <queue>   print the node that leads a quorum queue or a stream (the stream protocol)
package main

import (
	"context"
	"errors"
	"fmt"
	"math/rand"
	"os"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydohist"
	amqp "github.com/rabbitmq/amqp091-go"
	samqp "github.com/rabbitmq/rabbitmq-stream-go-client/pkg/amqp"
	"github.com/rabbitmq/rabbitmq-stream-go-client/pkg/stream"
)

var nodes = []string{"rabbit1", "rabbit2", "rabbit3"}

const (
	qJobs   = "j"  // quorum queue, delivery limit, dead-lettering at least once into jd
	qDead   = "jd" // quorum queue: j's dead letters
	qKeyed  = "k"  // quorum queue with a single active consumer
	sStream = "s"  // stream
	reader  = "r"  // the stream consumer whose offset the server keeps

	delLog   = "/data/client/delivered"   // ids of j that a consumer acknowledged (or the drain took from jd)
	ackUnk   = "/data/client/ack-unknown" // ids of j whose acknowledgement got no answer
	limitEnv = "RMQ_DELIVERY_LIMIT"
	replEnv  = "RMQ_REPLICAS" // replicas of each queue and of the stream (3: one per node)
)

var (
	hq = hydohist.New("q")
	ho = hydohist.New("o")
	hs = hydohist.New("s")
)

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: rmqc provision | pub <id> | consume <proc> | kpub <id> | sac <proc> | spub <id> | sread <proc> | drain | probe | ready <node> | leader <queue>")
		os.Exit(2)
	}
	arg := func() string {
		if len(os.Args) < 3 {
			fmt.Fprintln(os.Stderr, "rmqc: missing argument")
			os.Exit(2)
		}
		return os.Args[2]
	}
	switch os.Args[1] {
	case "provision":
		os.Exit(provision())
	case "pub":
		os.Exit(pub(arg()))
	case "consume":
		os.Exit(consume(num(arg())))
	case "kpub":
		os.Exit(kpub(arg()))
	case "sac":
		os.Exit(sac(num(arg())))
	case "spub":
		os.Exit(spub(arg()))
	case "sread":
		os.Exit(sread(num(arg())))
	case "drain":
		os.Exit(drain())
	case "probe":
		os.Exit(probe())
	case "ready":
		os.Exit(ready(arg()))
	case "leader":
		os.Exit(leader(arg()))
	}
	os.Exit(2)
}

func num(s string) int {
	var n int
	fmt.Sscanf(strings.TrimLeft(s, "mxos"), "%d", &n)
	return n
}

// event prints a console event for reactions (A=<event>).
func event(id, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	fmt.Fprintf(f, "HYDO-ASSERT {\"t\":\"sometimes\",\"id\":%q,\"ok\":true,\"msg\":%q}\n", id, msg)
	f.Close()
}

func uptime() float64 {
	b, _ := os.ReadFile("/proc/uptime")
	var u float64
	fmt.Sscanf(string(b), "%f", &u)
	return u
}

func appendLine(path, line string) {
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return
	}
	fmt.Fprintln(f, line)
	f.Sync()
	f.Close()
}

func lines(path string) map[string]bool {
	set := map[string]bool{}
	if b, err := os.ReadFile(path); err == nil {
		for _, l := range strings.Split(string(b), "\n") {
			if l != "" {
				set[l] = true
			}
		}
	}
	return set
}

func outcome(id, what string) {
	if p := os.Getenv("HYDO_OUTCOMES"); p != "" {
		appendLine(p, id+" "+what)
	}
}

// ---- AMQP 0.9.1 ----

// dial connects to one node, the first of nodes rotated by start, and tries the others when a
// node refuses: a refused connection published nothing.
func dial(start int) (*amqp.Connection, string, error) {
	var last error
	for i := 0; i < len(nodes); i++ {
		n := nodes[(start+i)%len(nodes)]
		c, err := amqp.DialConfig("amqp://guest:guest@"+n+":5672/", amqp.Config{
			Heartbeat: 5 * time.Second,
			Dial:      amqp.DefaultDial(3 * time.Second),
			Locale:    "en_US",
		})
		if err == nil {
			return c, n, nil
		}
		last = err
	}
	return nil, "", last
}

func ready(node string) int {
	c, err := amqp.DialConfig("amqp://guest:guest@"+node+":5672/", amqp.Config{Dial: amqp.DefaultDial(2 * time.Second)})
	if err != nil {
		return 1
	}
	defer c.Close()
	ch, err := c.Channel()
	if err != nil {
		return 1
	}
	defer ch.Close()
	return 0
}

func provision() int {
	limit := 2
	if v := os.Getenv(limitEnv); v != "" {
		fmt.Sscanf(v, "%d", &limit)
	}
	repl := replicas()
	deadline := uptime() + 180
	// First every node serves AMQP: a node counts as ready when its Erlang distribution port
	// answers, before it has joined the cluster. A queue or a stream declared before a node
	// joined gets no replica there, so the declaration waits for all three.
	for _, n := range nodes {
		for ready(n) != 0 {
			if uptime() > deadline {
				fmt.Println("provision:", n, "does not take AMQP connections")
				return 1
			}
			time.Sleep(time.Second)
		}
	}
	for uptime() < deadline {
		if err := declare(limit, repl); err != nil {
			fmt.Println("provision:", err)
			time.Sleep(2 * time.Second)
			continue
		}
		// every node serves the stream protocol and names the stream's leader and its replicas
		var err error
		for _, n := range nodes {
			env, e := senvOf(n)
			if e == nil {
				var md *stream.StreamMetadata
				md, e = env.StreamMetaData(sStream)
				if e == nil && (md.Leader == nil || int64(len(md.Replicas)+1) != repl) {
					e = fmt.Errorf("the stream has %d replicas besides its leader, not %d", len(md.Replicas), repl-1)
				}
				env.Close()
			}
			if e != nil {
				err = fmt.Errorf("%s: stream: %v", n, e)
				break
			}
		}
		if err != nil {
			fmt.Println("provision:", err)
			time.Sleep(2 * time.Second)
			continue
		}
		return 0
	}
	fmt.Println("provision: gave up")
	return 1
}

func replicas() int64 {
	n := int64(3)
	if v := os.Getenv(replEnv); v != "" {
		fmt.Sscanf(v, "%d", &n)
	}
	return n
}

func declare(limit int, repl int64) error {
	c, _, err := dial(0)
	if err != nil {
		return err
	}
	defer c.Close()
	ch, err := c.Channel()
	if err != nil {
		return err
	}
	defer ch.Close()
	if _, err := ch.QueueDeclare(qDead, true, false, false, false, amqp.Table{"x-queue-type": "quorum", "x-quorum-initial-group-size": repl}); err != nil {
		return err
	}
	if _, err := ch.QueueDeclare(qJobs, true, false, false, false, amqp.Table{
		"x-queue-type":                "quorum",
		"x-quorum-initial-group-size": repl,
		"x-delivery-limit":            int64(limit),
		"x-dead-letter-exchange":      "",
		"x-dead-letter-routing-key":   qDead,
		"x-dead-letter-strategy":      "at-least-once",
		"x-overflow":                  "reject-publish",
	}); err != nil {
		return err
	}
	if _, err := ch.QueueDeclare(qKeyed, true, false, false, false, amqp.Table{
		"x-queue-type":                "quorum",
		"x-single-active-consumer":    true,
		"x-quorum-initial-group-size": repl,
	}); err != nil {
		return err
	}
	_, err = ch.QueueDeclare(sStream, true, false, false, false, amqp.Table{
		"x-queue-type":                    "stream",
		"x-stream-max-segment-size-bytes": int64(1 << 20),
		"x-initial-cluster-size":          repl,
	})
	return err
}

// publish sends one persistent message to a queue with a publisher confirm. It returns "ok"
// (confirmed), "fail" (nothing was sent: no connection), or "info" (sent, no confirm in time,
// a nack, or the connection broke: the message may be stored).
func publish(start int, queue, id string, headers amqp.Table) (string, string) {
	c, node, err := dial(start)
	if err != nil {
		return "fail", err.Error()
	}
	defer c.Close()
	ch, err := c.Channel()
	if err != nil {
		return "fail", err.Error()
	}
	if err := ch.Confirm(false); err != nil {
		return "fail", err.Error()
	}
	confirms := ch.NotifyPublish(make(chan amqp.Confirmation, 1))
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	err = ch.PublishWithContext(ctx, "", queue, false, false, amqp.Publishing{
		DeliveryMode: amqp.Persistent,
		MessageId:    id,
		Headers:      headers,
		Body:         []byte(id),
	})
	if err != nil {
		return "info", node + ": " + err.Error()
	}
	select {
	case cf, ok := <-confirms:
		if ok && cf.Ack {
			return "ok", node
		}
		if ok {
			return "info", node + ": nack"
		}
		return "info", node + ": channel closed before the confirm"
	case <-time.After(10 * time.Second):
		return "info", node + ": no confirm within 10 s"
	}
}

func pub(id string) int {
	p := num(id)
	op := hq.Invoke(p, "send", qJobs, id)
	res, why := publish(p, qJobs, id, nil)
	switch res {
	case "ok":
		op.Ok(qJobs, id)
		outcome(id, "ack")
		event("rmq.pub_confirmed", "a quorum queue publish was confirmed")
		return 0
	case "fail":
		op.Fail()
		outcome(id, "failed")
	default:
		op.Info()
		outcome(id, "unknown")
	}
	fmt.Println("pub", id, res, why)
	return 1
}

func kpub(id string) int {
	p := num(id)
	key := fmt.Sprintf("g%d", p%3)
	item := id + ";k=" + key
	op := ho.Invoke(p, "send", qKeyed, item)
	res, why := publish(p, qKeyed, id, amqp.Table{"key": key})
	switch res {
	case "ok":
		op.Ok(qKeyed, item)
		return 0
	case "fail":
		op.Fail()
	default:
		op.Info()
	}
	fmt.Println("kpub", id, res, why)
	return 1
}

// syncAck acknowledges one delivery and waits until the channel processed it: basic.ack has no
// answer, so a passive declare on the same channel after it must come back.
func syncAck(ch *amqp.Channel, d amqp.Delivery, queue string) error {
	if err := d.Ack(false); err != nil {
		return err
	}
	_, err := ch.QueueDeclarePassive(queue, true, false, false, false, nil)
	return err
}

// giveUp ends a consumer that got nothing in time. A message can reach the client just as it
// gives up; the exit would then return it as a failed delivery, which counts against the delivery
// limit although no consumer saw it. So the consumer cancels itself and gives such a late message
// back with a requeue, which RabbitMQ does not count. The client never saw it, so no record.
func giveUp(ch *amqp.Channel, dels <-chan amqp.Delivery) {
	ch.Cancel("c", false)
	for {
		select {
		case d, ok := <-dels:
			if !ok {
				return
			}
			d.Nack(false, true)
		case <-time.After(300 * time.Millisecond):
			return
		}
	}
}

func consume(p int) int {
	proc := 50000 + p
	op := hq.Invoke(proc, "recv", qJobs, "w")
	c, _, err := dial(p + 1)
	if err != nil {
		op.Info()
		return 1
	}
	defer c.Close()
	ch, err := c.Channel()
	if err == nil {
		err = ch.Qos(1, 0, false)
	}
	var dels <-chan amqp.Delivery
	if err == nil {
		dels, err = ch.Consume(qJobs, "c", false, false, false, false, nil)
	}
	if err != nil {
		op.Info()
		return 1
	}
	var d amqp.Delivery
	var ok bool
	select {
	case d, ok = <-dels:
	case <-time.After(3 * time.Second):
		giveUp(ch, dels)
		op.Ok(qJobs, "w")
		return 0
	}
	if !ok {
		op.Info()
		return 1
	}
	id := string(d.Body)
	op.Ok(qJobs, "w", id)
	event("rmq.msg_delivered", "a consumer holds a quorum queue message")
	if strings.HasPrefix(id, "x") {
		// a poison message: the consumer crashes before its acknowledgement, and RabbitMQ
		// returns the message and counts the failed delivery. The crash gives the message back:
		// the exit closes the connection, and the queue returns what the channel held. So the
		// client records a give-back (nack without r=0) before it exits.
		fmt.Println("consume: poison", id)
		hq.Invoke(proc, "nack", qJobs, "w", id).Ok(qJobs, "w", id)
		os.Exit(3)
	}
	time.Sleep(time.Second) // the work
	a := hq.Invoke(proc, "ack", qJobs, "w", id)
	if err := syncAck(ch, d, qJobs); err != nil {
		a.Info()
		appendLine(ackUnk, id)
		return 1
	}
	a.Ok(qJobs, "w", id)
	appendLine(delLog, id)
	return 0
}

func sac(p int) int {
	proc := 60000 + p
	c, _, err := dial(p + 2)
	if err != nil {
		return 1
	}
	defer c.Close()
	ch, err := c.Channel()
	if err == nil {
		err = ch.Qos(1, 0, false)
	}
	var dels <-chan amqp.Delivery
	if err == nil {
		dels, err = ch.Consume(qKeyed, "c", false, false, false, false, nil)
	}
	if err != nil {
		return 1
	}
	end := time.Now().Add(3 * time.Second)
	for time.Now().Before(end) {
		op := ho.Invoke(proc, "recv", qKeyed, "-")
		var d amqp.Delivery
		var ok bool
		select {
		case d, ok = <-dels:
		case <-time.After(time.Until(end)):
			giveUp(ch, dels)
			op.Ok(qKeyed, "-")
			return 0
		}
		if !ok {
			op.Info()
			return 1
		}
		item := string(d.Body) + ";k=" + fmt.Sprint(d.Headers["key"])
		op.Ok(qKeyed, "-", item)
		a := ho.Invoke(proc, "ack", qKeyed, "-", item)
		if err := syncAck(ch, d, qKeyed); err != nil {
			a.Info()
			return 1
		}
		a.Ok(qKeyed, "-", item)
	}
	return 0
}

// ---- streams ----

func senv() (*stream.Environment, error) {
	order := rand.Perm(len(nodes))
	var last error
	// one node at a time: a node that is still starting refuses the stream port, and the
	// client then tries the next one
	for _, i := range order {
		env, err := senvOf(nodes[i])
		if err == nil {
			return env, nil
		}
		last = err
	}
	return nil, last
}

func senvOf(node string) (*stream.Environment, error) {
	return stream.NewEnvironment(stream.NewEnvironmentOptions().
		SetUri("rabbitmq-stream://guest:guest@" + node + ":5552/%2f").
		SetRPCTimeout(5 * time.Second).
		SetRequestedHeartbeat(5 * time.Second))
}

// spub publishes one message with a producer of its own name and publishing id 1. Without a
// confirm it sends the message once more with a new producer of the same name: the server keeps
// the last publishing id per producer name, so the second send must not store a second copy.
func spub(id string) int {
	p := num(id)
	op := hs.Invoke(p, "send", sStream, id)
	sent := false
	for attempt := 0; attempt < 2; attempt++ {
		res, why := spubOnce(id)
		switch res {
		case "ok":
			op.Ok(sStream, id)
			event("rmq.stream_confirmed", "a stream publish was confirmed")
			return 0
		case "info":
			sent = true
		}
		fmt.Println("spub", id, "attempt", attempt, res, why)
	}
	if sent {
		op.Info()
	} else {
		op.Fail()
	}
	return 1
}

func spubOnce(id string) (string, string) {
	env, err := senv()
	if err != nil {
		return "fail", err.Error()
	}
	defer env.Close()
	pr, err := env.NewProducer(sStream, stream.NewProducerOptions().
		SetProducerName("p-"+id).
		SetConfirmationTimeOut(8*time.Second))
	if err != nil {
		return "fail", err.Error()
	}
	defer pr.Close()
	confirms := pr.NotifyPublishConfirmation()
	m := samqp.NewMessage([]byte(id))
	m.SetPublishingId(1)
	if err := pr.Send(m); err != nil {
		return "info", err.Error()
	}
	select {
	case cs, ok := <-confirms:
		if !ok {
			return "info", "confirmations closed"
		}
		for _, c := range cs {
			if c.IsConfirmed() {
				return "ok", ""
			}
			return "info", fmt.Sprintf("not confirmed: code %d %v", c.GetErrorCode(), c.GetError())
		}
		return "info", "empty confirmation"
	case <-time.After(10 * time.Second):
		return "info", "no confirm within 10 s"
	}
}

type smsg struct {
	id  string
	off int64
}

// readStream reads s as consumer name (empty: no name) from spec until max messages or until
// quiet passes without one; it returns the messages in offset order.
func readStream(env *stream.Environment, name string, spec stream.OffsetSpecification, max int, quiet time.Duration) ([]smsg, *stream.Consumer, error) {
	var mu sync.Mutex
	got := []smsg{}
	last := time.Now()
	opts := stream.NewConsumerOptions().SetOffset(spec).SetManualCommit()
	if name != "" {
		opts = opts.SetConsumerName(name)
	}
	co, err := env.NewConsumer(sStream, func(cc stream.ConsumerContext, m *samqp.Message) {
		mu.Lock()
		defer mu.Unlock()
		if len(m.Data) > 0 && !strings.HasPrefix(string(m.Data[0]), "probe") {
			got = append(got, smsg{id: string(m.Data[0]), off: cc.Consumer.GetOffset()})
		}
		last = time.Now()
	}, opts)
	if err != nil {
		return nil, nil, err
	}
	closed := co.NotifyClose()
	for {
		mu.Lock()
		n, l := len(got), last
		mu.Unlock()
		if (max > 0 && n >= max) || time.Since(l) > quiet {
			break
		}
		select {
		case ev := <-closed:
			return nil, co, fmt.Errorf("consumer closed: %v", ev.Reason)
		case <-time.After(100 * time.Millisecond):
		}
	}
	mu.Lock()
	out := append([]smsg(nil), got...)
	mu.Unlock()
	sort.Slice(out, func(i, j int) bool { return out[i].off < out[j].off })
	if max > 0 && len(out) > max {
		out = out[:max]
	}
	return out, co, nil
}

func sitem(m smsg) string { return fmt.Sprintf("%s;o=%d", m.id, m.off) }

// storeOffset stores the offset of r and waits until the server answers it back: the store has
// no answer of its own.
func storeOffset(env *stream.Environment, off int64) error {
	if err := env.StoreOffset(reader, sStream, off); err != nil {
		return err
	}
	for i := 0; i < 30; i++ {
		got, err := env.QueryOffset(reader, sStream)
		if err == nil && got >= off {
			return nil
		}
		time.Sleep(100 * time.Millisecond)
	}
	return errors.New("the stored offset did not come back within 3 s")
}

func readerStart(env *stream.Environment) stream.OffsetSpecification {
	off, err := env.QueryOffset(reader, sStream)
	if err != nil {
		return stream.OffsetSpecification{}.First()
	}
	return stream.OffsetSpecification{}.Offset(off + 1)
}

func sread(p int) int {
	proc := 70000 + p
	op := hs.Invoke(proc, "recv", sStream, reader)
	env, err := senv()
	if err != nil {
		op.Info()
		return 1
	}
	defer env.Close()
	ms, co, err := readStream(env, reader, readerStart(env), 4, 3*time.Second)
	if co != nil {
		defer co.Close()
	}
	if err != nil {
		op.Info()
		fmt.Println("sread:", err)
		return 1
	}
	items := []string{}
	for _, m := range ms {
		items = append(items, sitem(m))
	}
	op.Ok(append([]string{sStream, reader}, items...)...)
	if len(ms) == 0 {
		return 0
	}
	a := hs.Invoke(proc, "ack", append([]string{sStream, reader}, items...)...)
	if err := storeOffset(env, ms[len(ms)-1].off); err != nil {
		a.Info()
		fmt.Println("sread: store:", err)
		return 1
	}
	a.Ok(append([]string{sStream, reader}, items...)...)
	return 0
}

// ---- the drain ----

const (
	drainSecs = 250 // below workload.timeout (330)
	quietJ    = 40  // above consumer_timeout (30 s): a held message comes back before it ends
)

// drainQueue takes every message of a quorum queue, acknowledging each, until quiet seconds
// pass without one and without an error. It counts the quiet window again from each error.
func drainQueue(h *hydohist.History, proc int, queue, group string, quiet float64, item func(amqp.Delivery) string, took func(string)) bool {
	deadline := uptime() + drainSecs
	last := uptime()
	var c *amqp.Connection
	var ch *amqp.Channel
	var dels <-chan amqp.Delivery
	reset := func() {
		if c != nil {
			c.Close()
		}
		c, ch, dels = nil, nil, nil
		last = uptime()
	}
	for uptime() < deadline {
		if c == nil {
			cc, _, err := dial(proc)
			if err == nil {
				ch, err = cc.Channel()
				if err == nil {
					err = ch.Qos(1, 0, false)
				}
				if err == nil {
					dels, err = ch.Consume(queue, "", false, false, false, false, nil)
				}
			}
			if err != nil {
				if cc != nil {
					cc.Close()
				}
				last = uptime()
				time.Sleep(time.Second)
				continue
			}
			c = cc
		}
		op := h.Invoke(proc, "recv", queue, group)
		var d amqp.Delivery
		var ok bool
		select {
		case d, ok = <-dels:
		case <-time.After(2 * time.Second):
			op.Ok(queue, group)
			if uptime()-last >= quiet {
				c.Close()
				h.Record(proc, "ok", "drained", queue, group) // hydohist v0.6.0 has no Drained yet
				return true
			}
			continue
		}
		if !ok {
			op.Info()
			reset()
			continue
		}
		if strings.HasPrefix(string(d.Body), "probe") {
			// an unrecorded message of rmqc probe (eventually_probe runs after the drain, so
			// only a recovery boot can meet one of the first boot's)
			op.Ok(queue, group)
			d.Ack(false)
			last = uptime()
			continue
		}
		it := item(d)
		op.Ok(queue, group, it)
		last = uptime()
		a := h.Invoke(proc, "ack", queue, group, it)
		if err := syncAck(ch, d, queue); err != nil {
			a.Info()
			reset()
			continue
		}
		a.Ok(queue, group, it)
		if took != nil {
			took(string(d.Body))
		}
	}
	if c != nil {
		c.Close()
	}
	fmt.Printf("drain: no quiet window of %.0f s on %s within %d s\n", quiet, queue, drainSecs)
	return false
}

// drainStream reads s for one group to its end: r from its stored offset (storing each read's
// offset), all from the first offset. The stream's end is a quiet window of 10 s.
func drainStream(proc int, group string) bool {
	deadline := uptime() + drainSecs
	if group != reader {
		hs.Invoke(proc, "sub", sStream, group, "from=start").Ok(sStream, group, "from=start")
	}
	var next int64 = -1 // all: the next offset to read
	for uptime() < deadline {
		env, err := senv()
		if err != nil {
			time.Sleep(time.Second)
			continue
		}
		var spec stream.OffsetSpecification
		name := ""
		if group == reader {
			spec, name = readerStart(env), reader
		} else if next < 0 {
			spec = stream.OffsetSpecification{}.First()
		} else {
			spec = stream.OffsetSpecification{}.Offset(next)
		}
		op := hs.Invoke(proc, "recv", sStream, group)
		ms, co, err := readStream(env, name, spec, 0, 10*time.Second)
		if co != nil {
			co.Close()
		}
		if err != nil {
			op.Info()
			env.Close()
			time.Sleep(time.Second)
			continue
		}
		items := []string{}
		for _, m := range ms {
			items = append(items, sitem(m))
		}
		op.Ok(append([]string{sStream, group}, items...)...)
		if group == reader && len(ms) > 0 {
			a := hs.Invoke(proc, "ack", append([]string{sStream, group}, items...)...)
			if err := storeOffset(env, ms[len(ms)-1].off); err != nil {
				a.Info()
				env.Close()
				continue
			}
			a.Ok(append([]string{sStream, group}, items...)...)
		}
		if len(ms) > 0 {
			next = ms[len(ms)-1].off + 1
		}
		env.Close()
		if len(ms) == 0 {
			hs.Record(proc, "ok", "drained", sStream, group)
			return true
		}
	}
	fmt.Printf("drain: the stream group %s did not end within %d s\n", group, drainSecs)
	return false
}

func drain() int {
	// The groups exist before the first send: provision declares the queues, and the reader r
	// starts at the stream's first offset.
	probeRecorded()
	hq.Invoke(100000, "sub", qJobs, "w", "from=start").Ok(qJobs, "w", "from=start")
	hq.Invoke(100001, "sub", qDead, "-", "from=start").Ok(qDead, "-", "from=start")
	ho.Invoke(100002, "sub", qKeyed, "-", "from=start").Ok(qKeyed, "-", "from=start")
	hs.Invoke(100003, "sub", sStream, reader, "from=start").Ok(sStream, reader, "from=start")
	var wg sync.WaitGroup
	res := make([]bool, 5)
	plain := func(d amqp.Delivery) string { return string(d.Body) }
	keyed := func(d amqp.Delivery) string { return string(d.Body) + ";k=" + fmt.Sprint(d.Headers["key"]) }
	took := func(id string) { appendLine(delLog, id) }
	run := func(i int, f func() bool) {
		wg.Add(1)
		go func() { defer wg.Done(); res[i] = f() }()
	}
	run(0, func() bool { return drainQueue(hq, 100000, qJobs, "w", quietJ, plain, took) })
	run(1, func() bool { return drainQueue(hq, 100001, qDead, "-", quietJ, plain, took) })
	run(2, func() bool { return drainQueue(ho, 100002, qKeyed, "-", quietJ, keyed, nil) })
	run(3, func() bool { return drainStream(100003, reader) })
	run(4, func() bool { return drainStream(100004, "all") })
	wg.Wait()
	for _, ok := range res {
		if !ok {
			return 1
		}
	}
	readBack()
	return 0
}

// readBack writes one status record per id the workload published to j: s=ok and f=1 when a
// consumer acknowledged the message or the drain took it from the dead-letter queue, s=info and
// f=0 when none did. An id whose acknowledgement got no answer and that nobody took later is not
// read back: RabbitMQ may have taken that acknowledgement, so its final state is unknown.
func readBack() {
	delivered := lines(delLog)
	unknown := lines(ackUnk)
	ids := []string{}
	seen := map[string]bool{}
	if b, err := os.ReadFile(os.Getenv("HYDO_OUTCOMES")); err == nil {
		for _, l := range strings.Split(string(b), "\n") {
			f := strings.Fields(l)
			if len(f) == 2 && !seen[f[0]] {
				seen[f[0]] = true
				if unknown[f[0]] && !delivered[f[0]] {
					continue
				}
				ids = append(ids, f[0])
			}
		}
	}
	const proc = 100010
	for i := 0; i < len(ids); i += 20 {
		j := i + 20
		if j > len(ids) {
			j = len(ids)
		}
		inv := []string{qJobs}
		args := []string{qJobs}
		for _, id := range ids[i:j] {
			inv = append(inv, id)
			if delivered[id] {
				args = append(args, id+";s=ok;f=1")
			} else {
				args = append(args, id+";s=info;f=0")
			}
		}
		hq.Invoke(proc, "status", inv...).Ok(args...)
	}
}

// probe (eventually_probe, after the drain): a publish to j and one to s are confirmed again
// after the faults. These messages are not recorded; the drain leaves out bodies "probe...".
func probe() int {
	end := uptime() + 60
	for uptime() < end {
		id := fmt.Sprintf("probe%d", int64(uptime()*1000))
		res, why := publish(0, qJobs, id, nil)
		if res == "ok" {
			res, why = spubOnce(id)
			if res == "ok" {
				return 0
			}
		}
		fmt.Println("probe:", res, why)
		time.Sleep(time.Second)
	}
	fmt.Println("probe: no publish was confirmed within 60 s after the faults")
	return 1
}

// probeRecorded, at the start of the drain: after the faults (and in the recovery boot after a
// power cut), one publish to the quorum queue j and one to the stream s are confirmed again.
// Both are recorded, so the drain must take them: a stream that lost its tail in a power cut
// must not hand a new message an offset that the reader r has already stored. The ids carry the
// guest's uptime, so each boot's are its own.
func probeRecorded() int {
	end := uptime() + 60
	tag := fmt.Sprintf("%d", int64(uptime()*1000))
	jid, sid := "mp"+tag, "sp"+tag
	jdone, sdone, jsent, ssent := false, false, false, false
	jop := hq.Invoke(90000, "send", qJobs, jid)
	sop := hs.Invoke(90001, "send", sStream, sid)
	for uptime() < end && !(jdone && sdone) {
		if !jdone {
			res, why := publish(0, qJobs, jid, nil)
			if res == "ok" {
				jop.Ok(qJobs, jid)
				outcome(jid, "ack")
				jdone = true
			} else {
				jsent = jsent || res == "info"
				fmt.Println("probe: j:", res, why)
			}
		}
		if !sdone {
			res, why := spubOnce(sid)
			if res == "ok" {
				sop.Ok(sStream, sid)
				sdone = true
			} else {
				ssent = ssent || res == "info"
				fmt.Println("probe: s:", res, why)
			}
		}
		if !(jdone && sdone) {
			time.Sleep(time.Second)
		}
	}
	// A retry of an unconfirmed publish can store the message twice in j (no deduplication
	// there), so an unconfirmed j probe counts as unknown. The stream's retries use one producer
	// name and publishing id, so they store it once.
	if !jdone {
		if jsent {
			jop.Info()
			outcome(jid, "unknown")
		} else {
			jop.Fail()
		}
	}
	if !sdone {
		if ssent {
			sop.Info()
		} else {
			sop.Fail()
		}
	}
	if jdone && sdone {
		return 0
	}
	fmt.Println("probe: a publish was not confirmed within 60 s after the faults")
	return 1
}

func leader(queue string) int {
	env, err := senv()
	if err != nil {
		return 1
	}
	defer env.Close()
	md, err := env.StreamMetaData(queue)
	if err != nil || md.Leader == nil {
		return 1
	}
	fmt.Println(md.Leader.Host)
	return 0
}
