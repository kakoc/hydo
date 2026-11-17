// rpclient: the test client of the Redpanda example. One static program with one subcommand per
// client operation. Each operation writes HYDO-HIST records of the history `rp` (hydo guide
// messages): one record before the request and one after the reply.
//
//	rpclient create                  create the topic t: 3 partitions, 3 replicas
//	rpclient produce <id> <seed>     one record with the idempotent producer, acks=all
//	rpclient txn <id> <seed>         two records in one transaction, committed or aborted
//	rpclient consume <id> <seed>     a member of group g: poll, then commit the offsets
//	rpclient drain                   the final read: every partition from the start, then group g
//	rpclient offsets                 print group g's committed offsets
//	rpclient lso                     exit 1 while a transaction is open on a partition of t
//	rpclient dump <partition> <from> one read_committed fetch, raw: aborted transactions and batches
//
// Every choice comes from the seed hydo gives. The program reads the guest clock only through
// /proc/uptime for its deadlines, which a clock step (T=) does not move.
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/twmb/franz-go/pkg/kerr"
	"github.com/twmb/franz-go/pkg/kgo"
	"github.com/twmb/franz-go/pkg/kmsg"
	"github.com/twmb/franz-go/pkg/kversion"
)

const (
	topic      = "t"
	partitions = 3
	group      = "g"
	hist       = "rp"
)

var brokers = []string{"n1:9092", "n2:9092", "n3:9092"}

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
	s := strings.Replace(f[0], ".", "", 1)
	return s + "0000000"
}

// rec writes one record: HYDO-HIST rp <process> <type> <time> <f> <args>
func rec(proc int64, typ string, args ...string) {
	if histOut == nil {
		return
	}
	line := fmt.Sprintf("HYDO-HIST %s %d %s %s %s\n", hist, proc, typ, uptimeNs(), strings.Join(args, " "))
	histOut.WriteString(line) // one write: one record
}

func item(id string, part int32, off int64) string {
	if off < 0 {
		return fmt.Sprintf("%s;k=p%d", id, part)
	}
	return fmt.Sprintf("%s;k=p%d;o=%d", id, part, off)
}

// items of a fetch, at most 40 per record (the console takes short lines)
func recvRecords(proc int64, grp string, recs []*kgo.Record) {
	for len(recs) > 0 {
		n := len(recs)
		if n > 40 {
			n = 40
		}
		args := []string{"recv", topic, grp}
		for _, r := range recs[:n] {
			args = append(args, item(string(r.Value), r.Partition, r.Offset))
		}
		rec(proc, "ok", args...)
		recs = recs[n:]
		if len(recs) > 0 {
			rec(proc, "invoke", "recv", topic, grp)
		}
	}
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

func client(opts ...kgo.Opt) *kgo.Client {
	base := []kgo.Opt{
		kgo.SeedBrokers(brokers...),
		kgo.DialTimeout(2 * time.Second),
		kgo.RequestTimeoutOverhead(3 * time.Second),
		kgo.RetryTimeout(8 * time.Second),
		kgo.MetadataMinAge(250 * time.Millisecond),
	}
	cl, err := kgo.NewClient(append(base, opts...)...)
	if err != nil {
		fmt.Fprintln(os.Stderr, "client:", err)
		os.Exit(2)
	}
	return cl
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

// ---- create ----

func create() {
	cl := client()
	defer cl.Close()
	end := uptime() + 100
	for uptime() < end {
		ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
		req := kmsg.NewPtrCreateTopicsRequest()
		t := kmsg.NewCreateTopicsRequestTopic()
		t.Topic = topic
		t.NumPartitions = partitions
		t.ReplicationFactor = 3
		req.Topics = append(req.Topics, t)
		req.TimeoutMillis = 7000
		resp, err := req.RequestWith(ctx, cl)
		cancel()
		if err == nil && len(resp.Topics) == 1 {
			e := kerr.ErrorForCode(resp.Topics[0].ErrorCode)
			if e == nil || errors.Is(e, kerr.TopicAlreadyExists) {
				if leaders(cl) == partitions {
					fmt.Println("topic t ready: 3 partitions with leaders")
					return
				}
			} else {
				fmt.Fprintln(os.Stderr, "create:", e)
			}
		} else if err != nil {
			fmt.Fprintln(os.Stderr, "create:", err)
		}
		time.Sleep(time.Second)
	}
	die(1, "create: topic t has no leaders after 100 s")
}

func leaders(cl *kgo.Client) int {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	req := kmsg.NewPtrMetadataRequest()
	rt := kmsg.NewMetadataRequestTopic()
	tn := topic
	rt.Topic = &tn
	req.Topics = append(req.Topics, rt)
	resp, err := req.RequestWith(ctx, cl)
	if err != nil || len(resp.Topics) != 1 {
		return 0
	}
	n := 0
	for _, p := range resp.Topics[0].Partitions {
		if p.ErrorCode == 0 && p.Leader >= 0 && len(p.ISR) == 3 {
			n++
		}
	}
	return n
}

// ---- produce ----

func produce(id, seed int64) {
	part := int32(seed % partitions)
	cl := client(
		kgo.DefaultProduceTopic(topic),
		kgo.RecordPartitioner(kgo.ManualPartitioner()),
		kgo.RequiredAcks(kgo.AllISRAcks()), // the default; stated for the reader
		kgo.RecordDeliveryTimeout(8*time.Second),
		kgo.ProducerLinger(0),
	)
	defer cl.Close()
	m := fmt.Sprintf("m%d", id)
	rec(id, "invoke", "send", topic, item(m, part, -1))
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	r := &kgo.Record{Value: []byte(m), Partition: part}
	res := cl.ProduceSync(ctx, r)
	if err := res.FirstErr(); err != nil {
		// idempotent retries hide what reached the leader: unknown
		rec(id, "info", "send", topic, item(m, part, -1))
		die(1, "produce %s: %v", m, err)
	}
	rec(id, "ok", "send", topic, item(m, r.Partition, r.Offset))
	fmt.Printf("produced %s p%d@%d\n", m, r.Partition, r.Offset)
}

// ---- txn ----

func txn(id, seed int64) {
	// two partitions, a transactional id from a pool of 4, commit or abort: all from the seed
	p1 := int32(seed % partitions)
	p2 := int32((seed/3)%2+1+int64(p1)) % partitions
	commit := (seed/7)%4 != 0 // 3 in 4 commit
	txid := fmt.Sprintf("tx-%d", (seed/29)%4)
	cl := client(
		kgo.DefaultProduceTopic(topic),
		kgo.RecordPartitioner(kgo.ManualPartitioner()),
		kgo.TransactionalID(txid),
		kgo.TransactionTimeout(10*time.Second),
		kgo.RecordDeliveryTimeout(8*time.Second),
		kgo.ProducerLinger(0),
	)
	defer cl.Close()
	a, b := fmt.Sprintf("m%da", id), fmt.Sprintf("m%db", id)
	rec(id, "invoke", "send", topic, item(a, p1, -1), item(b, p2, -1))
	unknown := func(why string, err error) {
		rec(id, "info", "send", topic, item(a, p1, -1)+";s=info", item(b, p2, -1)+";s=info")
		die(1, "txn %s %s (%s): %s: %v", a, b, txid, why, err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	if err := cl.BeginTransaction(); err != nil {
		unknown("begin", err)
	}
	ra := &kgo.Record{Value: []byte(a), Partition: p1}
	rb := &kgo.Record{Value: []byte(b), Partition: p2}
	// an event for reactions (A=rp.txn_add...): the first produce of a transaction makes the
	// client add the partitions to it at the coordinator, which starts it on each partition
	if histOut != nil {
		histOut.WriteString(`HYDO-ASSERT {"t":"sometimes","id":"rp.txn_add","ok":true,"msg":"a transaction adds its partitions"}` + "\n")
	}
	perr := cl.ProduceSync(ctx, ra, rb).FirstErr()
	if perr != nil {
		commit = false
	}
	how := kgo.TryAbort
	if commit {
		how = kgo.TryCommit
	}
	if err := cl.EndTransaction(ctx, how); err != nil {
		unknown("end", err)
	}
	if commit {
		rec(id, "ok", "send", topic, item(a, ra.Partition, ra.Offset)+";s=ok", item(b, rb.Partition, rb.Offset)+";s=ok")
		fmt.Printf("committed %s p%d@%d %s p%d@%d (%s)\n", a, ra.Partition, ra.Offset, b, rb.Partition, rb.Offset, txid)
		return
	}
	// the abort completed: neither record may ever be read with read_committed
	rec(id, "fail", "send", topic, item(a, p1, -1)+";s=fail", item(b, p2, -1)+";s=fail")
	if perr != nil {
		fmt.Printf("aborted %s %s (%s) after a produce error: %v\n", a, b, txid, perr)
	} else {
		fmt.Printf("aborted %s %s (%s)\n", a, b, txid)
	}
}

// ---- consume ----

func commitOffsets(cl *kgo.Client, proc int64, grp string, last map[int32]int64) error {
	if len(last) == 0 {
		return nil
	}
	var keys []int32
	for p := range last {
		keys = append(keys, p)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i] < keys[j] })
	args := []string{"commit", topic, grp}
	offs := map[int32]kgo.EpochOffset{}
	for _, p := range keys {
		args = append(args, fmt.Sprintf("p%d@%d", p, last[p]))
		offs[p] = kgo.EpochOffset{Epoch: -1, Offset: last[p] + 1}
	}
	rec(proc, "invoke", args...)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	var cerr error
	done := make(chan struct{})
	cl.CommitOffsetsSync(ctx, map[string]map[int32]kgo.EpochOffset{topic: offs},
		func(_ *kgo.Client, _ *kmsg.OffsetCommitRequest, resp *kmsg.OffsetCommitResponse, err error) {
			defer close(done)
			if err != nil {
				cerr = err
				return
			}
			for _, t := range resp.Topics {
				for _, p := range t.Partitions {
					if e := kerr.ErrorForCode(p.ErrorCode); e != nil && cerr == nil {
						cerr = e
					}
				}
			}
		})
	<-done
	if cerr != nil {
		rec(proc, "info", args...)
		return cerr
	}
	rec(proc, "ok", args...)
	return nil
}

func groupClient(extra ...kgo.Opt) *kgo.Client {
	return client(append([]kgo.Opt{
		kgo.ConsumerGroup(group),
		kgo.ConsumeTopics(topic),
		kgo.FetchIsolationLevel(kgo.ReadCommitted()),
		kgo.ConsumeResetOffset(kgo.NewOffset().AtStart()),
		kgo.DisableAutoCommit(),
		kgo.SessionTimeout(10 * time.Second),
		kgo.RebalanceTimeout(10 * time.Second),
		kgo.FetchMaxWait(500 * time.Millisecond),
	}, extra...)...)
}

func consume(id, seed int64) {
	max := int(10 + seed%40)
	cl := groupClient()
	defer cl.Close() // leaves the group
	rec(id, "invoke", "sub", topic, group, "from=start")
	rec(id, "ok", "sub", topic, group, "from=start")
	last := map[int32]int64{}
	got, empty := 0, 0
	end := uptime() + 6
	for got < max && uptime() < end {
		ctx, cancel := context.WithTimeout(context.Background(), 1500*time.Millisecond)
		rec(id, "invoke", "recv", topic, group)
		fs := cl.PollRecords(ctx, max-got)
		cancel()
		recs := fs.Records()
		if len(recs) == 0 {
			rec(id, "ok", "recv", topic, group)
			if empty++; empty >= 2 && got > 0 {
				break // caught up
			}
			continue
		}
		recvRecords(id, group, recs)
		for _, r := range recs {
			if o, ok := last[r.Partition]; !ok || r.Offset > o {
				last[r.Partition] = r.Offset
			}
		}
		got += len(recs)
	}
	if err := commitOffsets(cl, id, group, last); err != nil {
		die(1, "consume: read %d, commit: %v", got, err)
	}
	fmt.Printf("consumed %d, committed %v\n", got, last)
}

// ---- drain ----

// ends returns the last stable offset and the high watermark of each partition
func ends(cl *kgo.Client) (lso, hwm map[int32]int64, err error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	lso, hwm = map[int32]int64{}, map[int32]int64{}
	for iso := int8(0); iso <= 1; iso++ {
		req := kmsg.NewPtrListOffsetsRequest()
		req.ReplicaID = -1
		req.IsolationLevel = iso
		rt := kmsg.NewListOffsetsRequestTopic()
		rt.Topic = topic
		for p := int32(0); p < partitions; p++ {
			rp := kmsg.NewListOffsetsRequestTopicPartition()
			rp.Partition = p
			rp.Timestamp = -1
			rp.CurrentLeaderEpoch = -1
			rt.Partitions = append(rt.Partitions, rp)
		}
		req.Topics = append(req.Topics, rt)
		// ListOffsets goes to each partition's leader: the client splits the request
		shards := cl.RequestSharded(ctx, req)
		n := 0
		for _, s := range shards {
			if s.Err != nil {
				return nil, nil, s.Err
			}
			resp := s.Resp.(*kmsg.ListOffsetsResponse)
			for _, t := range resp.Topics {
				for _, p := range t.Partitions {
					if e := kerr.ErrorForCode(p.ErrorCode); e != nil {
						return nil, nil, fmt.Errorf("p%d: %w", p.Partition, e)
					}
					if iso == 0 {
						hwm[p.Partition] = p.Offset
					} else {
						lso[p.Partition] = p.Offset
					}
					n++
				}
			}
		}
		if n != partitions {
			return nil, nil, fmt.Errorf("list offsets: %d of %d partitions", n, partitions)
		}
	}
	return lso, hwm, nil
}

// drainOne reads until the partitions' ends hold still with no open transaction (the last stable
// offset equals the high watermark on every leader) and the consumer received nothing for a
// quiet window after that, with no fetch error. An error or a refusal is "not yet".
func drainOne(proc int64, grp string, deadline float64) bool {
	const quiet = 8.0
	var cl *kgo.Client
	if grp == "-" {
		at := map[int32]kgo.Offset{}
		for p := int32(0); p < partitions; p++ {
			at[p] = kgo.NewOffset().AtStart()
		}
		cl = client(
			kgo.ConsumePartitions(map[string]map[int32]kgo.Offset{topic: at}),
			kgo.FetchIsolationLevel(kgo.ReadCommitted()),
			kgo.FetchMaxWait(500*time.Millisecond),
		)
	} else {
		cl = groupClient()
		rec(proc, "invoke", "sub", topic, grp, "from=start")
		rec(proc, "ok", "sub", topic, grp, "from=start")
	}
	defer cl.Close()
	read := 0
	lastData := uptime() // the last receive with records, or the last error
	lastCheck := 0.0
	stable := "" // the ends as last seen, while they hold still
	stableSince := 0.0
	why := "no end offsets yet"
	for uptime() < deadline {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		rec(proc, "invoke", "recv", topic, grp)
		fs := cl.PollRecords(ctx, 400)
		cancel()
		recs := fs.Records()
		for _, fe := range fs.Errors() {
			if !errors.Is(fe.Err, context.DeadlineExceeded) && !errors.Is(fe.Err, context.Canceled) {
				why = fmt.Sprintf("fetch p%d: %v", fe.Partition, fe.Err)
				lastData = uptime()
			}
		}
		if len(recs) == 0 {
			rec(proc, "ok", "recv", topic, grp)
		} else {
			recvRecords(proc, grp, recs)
			read += len(recs)
			lastData = uptime()
			if grp != "-" {
				last := map[int32]int64{}
				for _, r := range recs {
					last[r.Partition] = r.Offset
				}
				if err := commitOffsets(cl, proc, grp, last); err != nil {
					why = "commit: " + err.Error()
				}
			}
			continue
		}
		if uptime()-lastCheck < 2 {
			continue
		}
		lastCheck = uptime()
		lso, hwm, err := ends(cl)
		if err != nil {
			why = "end offsets: " + err.Error()
			lastData, stable = uptime(), ""
			continue
		}
		open := ""
		for p := int32(0); p < partitions; p++ {
			if lso[p] != hwm[p] {
				open = fmt.Sprintf("p%d has an open transaction (lso %d, hwm %d)", p, lso[p], hwm[p])
			}
		}
		if open != "" {
			why, stable = open, ""
			continue
		}
		now := fmt.Sprint(hwm)
		if now != stable {
			stable, stableSince = now, uptime()
			continue
		}
		if uptime()-stableSince >= quiet && uptime()-lastData >= quiet {
			rec(proc, "ok", "drained", topic, grp)
			fmt.Printf("drained %s: read %d, ends %v\n", grp, read, hwm)
			return true
		}
		why = "waiting for a quiet window"
	}
	msg := fmt.Sprintf("drain %s: no end within the deadline (read %d): %s\n", grp, read, why)
	fmt.Print(msg)
	if histOut != nil {
		histOut.WriteString("rpclient: " + msg) // the console keeps the reason; the drain's output stays in the guest
	}
	return false
}

func drain() {
	start := uptime()
	ok1 := drainOne(1000001, "-", start+150)
	ok2 := drainOne(1000002, group, start+230)
	if !ok1 || !ok2 {
		os.Exit(1)
	}
}

// ---- offsets ----

func offsets() {
	cl := client()
	defer cl.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	req := kmsg.NewPtrOffsetFetchRequest()
	req.Group = group
	rt := kmsg.NewOffsetFetchRequestTopic()
	rt.Topic = topic
	rt.Partitions = []int32{0, 1, 2}
	req.Topics = append(req.Topics, rt)
	resp, err := req.RequestWith(ctx, cl)
	if err != nil {
		die(2, "offset fetch: %v", err)
	}
	if e := kerr.ErrorForCode(resp.ErrorCode); e != nil {
		die(2, "offset fetch: %v", e)
	}
	for _, t := range resp.Topics {
		for _, p := range t.Partitions {
			if e := kerr.ErrorForCode(p.ErrorCode); e != nil {
				die(2, "offset fetch p%d: %v", p.Partition, e)
			}
			fmt.Printf("p%d %d\n", p.Partition, p.Offset)
		}
	}
}

// ---- dump ----

// dump <partition> <from>: one read_committed fetch of a partition of t from an offset, printed
// raw: the aborted transactions the leader reports (producer id, first offset), then every batch
// with its producer, epoch and flags (control batches with their marker: commit or abort), and
// each data record's value. This is what a Kafka client's read_committed filter works from.
func dump(part int32, from int64) {
	cl := client(kgo.MaxVersions(kversion.V3_0_0())) // Fetch v12: topics by name, not by id
	defer cl.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	req := kmsg.NewPtrFetchRequest()
	req.ReplicaID = -1
	req.MaxWaitMillis = 500
	req.MaxBytes = 1 << 20
	req.IsolationLevel = 1
	rt := kmsg.NewFetchRequestTopic()
	rt.Topic = topic
	rp := kmsg.NewFetchRequestTopicPartition()
	rp.Partition = part
	rp.FetchOffset = from
	rp.PartitionMaxBytes = 1 << 20
	rp.CurrentLeaderEpoch = -1
	rp.LogStartOffset = -1
	rt.Partitions = append(rt.Partitions, rp)
	req.Topics = append(req.Topics, rt)
	// a fetch goes to the partition's leader, which the metadata names
	mreq := kmsg.NewPtrMetadataRequest()
	mt := kmsg.NewMetadataRequestTopic()
	tn := topic
	mt.Topic = &tn
	mreq.Topics = append(mreq.Topics, mt)
	mresp, err := mreq.RequestWith(ctx, cl)
	if err != nil || len(mresp.Topics) != 1 {
		die(2, "metadata: %v", err)
	}
	leader := int32(-1)
	for _, mp := range mresp.Topics[0].Partitions {
		if mp.Partition == part {
			leader = mp.Leader
		}
	}
	if leader < 0 {
		die(2, "p%d has no leader", part)
	}
	fmt.Printf("p%d leader n%d\n", part, leader)
	kr, err := cl.Broker(int(leader)).Request(ctx, req)
	if err != nil {
		die(2, "fetch: %v", err)
	}
	for _, resp := range []*kmsg.FetchResponse{kr.(*kmsg.FetchResponse)} {
		for _, t := range resp.Topics {
			for _, p := range t.Partitions {
				fmt.Printf("p%d error=%v hwm=%d lso=%d\n", p.Partition, kerr.ErrorForCode(p.ErrorCode), p.HighWatermark, p.LastStableOffset)
				for _, a := range p.AbortedTransactions {
					fmt.Printf("aborted producer=%d first=%d\n", a.ProducerID, a.FirstOffset)
				}
				raw := p.RecordBatches
				for len(raw) >= 61 {
					var b kmsg.RecordBatch
					if err := b.ReadFrom(raw); err != nil {
						fmt.Printf("batch parse: %v\n", err)
						break
					}
					n := int(b.Length) + 12
					if n <= 0 || n > len(raw) {
						break
					}
					raw = raw[n:]
					ctl := b.Attributes&0x20 != 0
					txn := b.Attributes&0x10 != 0
					line := fmt.Sprintf("batch base=%d last=%d producer=%d epoch=%d txn=%v control=%v", b.FirstOffset, b.FirstOffset+int64(b.LastOffsetDelta), b.ProducerID, b.ProducerEpoch, txn, ctl)
					var recs []kmsg.Record
					if b.Attributes&0x07 == 0 { // uncompressed: the client produces without compression
						recs = readRecords(int(b.NumRecords), b.Records)
					}
					for _, r := range recs {
						if ctl && len(r.Key) >= 4 {
							kind := "abort"
							if r.Key[3] == 1 {
								kind = "commit"
							}
							line += " marker=" + kind
						} else if !ctl {
							line += fmt.Sprintf(" %d:%s", b.FirstOffset+int64(r.OffsetDelta), r.Value)
						}
					}
					fmt.Println(line)
				}
			}
		}
	}
}

func readRecords(n int, in []byte) []kmsg.Record {
	var out []kmsg.Record
	for i := 0; i < n && len(in) > 0; i++ {
		l, used := varint(in)
		if used <= 0 || int(l)+used > len(in) {
			break
		}
		var r kmsg.Record
		if err := r.ReadFrom(in[:used+int(l)]); err != nil {
			break
		}
		out = append(out, r)
		in = in[used+int(l):]
	}
	return out
}

func varint(b []byte) (int64, int) {
	var x uint64
	var s uint
	for i, c := range b {
		if i == 10 {
			return 0, -1
		}
		x |= uint64(c&0x7f) << s
		if c < 0x80 {
			return int64(x>>1) ^ -int64(x&1), i + 1
		}
		s += 7
	}
	return 0, 0
}

// ---- lso ----

// lso: exit 0 when no transaction is open on any partition of t (each leader's last stable
// offset equals its high watermark), 1 when one is, 2 when a leader does not answer
func lso() {
	cl := client()
	defer cl.Close()
	l, h, err := ends(cl)
	if err != nil {
		die(2, "cannot read the end offsets: %v", err)
	}
	open := 0
	for p := int32(0); p < partitions; p++ {
		if l[p] != h[p] {
			fmt.Printf("p%d has an open transaction: last stable offset %d, high watermark %d\n", p, l[p], h[p])
			open++
		}
	}
	if open > 0 {
		os.Exit(1)
	}
	fmt.Printf("no open transaction: ends %v\n", h)
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
		die(2, "usage: rpclient create|produce|txn|consume|drain|offsets|lso|dump ...")
	}
	arg := func(i int) int64 {
		if len(os.Args) <= i {
			die(2, "missing argument")
		}
		return atoi(os.Args[i])
	}
	switch os.Args[1] {
	case "create":
		create()
	case "produce":
		produce(arg(2), arg(3))
	case "txn":
		txn(arg(2), arg(3))
	case "consume":
		consume(arg(2), arg(3))
	case "drain":
		drain()
	case "offsets":
		offsets()
	case "lso":
		lso()
	case "dump":
		dump(int32(arg(2)), arg(3))
	default:
		die(2, "unknown command %s", os.Args[1])
	}
}
