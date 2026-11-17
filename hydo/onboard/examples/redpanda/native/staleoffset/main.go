// staleoffset: finding 4 of ../../RESULTS.md without hydo. A transactional producer restarts
// its sequence numbers at 0 on each partition in each new epoch. A partition's follower keeps
// the finished requests of every epoch in its cache of the producer's requests, and the cache
// matches a request by its first and last sequence number only. After a change of the
// partition's leader, a request whose sequence range an earlier epoch used gets that earlier
// batch's answer.
//
//	staleoffset create              creates the topic t (3 partitions, 3 replicas)
//	staleoffset retry [-n N] [-p P] [-transfer=false]
//	    N times: transaction 1 writes A0 to partition P and commits. Transaction 2, a new
//	    producer with the same transactional id, writes B0 to P. The leader of P moves to
//	    another node, and the same Produce request of B0 is sent again to the new leader, as a
//	    client does when the first answer is lost. Then transaction 2 commits.
//	staleoffset loss [-n N] [-p P] [-transfer=false]
//	    N times: transaction 1 writes A0 and A1 to P, one Produce request each, and commits.
//	    Transaction 2, a new producer with the same transactional id, writes B0, the leader of
//	    P moves to another node, and it writes B1 and commits. No request is sent twice.
//
// Each mode reads partition P back at the end, with read_committed and read_uncommitted, and
// compares the offsets that the producers were given with the records at those offsets.
package main

import (
	"context"
	"encoding/binary"
	"flag"
	"fmt"
	"hash/crc32"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/twmb/franz-go/pkg/kerr"
	"github.com/twmb/franz-go/pkg/kgo"
	"github.com/twmb/franz-go/pkg/kmsg"
)

const topic = "t"

var brokers = strings.Split(envOr("BROKERS", "n1:9092,n2:9092,n3:9092"), ",")

func envOr(k, d string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return d
}

func client(opts ...kgo.Opt) *kgo.Client {
	base := []kgo.Opt{
		kgo.SeedBrokers(brokers...),
		kgo.DialTimeout(2 * time.Second),
		kgo.RequestTimeoutOverhead(3 * time.Second),
		kgo.RetryTimeout(15 * time.Second),
		kgo.MetadataMinAge(250 * time.Millisecond),
	}
	cl, err := kgo.NewClient(append(base, opts...)...)
	if err != nil {
		fmt.Fprintln(os.Stderr, "client:", err)
		os.Exit(2)
	}
	return cl
}

func producer(txid string) *kgo.Client {
	return client(
		kgo.DefaultProduceTopic(topic),
		kgo.RecordPartitioner(kgo.ManualPartitioner()),
		kgo.TransactionalID(txid),
		kgo.TransactionTimeout(30*time.Second),
		kgo.RecordDeliveryTimeout(15*time.Second),
		kgo.ProducerLinger(0),
	)
}

func create() {
	cl := client()
	defer cl.Close()
	end := time.Now().Add(100 * time.Second)
	for time.Now().Before(end) {
		ctx, cancel := context.WithTimeout(context.Background(), 8*time.Second)
		req := kmsg.NewPtrCreateTopicsRequest()
		t := kmsg.NewCreateTopicsRequestTopic()
		t.Topic, t.NumPartitions, t.ReplicationFactor = topic, 3, 3
		req.Topics = append(req.Topics, t)
		req.TimeoutMillis = 7000
		resp, err := req.RequestWith(ctx, cl)
		cancel()
		if err == nil && len(resp.Topics) == 1 {
			e := kerr.ErrorForCode(resp.Topics[0].ErrorCode)
			if e == nil || e == kerr.TopicAlreadyExists {
				ok := true
				for p := int32(0); p < 3; p++ {
					if leader(cl, p) < 0 {
						ok = false
					}
				}
				if ok {
					fmt.Println("topic t ready")
					return
				}
			}
		}
		time.Sleep(time.Second)
	}
	fmt.Fprintln(os.Stderr, "create: no topic after 100 s")
	os.Exit(1)
}

// leader returns the node id of partition p's leader, or -1.
func leader(cl *kgo.Client, p int32) int32 {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	req := kmsg.NewPtrMetadataRequest()
	rt := kmsg.NewMetadataRequestTopic()
	tn := topic
	rt.Topic = &tn
	req.Topics = append(req.Topics, rt)
	resp, err := req.RequestWith(ctx, cl)
	if err != nil || len(resp.Topics) != 1 {
		return -1
	}
	for _, pp := range resp.Topics[0].Partitions {
		if pp.Partition == p && pp.ErrorCode == 0 {
			return pp.Leader
		}
	}
	return -1
}

// move transfers partition p's leadership to another node through the admin API and waits
// until the metadata names the new leader. It returns the old and the new leader.
func move(cl *kgo.Client, p int32) (int32, int32) {
	for try := 0; try < 5; try++ {
		from := leader(cl, p)
		if from < 0 {
			time.Sleep(time.Second)
			continue
		}
		to := from%3 + 1
		url := fmt.Sprintf("http://n%d:9644/v1/partitions/kafka/%s/%d/transfer_leadership?target=%d", from, topic, p, to)
		resp, err := http.Post(url, "application/json", nil)
		if err == nil {
			resp.Body.Close()
		}
		end := time.Now().Add(20 * time.Second)
		for time.Now().Before(end) {
			if leader(cl, p) == to {
				return from, to
			}
			time.Sleep(200 * time.Millisecond)
		}
	}
	fmt.Fprintln(os.Stderr, "move: the leader did not move")
	os.Exit(1)
	return -1, -1
}

// batch encodes one transactional record batch of one record with the given sequence.
func batch(pid int64, epoch int16, seq int32, value []byte) []byte {
	r := kmsg.Record{Value: value}
	body := r.AppendTo(nil)
	r.Length = int32(len(body) - 1) // the varint of 0 takes one byte
	rec := r.AppendTo(nil)
	now := time.Now().UnixMilli()
	b := kmsg.RecordBatch{
		PartitionLeaderEpoch: -1, Magic: 2, Attributes: 0x10, // transactional
		FirstTimestamp: now, MaxTimestamp: now,
		ProducerID: pid, ProducerEpoch: epoch, FirstSequence: seq, NumRecords: 1, Records: rec,
	}
	out := b.AppendTo(nil)
	b.Length = int32(len(out) - 12)
	out = b.AppendTo(nil)
	crc := crc32.Checksum(out[21:], crc32.MakeTable(crc32.Castagnoli))
	binary.BigEndian.PutUint32(out[17:21], crc)
	return out
}

// resend sends one Produce request of a transactional batch to the broker `to`, and returns the
// answer's base offset and error.
func resend(cl *kgo.Client, to int32, txid string, p int32, pid int64, epoch int16, seq int32, value []byte) (int64, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	req := kmsg.NewPtrProduceRequest()
	req.TransactionID = &txid
	req.Acks = -1
	req.TimeoutMillis = 10000
	rt := kmsg.NewProduceRequestTopic()
	rt.Topic = topic
	rp := kmsg.NewProduceRequestTopicPartition()
	rp.Partition = p
	rp.Records = batch(pid, epoch, seq, value)
	rt.Partitions = append(rt.Partitions, rp)
	req.Topics = append(req.Topics, rt)
	// a new leader answers NOT_LEADER_FOR_PARTITION until it is ready: ask again, as a client does
	for try := 0; ; try++ {
		kr, err := cl.Broker(int(to)).Request(ctx, req)
		if err != nil {
			return -1, err
		}
		resp := kr.(*kmsg.ProduceResponse)
		if len(resp.Topics) != 1 || len(resp.Topics[0].Partitions) != 1 {
			return -1, fmt.Errorf("short answer")
		}
		pr := resp.Topics[0].Partitions[0]
		err = kerr.ErrorForCode(pr.ErrorCode)
		if err == kerr.NotLeaderForPartition && try < 20 {
			time.Sleep(250 * time.Millisecond)
			continue
		}
		return pr.BaseOffset, err
	}
}

// read returns partition p's records by offset.
func read(p int32, iso kgo.IsolationLevel) map[int64]string {
	cl := client(
		kgo.ConsumePartitions(map[string]map[int32]kgo.Offset{topic: {p: kgo.NewOffset().AtStart()}}),
		kgo.FetchIsolationLevel(iso),
	)
	defer cl.Close()
	seen := map[int64]string{}
	idle := 0
	for idle < 3 {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		fs := cl.PollFetches(ctx)
		cancel()
		n := 0
		fs.EachRecord(func(r *kgo.Record) { seen[r.Offset] = string(r.Value); n++ })
		if n == 0 {
			idle++
		} else {
			idle = 0
		}
	}
	return seen
}

// txn1 commits one transaction of the given values to p, one Produce request per value.
func txn1(txid string, p int32, values ...string) ([]int64, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	c := producer(txid)
	defer c.Close()
	if err := c.BeginTransaction(); err != nil {
		return nil, err
	}
	var offs []int64
	for _, v := range values {
		r := &kgo.Record{Value: []byte(v), Partition: p}
		if err := c.ProduceSync(ctx, r).FirstErr(); err != nil {
			_ = c.EndTransaction(ctx, kgo.TryAbort)
			return nil, err
		}
		offs = append(offs, r.Offset)
	}
	return offs, c.EndTransaction(ctx, kgo.TryCommit)
}

type result struct {
	name  string // the record whose answer is checked
	given int64  // the offset its producer was given
}

func retry(args []string) {
	fs := flag.NewFlagSet("retry", flag.ExitOnError)
	n := fs.Int("n", 10, "rounds")
	pp := fs.Int("p", 0, "partition")
	transfer := fs.Bool("transfer", true, "move the leader before the resend")
	fs.Parse(args)
	p := int32(*pp)
	admin := client()
	defer admin.Close()
	var res []result
	wrong := 0
	for i := 0; i < *n; i++ {
		tag := fmt.Sprintf("r%d-%d", os.Getpid(), i)
		txid := "retry-" + tag
		a0 := tag + "-A0"
		oa, err := txn1(txid, p, a0)
		if err != nil {
			fmt.Printf("%d: transaction 1: %v\n", i, err)
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		c := producer(txid)
		_ = c.BeginTransaction()
		b0 := tag + "-B0"
		rb := &kgo.Record{Value: []byte(b0), Partition: p}
		if err := c.ProduceSync(ctx, rb).FirstErr(); err != nil {
			fmt.Printf("%d: B0: %v\n", i, err)
			c.Close()
			cancel()
			continue
		}
		pid, epoch, _ := c.ProducerID(ctx)
		from, to := leader(admin, p), leader(admin, p)
		if *transfer {
			from, to = move(admin, p)
		}
		off, rerr := resend(c, to, txid, p, pid, epoch, 0, []byte(b0))
		cerr := c.EndTransaction(ctx, kgo.TryCommit)
		c.Close()
		cancel()
		fmt.Printf("%d: pid %d: A0@%d (epoch %d); B0@%d (epoch %d) on n%d; B0 sent again to n%d: answer @%d %v; commit: %v\n",
			i, pid, oa[0], epoch-1, rb.Offset, epoch, from, to, off, rerr, cerr)
		if rerr == nil && off != rb.Offset {
			wrong++
			res = append(res, result{b0, off})
		}
	}
	fmt.Printf("answers to the resent B0 with another offset than its first answer: %d of %d\n", wrong, *n)
	show(p, res)
}

func loss(args []string) {
	fs := flag.NewFlagSet("loss", flag.ExitOnError)
	n := fs.Int("n", 10, "rounds")
	pp := fs.Int("p", 0, "partition")
	transfer := fs.Bool("transfer", true, "move the leader between B0 and B1")
	fs.Parse(args)
	p := int32(*pp)
	admin := client()
	defer admin.Close()
	var res []result
	ok := 0
	for i := 0; i < *n; i++ {
		tag := fmt.Sprintf("l%d-%d", os.Getpid(), i)
		txid := "loss-" + tag
		oa, err := txn1(txid, p, tag+"-A0", tag+"-A1")
		if err != nil {
			fmt.Printf("%d: transaction 1: %v\n", i, err)
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		c := producer(txid)
		_ = c.BeginTransaction()
		r0 := &kgo.Record{Value: []byte(tag + "-B0"), Partition: p}
		r1 := &kgo.Record{Value: []byte(tag + "-B1"), Partition: p}
		e0 := c.ProduceSync(ctx, r0).FirstErr()
		from, to := leader(admin, p), leader(admin, p)
		if *transfer && e0 == nil {
			from, to = move(admin, p)
		}
		e1 := c.ProduceSync(ctx, r1).FirstErr()
		cerr := c.EndTransaction(ctx, kgo.TryCommit)
		c.Close()
		cancel()
		fmt.Printf("%d: A0@%d A1@%d; B0@%d on n%d; B1@%d on n%d; produce: %v %v; commit: %v\n",
			i, oa[0], oa[1], r0.Offset, from, r1.Offset, to, e0, e1, cerr)
		if e0 == nil && e1 == nil && cerr == nil {
			ok++
			res = append(res, result{tag + "-B0", r0.Offset}, result{tag + "-B1", r1.Offset})
		}
	}
	fmt.Printf("transaction 2 committed without error: %d of %d\n", ok, *n)
	show(p, res)
}

// show reads p back and prints each checked record that is not at the offset its producer was
// given, and whether a read_committed reader reads it at all.
func show(p int32, res []result) {
	if len(res) == 0 {
		return
	}
	committed := read(p, kgo.ReadCommitted())
	all := read(p, kgo.ReadUncommitted())
	where, inLog := map[string]int64{}, map[string]bool{}
	for o, v := range committed {
		where[v] = o
	}
	for _, v := range all {
		inLog[v] = true
	}
	bad, lost := 0, 0
	for _, r := range res {
		at, read := where[r.name]
		if read && at == r.given {
			continue
		}
		bad++
		if !read {
			lost++
			fmt.Printf("LOST %s: given offset %d, which holds %q; no read_committed reader reads %s; in the log at all (read_uncommitted): %v\n",
				r.name, r.given, all[r.given], r.name, inLog[r.name])
		} else {
			fmt.Printf("WRONG-OFFSET %s: given offset %d, which holds %q; read at offset %d\n", r.name, r.given, all[r.given], at)
		}
	}
	fmt.Printf("records checked: %d; at another offset than given: %d; never read with read_committed: %d\n", len(res), bad-lost, lost)
}

func main() {
	cmd := ""
	if len(os.Args) > 1 {
		cmd = os.Args[1]
	}
	switch cmd {
	case "create":
		create()
	case "retry":
		retry(os.Args[2:])
	case "loss":
		loss(os.Args[2:])
	default:
		fmt.Fprintln(os.Stderr, "usage: staleoffset create|retry|loss ...")
		os.Exit(2)
	}
}
