// rmq: the test client of the RocketMQ example, for hydo's workload. It records every operation
// as a HYDO-HIST record and prints events for reactions. Two histories, one per check:
//
//	q  the topic j: plain, delayed and transactional messages, consumed by the group w with
//	   offsets on the broker, poison messages retried and dead-lettered (channel dlq)
//	o  the topic o: messages ordered by key, one queue per key, consumed by the group og
//
// Commands:
//
//	rmq provision        wait for a master, create the topics j, o, %RETRY%w and %DLQ%w
//	rmq pub <id>         send one message to j with SendSync (m<n> plain, x<n> poison, d<n>
//	                     delayed by 5 s)
//	rmq tx <id>          send one transactional message to j: t<n> commits, r<n> rolls back, u<n>
//	                     answers "unknown" and commits at the broker's check
//	rmq opub <n>         send one message to o, to the queue of its key (n mod 4)
//	rmq consume <proc>   pull up to 4 messages of one queue of j (or of %RETRY%w) from w's
//	                     offset, give a poison message back for a retry, commit the offset
//	rmq oconsume <proc>  pull up to 4 messages of one queue of o from og's offset, commit it
//	rmq drain            answer the transaction checks and take every message left, then end
//	                     each group with a drained record
//	rmq leader           print the service that is broker-a's master
//	rmq ready            exit 0 when the NameServer names a master
package main

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"syscall"
	"time"

	rocketmq "github.com/apache/rocketmq-client-go/v2"
	"github.com/apache/rocketmq-client-go/v2/primitive"
	"github.com/apache/rocketmq-client-go/v2/producer"
	"github.com/apache/rocketmq-client-go/v2/rlog"
	"github.com/kakoc/hydo/sdk/go/hydohist"
)

const (
	topicJ     = "j"
	topicO     = "o"
	groupW     = "w"
	groupO     = "og"
	groupDLQ   = "dlqread"
	retryW     = "%RETRY%w"
	dlqW       = "%DLQ%w"
	queues     = 4
	maxRetries = 2 // a poison message is delivered 3 times, then dead-lettered
	decisions  = "/data/client/tx-decisions"
)

var (
	hq = hydohist.New("q")
	ho = hydohist.New("o")
)

var services = map[string]string{"10.89.0.11": "broker0", "10.89.0.12": "broker1", "10.89.0.13": "broker2"}

func main() {
	rlog.SetLogLevel("fatal")
	os.Setenv("ROCKETMQ_GO_LOG_LEVEL", "fatal")
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: rmq provision | pub <id> | tx <id> | opub <n> | consume <proc> | oconsume <proc> | drain | leader | ready")
		os.Exit(2)
	}
	arg := func() string {
		if len(os.Args) < 3 {
			fmt.Fprintln(os.Stderr, "rmq: missing argument")
			os.Exit(2)
		}
		return os.Args[2]
	}
	switch os.Args[1] {
	case "provision":
		os.Exit(provision())
	case "pub":
		os.Exit(pub(arg()))
	case "tx":
		os.Exit(tx(arg()))
	case "opub":
		os.Exit(opub(num(arg())))
	case "consume":
		os.Exit(consume(num(arg())))
	case "oconsume":
		os.Exit(oconsume(num(arg())))
	case "drain":
		os.Exit(drain())
	case "leader":
		os.Exit(leader())
	case "ready":
		if _, err := master(); err != nil {
			os.Exit(1)
		}
		os.Exit(0)
	}
	os.Exit(2)
}

func num(s string) int {
	n, _ := strconv.Atoi(strings.TrimLeft(s, "mxdturo"))
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

func say(format string, a ...interface{}) {
	fmt.Fprintf(os.Stderr, "rmq: "+format+"\n", a...)
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

// lock takes an exclusive lock of one queue for one group, so that two commands never pull and
// commit one queue at once (RocketMQ gives each queue to one consumer of a group).
func lock(name string) func() {
	f, err := os.OpenFile("/data/client/lock."+strings.ReplaceAll(name, "%", "_"), os.O_CREATE|os.O_RDWR, 0644)
	if err != nil {
		return func() {}
	}
	syscall.Flock(int(f.Fd()), syscall.LOCK_EX)
	return func() { syscall.Flock(int(f.Fd()), syscall.LOCK_UN); f.Close() }
}

func newProducer(group string) (rocketmq.Producer, error) {
	p, err := rocketmq.NewProducer(
		producer.WithNsResolver(primitive.NewPassthroughResolver([]string{namesrvAddr})),
		producer.WithGroupName(group),
		producer.WithRetry(0), // one attempt: an error is an unknown outcome, not a second copy
		producer.WithSendMsgTimeout(10*time.Second),
		producer.WithQueueSelector(producer.NewManualQueueSelector()),
	)
	if err != nil {
		return nil, err
	}
	return p, p.Start()
}

// sysID is a stored copy's system id: the queue and its offset in one number.
func sysID(q int, off int64) string { return strconv.FormatInt(int64(q)*1000000+off, 10) }

func provision() int {
	deadline := uptime() + 240
	for {
		addrs, err := brokers()
		if err == nil && addrs[0] != "" && len(addrs) == 3 {
			ok := true
			for _, a := range addrs {
				for _, t := range []struct {
					name string
					q    int
					perm int
				}{{topicJ, queues, 6}, {topicO, queues, 6}, {retryW, 1, 6}, {dlqW, 1, 6}} {
					if err := createTopic(a, t.name, t.q, t.perm); err != nil {
						say("provision: %s on %s: %v", t.name, a, err)
						if a == addrs[0] {
							ok = false
						}
					}
				}
			}
			if ok {
				// the producers find j and o through the NameServer's routes
				p, err := newProducer("provision")
				if err == nil {
					for _, t := range []string{topicJ, topicO} {
						for uptime() < deadline {
							if _, err := p.SendSync(context.Background(), withQueue(primitive.NewMessage(t, []byte("p")), 0)); err == nil {
								break
							} else {
								say("provision: send to %s: %v", t, err)
							}
							time.Sleep(time.Second)
						}
					}
					p.Shutdown()
				}
				// the provisioning messages are consumed now, so that the workload starts after them
				for _, t := range []string{topicJ, topicO} {
					g := groupW
					if t == topicO {
						g = groupO
					}
					for q := 0; q < queues; q++ {
						r, err := pull(addrs[0], g, t, q, 0, 32)
						if err == nil && r.code == resSuccess {
							updateOffset(addrs[0], g, t, q, r.next)
						}
					}
				}
				say("provision: master %s, brokers %v", addrs[0], addrs)
				return 0
			}
		} else {
			say("provision: brokers %v %v", addrs, err)
		}
		if uptime() > deadline {
			say("provision: no master within 240 s")
			return 1
		}
		time.Sleep(2 * time.Second)
	}
}

func withQueue(m *primitive.Message, q int) *primitive.Message {
	m.Queue = &primitive.MessageQueue{Topic: m.Topic, BrokerName: brokerName, QueueId: q}
	return m
}

// pub sends one message to j: the queue is the id's number mod 4.
func pub(id string) int {
	n := num(id)
	item := id
	m := withQueue(primitive.NewMessage(topicJ, []byte(id)), n%queues)
	m.WithKeys([]string{id})
	if strings.HasPrefix(id, "d") {
		m.WithDelayTimeLevel(2) // 5 s
		item = id + ";d=5000"
	}
	p, err := newProducer("pg")
	if err != nil {
		say("pub %s: %v", id, err)
		return 1
	}
	defer p.Shutdown()
	op := hq.Invoke(n, "send", topicJ, item)
	r, err := p.SendSync(context.Background(), m)
	if err != nil {
		say("pub %s: %v", id, err)
		op.Info()
		return 0
	}
	if r.Status != primitive.SendOK {
		say("pub %s: status %d", id, r.Status)
		op.Info()
		return 0
	}
	op.Ok(item + ";o=" + sysID(r.MessageQueue.QueueId, r.QueueOffset))
	event("rmq.pub_acked", id)
	return 0
}

type txListener struct {
	local primitive.LocalTransactionState
}

func (l *txListener) ExecuteLocalTransaction(*primitive.Message) primitive.LocalTransactionState {
	return l.local
}

// CheckLocalTransaction answers the broker's check from the decisions file.
func (l *txListener) CheckLocalTransaction(m *primitive.MessageExt) primitive.LocalTransactionState {
	id := string(m.Body)
	state := primitive.UnknowState
	if b, err := os.ReadFile(decisions); err == nil {
		for _, line := range strings.Split(string(b), "\n") {
			f := strings.Fields(line)
			if len(f) == 2 && f[0] == id {
				if f[1] == "commit" {
					state = primitive.CommitMessageState
				} else {
					state = primitive.RollbackMessageState
				}
			}
		}
	} else {
		state = primitive.RollbackMessageState
	}
	note("check of %s: %v", id, state)
	return state
}

func newTxProducer(l *txListener) (rocketmq.TransactionProducer, error) {
	p, err := rocketmq.NewTransactionProducer(l,
		producer.WithNsResolver(primitive.NewPassthroughResolver([]string{namesrvAddr})),
		producer.WithGroupName("txg"),
		producer.WithRetry(0),
		producer.WithSendMsgTimeout(10*time.Second),
		producer.WithQueueSelector(producer.NewManualQueueSelector()),
	)
	if err != nil {
		return nil, err
	}
	return p, p.Start()
}

// tx sends one transactional message to j. The decision is written (and synced) before the half
// message is sent, so that the broker's check finds it whatever happens to the sender.
func tx(id string) int {
	n := num(id)
	var local primitive.LocalTransactionState
	switch id[0] {
	case 't':
		local = primitive.CommitMessageState
		appendLine(decisions, id+" commit")
	case 'r':
		local = primitive.RollbackMessageState
		appendLine(decisions, id+" rollback")
	default:
		local = primitive.UnknowState
		appendLine(decisions, id+" commit")
	}
	p, err := newTxProducer(&txListener{local: local})
	if err != nil {
		say("tx %s: %v", id, err)
		return 1
	}
	defer p.Shutdown()
	m := withQueue(primitive.NewMessage(topicJ, []byte(id)), n%queues)
	m.WithKeys([]string{id})
	op := hq.Invoke(n, "send", topicJ, id)
	r, err := p.SendMessageInTransaction(context.Background(), m)
	switch {
	case id[0] == 'r':
		op.Fail() // rolled back: the message never reaches a consumer
	case err != nil:
		say("tx %s: %v", id, err)
		op.Info()
	case r.Status != primitive.SendOK:
		say("tx %s: status %d", id, r.Status)
		op.Info()
	default:
		op.Ok(id)
		event("rmq.tx_sent", id)
	}
	// stay a moment, so that a check of the broker finds a producer of the group
	time.Sleep(2 * time.Second)
	return 0
}

// opub sends one message to o, to the queue of its key (n mod 4).
func opub(n int) int {
	id := "o" + strconv.Itoa(n)
	q := n % queues
	key := "k" + strconv.Itoa(q)
	item := id + ";k=" + key
	p, err := newProducer("opg")
	if err != nil {
		say("opub %s: %v", id, err)
		return 1
	}
	defer p.Shutdown()
	m := withQueue(primitive.NewMessage(topicO, []byte(id)), q)
	m.WithKeys([]string{id})
	op := ho.Invoke(n, "send", topicO, item)
	r, err := p.SendSync(context.Background(), m)
	if err != nil || r.Status != primitive.SendOK {
		say("opub %s: %v %v", id, err, r)
		op.Info()
		return 0
	}
	op.Ok(item + ";o=" + sysID(q, r.QueueOffset))
	event("rmq.opub_acked", id)
	return 0
}

type msgExt struct {
	topic           string
	queueID         int
	queueOffset     int64
	commitLogOffset int64
	reconsume       int
	msgID           string
	body            string
	props           map[string]string
}

func decode(body []byte) []*msgExt {
	var out []*msgExt
	for _, m := range primitive.DecodeMessage(body) {
		out = append(out, &msgExt{
			topic: m.Topic, queueID: m.Queue.QueueId, queueOffset: m.QueueOffset,
			commitLogOffset: m.CommitLogOffset, reconsume: int(m.ReconsumeTimes),
			msgID: m.MsgId, body: string(m.Body), props: m.GetProperties(),
		})
	}
	return out
}

// item is the record of one delivered message.
func item(m *msgExt, withKey bool) string {
	s := hydohist.Enc(m.body)
	if withKey {
		s += ";k=k" + strconv.Itoa(m.queueID)
	}
	if !strings.HasPrefix(m.topic, "%") {
		s += ";o=" + sysID(m.queueID, m.queueOffset)
	}
	return s + ";n=" + strconv.Itoa(m.reconsume+1)
}

// quietRecv: the drain records a receive only when it got messages, and then writes its
// invocation after the pull. The drain is the only process left, so no record of another process
// falls between, and hvx's 64 KiB of console per boot keeps room for the workload's records.
var quietRecv bool

type recvOp struct {
	h              *hydohist.History
	proc           int
	channel, group string
}

func recvInvoke(h *hydohist.History, proc int, channel, group string) *recvOp {
	r := &recvOp{h, proc, channel, group}
	if !quietRecv {
		h.Record(proc, "invoke", "recv", channel, group)
	}
	return r
}

func (r *recvOp) info() {
	if !quietRecv {
		r.h.Record(r.proc, "info", "recv", r.channel, r.group)
	}
}

func (r *recvOp) ok(items ...string) {
	if quietRecv {
		if len(items) == 0 {
			return
		}
		r.h.Record(r.proc, "invoke", "recv", r.channel, r.group)
	}
	r.h.Record(r.proc, "ok", "recv", append([]string{r.channel, r.group}, items...)...)
}

// note writes one line to the console too: hvx keeps a command's own output in the guest.
func note(format string, a ...interface{}) {
	say(format, a...)
	if f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0); err == nil {
		fmt.Fprintf(f, "rmq: "+format+"\n", a...)
		f.Close()
	}
}

// one pulls up to 4 messages of one queue from the group's offset, records them, hands poison
// messages back for a retry, and commits the offset. It returns how many messages it got.
func one(h *hydohist.History, proc int, channel, group, topic string, q int, ordered bool) (int, error) {
	defer lock(group + "." + topic + "." + strconv.Itoa(q))()
	addr, err := master()
	if err != nil {
		return 0, err
	}
	off, found, err := queryOffset(addr, group, topic, q)
	if err != nil {
		return 0, err
	}
	if !found {
		off = 0
	}
	rec := recvInvoke(h, proc, channel, group)
	p, err := pull(addr, group, topic, q, off, 4)
	if err != nil {
		rec.info()
		return 0, err
	}
	if p.code == resPullMoved {
		// as RocketMQ's own consumers do: take the broker's corrected offset
		note("pull %s:%d of %s at %d: offset moved to %d (min %d max %d)", topic, q, group, off, p.next, p.min, p.max)
		rec.ok()
		return 0, updateOffset(addr, group, topic, q, p.next)
	}
	items := []string{}
	for _, m := range p.msgs {
		items = append(items, item(m, ordered))
	}
	rec.ok(items...)
	if len(items) == 0 {
		return 0, nil
	}
	var acks, nacks []string
	for i, m := range p.msgs {
		if strings.HasPrefix(m.body, "x") {
			op := h.Invoke(proc, "nack", channel, group, items[i])
			if err := sendBack(addr, group, m, 1, maxRetries); err != nil {
				say("send back %s: %v", m.body, err)
				op.Info()
				// the offset is not committed past a message that went nowhere
				if i == 0 {
					return len(p.msgs), err
				}
				p.next = p.msgs[i].queueOffset
				break
			}
			op.Ok()
			nacks = append(nacks, items[i])
			continue
		}
		acks = append(acks, items[i])
	}
	_ = nacks
	if len(acks) == 0 {
		return len(p.msgs), updateOffset(addr, group, topic, q, p.next)
	}
	ack := h.Invoke(proc, "ack", append([]string{channel, group}, acks...)...)
	if err := updateOffset(addr, group, topic, q, p.next); err != nil {
		ack.Info()
		return len(p.msgs), err
	}
	ack.Ok()
	return len(p.msgs), nil
}

func consume(proc int) int {
	hq.Record(proc, "invoke", "sub", topicJ, groupW, "from=start")
	hq.Record(proc, "ok", "sub", topicJ, groupW, "from=start")
	topic, q := topicJ, proc%5
	if q == 4 {
		topic, q = retryW, 0
	}
	if _, err := one(hq, proc, topicJ, groupW, topic, q, false); err != nil {
		say("consume %s:%d: %v", topic, q, err)
	}
	return 0
}

func oconsume(proc int) int {
	ho.Record(proc, "invoke", "sub", topicO, groupO, "from=start")
	ho.Record(proc, "ok", "sub", topicO, groupO, "from=start")
	if _, err := one(ho, proc, topicO, groupO, topicO, proc%queues, true); err != nil {
		say("oconsume %d: %v", proc%queues, err)
	}
	event("rmq.oconsumed", strconv.Itoa(proc))
	return 0
}

// drain answers the broker's transaction checks and takes every message left, until a quiet
// window of 45 s without a message or an error, within 250 s (workload.timeout is 300 s).
func drain() int {
	const proc = 100000
	quietRecv = true
	tp, err := newTxProducer(&txListener{local: primitive.UnknowState})
	if err != nil {
		note("drain: transaction producer: %v", err)
	} else {
		defer tp.Shutdown()
		// The producer sends a heartbeat to the brokers it knows 1 s after its start, and then
		// every 30 s; a broker sends its checks only to producers whose heartbeat it has. A
		// rolled-back message to the topic tk makes it learn broker-a's addresses at once.
		appendLine(decisions, "keep rollback")
		k := withQueue(primitive.NewMessage("tk", []byte("keep")), 0)
		if _, err := tp.SendMessageInTransaction(context.Background(), k); err != nil {
			note("drain: route of the transaction producer: %v", err)
		}
	}
	deadline := uptime() + 250
	last := uptime()
	for uptime() < deadline {
		got := 0
		failed := false
		for q := 0; q < queues; q++ {
			for {
				n, err := one(hq, proc, topicJ, groupW, topicJ, q, false)
				if err != nil {
					say("drain j:%d: %v", q, err)
					failed = true
				}
				got += n
				if n == 0 || err != nil {
					break
				}
			}
			for {
				n, err := one(ho, proc, topicO, groupO, topicO, q, true)
				if err != nil {
					say("drain o:%d: %v", q, err)
					failed = true
				}
				got += n
				if n == 0 || err != nil {
					break
				}
			}
		}
		for {
			n, err := one(hq, proc, topicJ, groupW, retryW, 0, false)
			if err != nil {
				say("drain %s: %v", retryW, err)
				failed = true
			}
			got += n
			if n == 0 || err != nil {
				break
			}
		}
		for {
			n, err := dlq(proc)
			if err != nil {
				say("drain %s: %v", dlqW, err)
				failed = true
			}
			got += n
			if n == 0 || err != nil {
				break
			}
		}
		if got > 0 || failed {
			last = uptime()
		}
		if uptime()-last >= 45 {
			note("drain: quiet for 45 s")
			hq.Record(proc, "ok", "drained", topicJ, groupW)
			hq.Record(proc, "ok", "drained", "dlq", "-")
			ho.Record(proc, "ok", "drained", topicO, groupO)
			return 0
		}
		time.Sleep(time.Second)
	}
	fmt.Println("rmq drain: no quiet window of 45 s within 250 s")
	return 1
}

// dlq takes the dead-lettered messages of w, as the channel dlq of the group -.
func dlq(proc int) (int, error) {
	defer lock(groupDLQ)()
	addr, err := master()
	if err != nil {
		return 0, err
	}
	off, _, err := queryOffset(addr, groupDLQ, dlqW, 0)
	if err != nil {
		return 0, err
	}
	rec := recvInvoke(hq, proc, "dlq", "-")
	p, err := pull(addr, groupDLQ, dlqW, 0, off, 8)
	if err != nil {
		rec.info()
		return 0, err
	}
	items := []string{}
	for _, m := range p.msgs {
		items = append(items, hydohist.Enc(m.body))
	}
	rec.ok(items...)
	if len(items) == 0 {
		return 0, nil
	}
	ack := hq.Invoke(proc, "ack", append([]string{"dlq", "-"}, items...)...)
	if err := updateOffset(addr, groupDLQ, dlqW, 0, p.next); err != nil {
		ack.Info()
		return len(items), err
	}
	ack.Ok()
	return len(items), nil
}

func leader() int {
	addr, err := master()
	if err != nil {
		say("leader: %v", err)
		return 1
	}
	host := strings.Split(addr, ":")[0]
	if s, ok := services[host]; ok {
		fmt.Println(s)
		return 0
	}
	say("leader: unknown address %s", addr)
	return 1
}
