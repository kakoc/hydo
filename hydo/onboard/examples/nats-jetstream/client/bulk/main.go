// natsbulk: fills the stream of the three-node JetStream cluster with many messages, so that
// each node's store holds several 8 MB blocks (Jepsen's finding 3.2 needs a corrupted block in
// the middle of the stream; RESULTS.md, "Several nodes' corrupted blocks").
//
//	natsbulk fill <prefix> <count> [<size>]
//	natsbulk read
//
// fill publishes <count> messages <prefix>000001, <prefix>000002, ... to the stream, each padded with
// spaces to <size> bytes (default 100; the readers trim them), up to 1,000 unacknowledged at a
// time, and prints the value of each acknowledged message on its own line, in order. Exit code:
// 0 every message was acknowledged, 3 some were not (the printed ones were), 1 no connection.
// read prints every message of the stream, one value per line (below).
//
// It is a program of its own so that the bytes of natsjs, and the images of the other
// manifests of this example, stay as they were.
package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

const subject = "jepsen.v"

func servers() string {
	if s := os.Getenv("NATS_URL"); s != "" {
		return s
	}
	return "nats://n1:4222,nats://n2:4222,nats://n3:4222"
}

func fill(prefix string, count, size int) int {
	nc, err := nats.Connect(servers(), nats.Name("hydo-natsbulk"), nats.Timeout(5*time.Second))
	if err != nil {
		fmt.Fprintln(os.Stderr, "connect:", err)
		return 1
	}
	defer nc.Close()
	js, err := jetstream.New(nc, jetstream.WithPublishAsyncMaxPending(1000))
	if err != nil {
		fmt.Fprintln(os.Stderr, "jetstream:", err)
		return 1
	}
	out := make([]string, 0, count)
	futures := make([]jetstream.PubAckFuture, 0, 1000)
	failed := 0
	settle := func() {
		for i, f := range futures {
			select {
			case <-f.Ok():
				out = append(out, strings.TrimSpace(string(f.Msg().Data)))
			case err := <-f.Err():
				failed++
				if failed <= 5 {
					fmt.Fprintf(os.Stderr, "message %d of the batch: %v\n", i, err)
				}
			case <-time.After(30 * time.Second):
				failed++
			}
		}
		futures = futures[:0]
	}
	for i := 1; i <= count; i++ {
		v := fmt.Sprintf("%s%06d", prefix, i)
		if len(v) < size {
			v += strings.Repeat(" ", size-len(v))
		}
		f, err := js.PublishAsync(subject, []byte(v))
		if err != nil {
			failed++
			continue
		}
		futures = append(futures, f)
		if len(futures) == 1000 {
			settle()
		}
	}
	settle()
	for _, v := range out {
		fmt.Println(v)
	}
	fmt.Fprintf(os.Stderr, "fill: %d acknowledged, %d not\n", len(out), failed)
	if failed > 0 {
		return 3
	}
	return 0
}

// read prints every message of the stream, as natsjs read does, for a stream of many messages:
// it gets each message by its sequence, without a consumer, and has 150 s. Exit
// codes as natsjs read: 0 all read, 4 the stream does not exist, 2 not readable now.
func read(deadline time.Duration) int {
	end := time.Now().Add(deadline)
	var last error
	for time.Now().Before(end) {
		vals, notFound, err := readOnce(time.Until(end))
		if notFound {
			fmt.Println("stream not found:", err)
			return 4
		}
		if err == nil {
			w := bufio.NewWriterSize(os.Stdout, 1<<20)
			for _, v := range vals {
				fmt.Fprintln(w, v)
			}
			w.Flush()
			return 0
		}
		last = err
		fmt.Fprintln(os.Stderr, "read attempt:", err)
		time.Sleep(time.Second)
	}
	fmt.Println("read failed:", last)
	return 2
}

func readOnce(budget time.Duration) ([]string, bool, error) {
	nc, err := nats.Connect(servers(), nats.Name("hydo-natsbulk"), nats.Timeout(2*time.Second))
	if err != nil {
		return nil, false, err
	}
	defer nc.Close()
	js, err := jetstream.New(nc)
	if err != nil {
		return nil, false, err
	}
	ctx, cancel := context.WithTimeout(context.Background(), budget)
	defer cancel()
	s, err := js.Stream(ctx, "jepsen")
	if errors.Is(err, jetstream.ErrStreamNotFound) {
		return nil, true, err
	}
	if err != nil {
		return nil, false, err
	}
	info, err := s.Info(ctx)
	if err != nil {
		return nil, false, err
	}
	lastSeq := info.State.LastSeq
	leader := ""
	if info.Cluster != nil {
		leader = info.Cluster.Leader
	}
	fmt.Fprintf(os.Stderr, "stream msgs=%d first=%d last=%d leader=%s\n",
		info.State.Msgs, info.State.FirstSeq, lastSeq, leader)
	out := make([]string, 0, info.State.Msgs)
	if info.State.Msgs == 0 {
		return out, false, nil
	}
	// each message by its sequence from the stream's leader: with 24,000 messages of 1 KB an
	// ordered consumer of nats.go 1.37 got "no responders" on every attempt in the guest
	for seq := info.State.FirstSeq; seq <= lastSeq; seq++ {
		m, err := s.GetMsg(ctx, seq)
		if errors.Is(err, jetstream.ErrMsgNotFound) {
			continue // deleted or never stored: a gap the checks count
		}
		if err != nil {
			return nil, false, fmt.Errorf("sequence %d of %d: %w", seq, lastSeq, err)
		}
		out = append(out, strings.TrimSpace(string(m.Data)))
	}
	return out, false, nil
}

func main() {
	if len(os.Args) == 2 && os.Args[1] == "read" {
		os.Exit(read(150 * time.Second))
	}
	if len(os.Args) < 4 || os.Args[1] != "fill" {
		fmt.Fprintln(os.Stderr, "usage: natsbulk fill <prefix> <count> [<size>] | natsbulk read")
		os.Exit(2)
	}
	count, err := strconv.Atoi(os.Args[3])
	if err != nil || count < 1 {
		os.Exit(2)
	}
	size := 100
	if len(os.Args) > 4 {
		if size, err = strconv.Atoi(os.Args[4]); err != nil || size < 1 {
			os.Exit(2)
		}
	}
	os.Exit(fill(os.Args[2], count, size))
}
