// natsjs: the test client for the three-node JetStream cluster (hydo's workload and checks).
//
//	natsjs create          create the stream (3 replicas, file storage); idempotent
//	natsjs pub <value>     publish <value> and wait for the JetStream acknowledgement
//	natsjs read            print every message of the stream, one value per line
//
// Exit codes. pub: 0 acknowledged, 1 certainly not stored (no server took the request),
// 3 unknown (a timeout or another error: it may have been stored). read: 0 all messages read,
// 4 the cluster answered that the stream does not exist, 2 the stream could not be read now.
//
// The workload follows Jepsen's NATS queue test: unique values published to one stream; after
// the faults, every acknowledged value must be readable.
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

const (
	stream  = "jepsen"
	subject = "jepsen.v"
)

func servers() string {
	if s := os.Getenv("NATS_URL"); s != "" {
		return s
	}
	return "nats://n1:4222,nats://n2:4222,nats://n3:4222"
}

func connect() (*nats.Conn, jetstream.JetStream, error) {
	nc, err := nats.Connect(servers(),
		nats.Name("hydo-natsjs"),
		nats.Timeout(2*time.Second),
		nats.RetryOnFailedConnect(false),
		nats.MaxReconnects(5),
		nats.ReconnectWait(200*time.Millisecond),
		nats.NoCallbacksAfterClientClose(),
	)
	if err != nil {
		return nil, nil, err
	}
	js, err := jetstream.New(nc)
	if err != nil {
		nc.Close()
		return nil, nil, err
	}
	return nc, js, nil
}

func create(deadline time.Duration) int {
	end := time.Now().Add(deadline)
	for {
		err := func() error {
			nc, js, err := connect()
			if err != nil {
				return err
			}
			defer nc.Close()
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			_, err = js.CreateOrUpdateStream(ctx, jetstream.StreamConfig{
				Name:     stream,
				Subjects: []string{"jepsen.>"},
				Replicas: 3,
				Storage:  jetstream.FileStorage,
			})
			return err
		}()
		if err == nil {
			fmt.Println("stream created")
			return 0
		}
		if time.Now().After(end) {
			fmt.Println("create failed:", err)
			return 1
		}
		time.Sleep(500 * time.Millisecond)
	}
}

func pub(value string) int {
	nc, js, err := connect()
	if err != nil {
		fmt.Println("connect:", err)
		return 1 // nothing was sent
	}
	defer nc.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	ack, err := js.Publish(ctx, subject, []byte(value), jetstream.WithMsgID(value))
	if err == nil {
		fmt.Printf("ack %s seq=%d\n", value, ack.Sequence)
		return 0
	}
	fmt.Println("publish:", err)
	if errors.Is(err, nats.ErrNoResponders) || errors.Is(err, jetstream.ErrNoStreamResponse) {
		return 1 // no server took the request
	}
	return 3
}

func read(deadline time.Duration) int {
	end := time.Now().Add(deadline)
	var last error
	for time.Now().Before(end) {
		vals, notFound, err := readOnce()
		if notFound {
			fmt.Println("stream not found:", err)
			return 4
		}
		if err == nil {
			for _, v := range vals {
				fmt.Println(v)
			}
			return 0
		}
		last = err
		time.Sleep(time.Second)
	}
	fmt.Println("read failed:", last)
	return 2
}

func readOnce() ([]string, bool, error) {
	nc, js, err := connect()
	if err != nil {
		return nil, false, err
	}
	defer nc.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	s, err := js.Stream(ctx, stream)
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
	var out []string
	if info.State.Msgs == 0 {
		return out, false, nil
	}
	c, err := s.OrderedConsumer(ctx, jetstream.OrderedConsumerConfig{})
	if err != nil {
		return nil, false, err
	}
	for {
		batch, err := c.Fetch(256, jetstream.FetchMaxWait(2*time.Second))
		if err != nil {
			return nil, false, err
		}
		n := 0
		var seq uint64
		for m := range batch.Messages() {
			n++
			md, err := m.Metadata()
			if err != nil {
				return nil, false, err
			}
			seq = md.Sequence.Stream
			out = append(out, strings.TrimSpace(string(m.Data())))
		}
		if err := batch.Error(); err != nil && !errors.Is(err, nats.ErrTimeout) {
			return nil, false, err
		}
		if seq >= lastSeq {
			return out, false, nil
		}
		if n == 0 {
			return nil, false, fmt.Errorf("read stopped at stream sequence %d of %d", seq, lastSeq)
		}
	}
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: natsjs create|pub <value>|read")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "create":
		os.Exit(create(60 * time.Second))
	case "pub":
		if len(os.Args) != 3 {
			os.Exit(2)
		}
		os.Exit(pub(os.Args[2]))
	case "read":
		os.Exit(read(60 * time.Second))
	}
	os.Exit(2)
}
