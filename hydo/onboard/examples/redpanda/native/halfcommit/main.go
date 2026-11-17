// halfcommit: finding 3 of ../../RESULTS.md without hydo. A transactional producer writes one
// record to partition 1 and one to partition 2 of the topic t and commits. A second producer
// with the same transactional id calls InitProducerId while the first one's transaction is open.
// Redpanda aborts the open transaction but keeps the producer epoch, so the first producer is
// not fenced: its next AddPartitionsToTxn starts a new transaction that holds only partition 2,
// and its commit returns no error. Its record on partition 1 is aborted.
//
//	halfcommit steps [-n N]   N times, in this order: A produces to p1; B sends InitProducerId
//	                          with the same transactional id; A produces to p2; A commits
//	halfcommit race  [-n N]   N pairs of producers with one transactional id at the same time,
//	                          each writing to p1 and p2 and committing, as the hvx runs did
//	halfcommit check          reads committed.txt and prints every committed record that a
//	                          read_committed reader never reads (exit 1 if there is one)
//
// steps and race append one line per commit that returned no error to committed.txt.
package main

import (
	"bufio"
	"context"
	"flag"
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"
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
		kgo.RetryTimeout(8 * time.Second),
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
		kgo.TransactionTimeout(10*time.Second),
		kgo.RecordDeliveryTimeout(8*time.Second),
		kgo.ProducerLinger(0),
	)
}

func record(out *os.File, a, b, txid string, ra, rb *kgo.Record) {
	fmt.Fprintf(out, "%s %s %s p%d@%d p%d@%d\n", a, b, txid, ra.Partition, ra.Offset, rb.Partition, rb.Offset)
}

func openOut() *os.File {
	out, err := os.OpenFile("committed.txt", os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	return out
}

// steps: the interleaving of the hvx trace, one step after the other.
func steps(args []string) {
	fs := flag.NewFlagSet("steps", flag.ExitOnError)
	n := fs.Int("n", 20, "transactions")
	fs.Parse(args)
	out := openOut()
	defer out.Close()
	ok := 0
	for i := 0; i < *n; i++ {
		txid := fmt.Sprintf("steps-%d-%d", os.Getpid(), i)
		a, b := fmt.Sprintf("s%d-%da", os.Getpid(), i), fmt.Sprintf("s%d-%db", os.Getpid(), i)
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		A := producer(txid)
		if err := A.BeginTransaction(); err != nil {
			fmt.Println("begin:", err)
		}
		// 1. A writes to p1: the client sends InitProducerId, AddPartitionsToTxn [p1], Produce
		ra := &kgo.Record{Value: []byte(a), Partition: 1}
		if err := A.ProduceSync(ctx, ra).FirstErr(); err != nil {
			fmt.Printf("%d: A produce p1: %v\n", i, err)
			A.Close()
			cancel()
			continue
		}
		// 2. B, another instance with the same transactional id, sends InitProducerId once
		B := client()
		req := kmsg.NewPtrInitProducerIDRequest()
		req.TransactionalID = &txid
		req.TransactionTimeoutMillis = 10000
		req.ProducerID, req.ProducerEpoch = -1, -1
		resp, err := req.RequestWith(ctx, B)
		B.Close()
		berr := "no error"
		if err != nil {
			berr = err.Error()
		} else if e := kerr.ErrorForCode(resp.ErrorCode); e != nil {
			berr = e.Error()
		}
		// 3. A writes to p2 and commits
		rb := &kgo.Record{Value: []byte(b), Partition: 2}
		perr := A.ProduceSync(ctx, rb).FirstErr()
		cerr := A.EndTransaction(ctx, kgo.TryCommit)
		A.Close()
		cancel()
		fmt.Printf("%d: %s: A p1@%d; B's InitProducerId: %s; A p2@%d produce: %v; A commit: %v\n",
			i, txid, ra.Offset, berr, rb.Offset, perr, cerr)
		if perr == nil && cerr == nil {
			ok++
			record(out, a, b, txid, ra, rb)
		}
	}
	fmt.Printf("commits without error: %d of %d\n", ok, *n)
}

// race: two producers with one transactional id at the same time, as the guest's client ran.
func race(args []string) {
	fs := flag.NewFlagSet("race", flag.ExitOnError)
	n := fs.Int("n", 100, "pairs")
	fs.Parse(args)
	out := openOut()
	defer out.Close()
	var mu sync.Mutex
	ok := 0
	for i := 0; i < *n; i++ {
		txid := fmt.Sprintf("race-%d-%d", os.Getpid(), i%4)
		var wg sync.WaitGroup
		for j := 0; j < 2; j++ {
			a, b := fmt.Sprintf("r%d-%d.%da", os.Getpid(), i, j), fmt.Sprintf("r%d-%d.%db", os.Getpid(), i, j)
			wg.Add(1)
			go func() {
				defer wg.Done()
				ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
				defer cancel()
				A := producer(txid)
				defer A.Close()
				if A.BeginTransaction() != nil {
					return
				}
				ra := &kgo.Record{Value: []byte(a), Partition: 1}
				rb := &kgo.Record{Value: []byte(b), Partition: 2}
				if A.ProduceSync(ctx, ra, rb).FirstErr() != nil {
					_ = A.EndTransaction(ctx, kgo.TryAbort)
					return
				}
				if A.EndTransaction(ctx, kgo.TryCommit) != nil {
					return
				}
				mu.Lock()
				ok++
				record(out, a, b, txid, ra, rb)
				mu.Unlock()
			}()
		}
		wg.Wait()
	}
	fmt.Printf("commits without error: %d of %d\n", ok, 2**n)
}

// read returns every record value of partitions 1 and 2 with its partition and offset.
func read(iso kgo.IsolationLevel) map[string]string {
	cl := client(
		kgo.ConsumePartitions(map[string]map[int32]kgo.Offset{topic: {
			1: kgo.NewOffset().AtStart(), 2: kgo.NewOffset().AtStart()}}),
		kgo.FetchIsolationLevel(iso),
	)
	defer cl.Close()
	seen := map[string]string{}
	idle := 0
	for idle < 3 {
		ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
		fs := cl.PollFetches(ctx)
		cancel()
		n := 0
		fs.EachRecord(func(r *kgo.Record) {
			seen[string(r.Value)] = fmt.Sprintf("p%d@%d", r.Partition, r.Offset)
			n++
		})
		if n == 0 {
			idle++
		} else {
			idle = 0
		}
	}
	return seen
}

func check() {
	f, err := os.Open("committed.txt")
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
	defer f.Close()
	var want []string
	txs := 0
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		fl := strings.Fields(sc.Text())
		if len(fl) >= 2 {
			want = append(want, fl[0], fl[1])
			txs++
		}
	}
	committed := read(kgo.ReadCommitted())
	var missing []string
	for _, v := range want {
		if _, ok := committed[v]; !ok {
			missing = append(missing, v)
		}
	}
	sort.Strings(missing)
	fmt.Printf("committed transactions %d (%d records); records a read_committed reader never read: %d\n", txs, len(want), len(missing))
	if len(missing) == 0 {
		return
	}
	all := read(kgo.ReadUncommitted())
	for _, v := range missing {
		fmt.Printf("HALF-COMMIT %s: never read with read_committed, in the log at %s\n", v, all[v])
	}
	os.Exit(1)
}

func main() {
	cmd := ""
	if len(os.Args) > 1 {
		cmd = os.Args[1]
	}
	switch cmd {
	case "steps":
		steps(os.Args[2:])
	case "race":
		race(os.Args[2:])
	case "check":
		check()
	default:
		fmt.Fprintln(os.Stderr, "usage: halfcommit steps|race|check ...")
		os.Exit(2)
	}
}
