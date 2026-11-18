// iggyc: the test client of the Iggy example. One static program with one subcommand per client
// operation, on Iggy's Go SDK (github.com/apache/iggy/foreign/go v0.10.0, the server's release).
// Each operation on the topic t writes HYDO-HIST records of the history `ig` (hydo guide
// messages): one record before the request and one after the reply.
//
//	iggyc ready                      exit 0 when the server answers a login and a stream list
//	iggyc create                     the stream s, the topics t and x, the consumer group g on t
//	iggyc produce <id> <seed>        1 to 3 messages to one partition of t, one batch
//	iggyc consume <id> <seed>        a member of group g: read, then store the offsets of what it read
//	iggyc expire <id> <seed>         16 messages of 8 KiB to x, the topic with a message expiry
//	iggyc drain                      the final read: every partition of t from offset 0, then group g
//	iggyc xprobe                     read x during the run: exit 100 when a message is gone before its expiry
//	iggyc check-expiry               the check of x: no message gone before its expiry, none kept long after
//	iggyc offsets                    print group g's stored offsets
//	iggyc topic <name>               print a topic's partitions and options
//
// The topics' durability (`replicated`, Iggy's default, or `persisted`) comes from IGGY_DURABILITY,
// which the image sets from the build parameter `durability` (hydo.yaml). Every choice comes from
// the seed hydo gives. Deadlines read /proc/uptime, which a clock step (T=) does not move; the
// expiry check reads the wall clock on purpose, as the server does.
package main

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/apache/iggy/foreign/go/client"
	"github.com/apache/iggy/foreign/go/client/reader"
	"github.com/apache/iggy/foreign/go/client/tcp"
	iggcon "github.com/apache/iggy/foreign/go/contracts"
)

const (
	streamName = "s"
	topicName  = "t" // the topic under the messages check
	expiryName = "x" // the topic with a message expiry
	groupName  = "g"
	partitions = 3
	hist       = "ig"
)

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

// rec writes one record: HYDO-HIST ig <process> <type> <time> <operation> <args>
func rec(proc int64, typ string, args ...string) {
	if histOut == nil {
		return
	}
	line := fmt.Sprintf("HYDO-HIST %s %d %s %s %s\n", hist, proc, typ, uptimeNs(), strings.Join(args, " "))
	histOut.WriteString(line) // one write: one record
}

func note(f string, a ...any) {
	msg := fmt.Sprintf(f, a...)
	fmt.Println(msg)
	if histOut != nil {
		histOut.WriteString("iggyc: " + msg + "\n") // the console keeps it; a command's output stays in the guest
	}
}

func item(name string, part uint32, off int64) string {
	if off < 0 {
		return fmt.Sprintf("%s;k=p%d", name, part)
	}
	return fmt.Sprintf("%s;k=p%d;o=%d", name, part, off)
}

// recvBatch records a batch of t, at most 40 items per record (the console takes short lines)
func recvBatch(proc int64, grp string, b *iggcon.PolledMessage) {
	ms := b.Messages
	for len(ms) > 0 {
		n := min(len(ms), 40)
		args := []string{"recv", topicName, grp}
		for _, m := range ms[:n] {
			args = append(args, item(string(m.Payload), b.PartitionId, int64(m.Header.Offset)))
		}
		rec(proc, "ok", args...)
		ms = ms[n:]
		if len(ms) > 0 {
			rec(proc, "invoke", "recv", topicName, grp)
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

func die(code int, f string, a ...any) {
	note(f, a...)
	os.Exit(code)
}

func atoi(s string) int64 {
	v, err := strconv.ParseInt(s, 10, 64)
	if err != nil {
		die(2, "not a number: %s", s)
	}
	return v
}

func env(k, def string) string {
	if v := os.Getenv(k); v != "" {
		return v
	}
	return def
}

func ident[T uint32 | string](v T) iggcon.Identifier {
	id, err := iggcon.NewIdentifier(v)
	if err != nil {
		die(2, "identifier %v: %v", v, err)
	}
	return id
}

var (
	sid = ident(streamName)
	tid = ident(topicName)
	xid = ident(expiryName)
	gid = ident(groupName)
)

// connect logs in; it retries until the deadline (uptime), because a starting server refuses
func connect(deadline float64) (iggcon.Client, error) {
	var last error
	for {
		cli, err := client.NewIggyClient(client.WithTcp(
			tcp.WithServerAddress(env("IGGY_ADDR", "127.0.0.1:8090")),
		))
		if err == nil {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			err = cli.Connect(ctx)
			if err == nil {
				_, err = cli.LoginUser(ctx, "iggy", "iggy")
			}
			cancel()
			if err == nil {
				return cli, nil
			}
			cli.Close()
		}
		last = err
		if uptime() >= deadline {
			return nil, last
		}
		time.Sleep(500 * time.Millisecond)
	}
}

func mustConnect(secs float64) iggcon.Client {
	cli, err := connect(uptime() + secs)
	if err != nil {
		die(1, "connect: %v", err)
	}
	return cli
}

func durability() iggcon.Durability {
	switch d := env("IGGY_DURABILITY", "replicated"); d {
	case "replicated":
		return iggcon.DurabilityReplicated
	case "persisted":
		return iggcon.DurabilityPersisted
	default:
		die(2, "IGGY_DURABILITY: replicated or persisted, not %s", d)
	}
	return ""
}

// expiry of x in seconds (IGGY_X_EXPIRY_S, 20 by default)
func expirySecs() int64 { return atoi(env("IGGY_X_EXPIRY_S", "20")) }

// msgID: the 16-byte message id of message <i> of command <cmd>, never zero
func msgID(cmd int64, i int) [16]byte {
	var id [16]byte
	binary.LittleEndian.PutUint64(id[:8], uint64(cmd)*64+uint64(i)+1)
	return id
}

func appendLine(path, line string) {
	if path == "" {
		return
	}
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	f.WriteString(line + "\n")
	f.Close()
}

func readLines(path string) []string {
	if path == "" {
		return nil
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return nil
	}
	var out []string
	for _, l := range strings.Split(string(b), "\n") {
		if l = strings.TrimSpace(l); l != "" {
			out = append(out, l)
		}
	}
	return out
}

func isExists(err error) bool {
	return err != nil && strings.Contains(strings.ToLower(err.Error()), "already exists")
}

// ---- ready, create ----

func ready() {
	cli, err := connect(uptime() + 3)
	if err != nil {
		os.Exit(1)
	}
	defer cli.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	if _, err := cli.GetStreams(ctx); err != nil {
		os.Exit(1)
	}
}

func create() {
	cli := mustConnect(60)
	defer cli.Close()
	d := durability()
	end := uptime() + 100
	step := func(what string, f func(ctx context.Context) error) {
		for {
			ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
			err := f(ctx)
			cancel()
			if err == nil || isExists(err) {
				return
			}
			if uptime() > end {
				die(1, "create %s: %v", what, err)
			}
			note("create %s: %v (again)", what, err)
			time.Sleep(time.Second)
		}
	}
	step("stream s", func(ctx context.Context) error { _, err := cli.CreateStream(ctx, streamName, nil); return err })
	step("topic t", func(ctx context.Context) error {
		n := uint32(partitions)
		never := iggcon.IggyExpiryNeverExpire
		_, err := cli.CreateTopic(ctx, sid, topicName, &iggcon.TopicOptions{
			PartitionsCount: &n, MessageExpiry: &never, Durability: &d, ConsumerOffsetDurability: &d,
		})
		return err
	})
	step("topic x", func(ctx context.Context) error {
		n := uint32(1)
		exp := iggcon.Duration(uint64(expirySecs()) * 1_000_000)
		seg := uint64(1 << 20) // the smallest segment, so that a run seals several
		_, err := cli.CreateTopic(ctx, sid, expiryName, &iggcon.TopicOptions{
			PartitionsCount: &n, MessageExpiry: &exp, SegmentSize: &seg, Durability: &d, ConsumerOffsetDurability: &d,
		})
		return err
	})
	step("group g", func(ctx context.Context) error {
		_, err := cli.CreateConsumerGroup(ctx, sid, tid, groupName)
		return err
	})
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	for _, t := range []iggcon.Identifier{tid, xid} {
		td, err := cli.GetTopic(ctx, sid, t)
		if err != nil {
			die(1, "get topic: %v", err)
		}
		note("topic %s: id %d, %d partitions, durability %s, expiry %d us", td.Name, td.Id, len(td.Partitions), d, td.MessageExpiry)
	}
}

// partitionIDs of a topic, in order
func partitionIDs(cli iggcon.Client, t iggcon.Identifier) ([]uint32, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	td, err := cli.GetTopic(ctx, sid, t)
	if err != nil {
		return nil, err
	}
	var ids []uint32
	for _, p := range td.Partitions {
		ids = append(ids, p.Id)
	}
	sort.Slice(ids, func(i, j int) bool { return ids[i] < ids[j] })
	if len(ids) == 0 {
		return nil, errors.New("the topic has no partitions")
	}
	return ids, nil
}

// ---- produce ----

func produce(id, seed int64) {
	cli := mustConnect(8)
	defer cli.Close()
	ids, err := partitionIDs(cli, tid)
	if err != nil || len(ids) != partitions {
		die(1, "produce: partitions of t: %v %v", ids, err)
	}
	part := ids[seed%partitions]
	n := int(1 + (seed/3)%3)
	var msgs []iggcon.IggyMessage
	var names []string
	for i := 0; i < n; i++ {
		name := fmt.Sprintf("m%d_%d", id, i)
		m, err := iggcon.NewIggyMessage([]byte(name), iggcon.WithID(msgID(id, i)))
		if err != nil {
			die(2, "message: %v", err)
		}
		msgs = append(msgs, m)
		names = append(names, name)
	}
	items := func(base int64, suffix string) []string {
		args := []string{"send", topicName}
		for i, nm := range names {
			off := int64(-1)
			if base >= 0 {
				off = base + int64(i)
			}
			s := item(nm, part, off)
			if len(names) > 1 {
				s += suffix
			}
			args = append(args, s)
		}
		return args
	}
	rec(id, "invoke", items(-1, "")...)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	res, err := cli.SendMessages(ctx, sid, tid, iggcon.PartitionId(part), msgs)
	if err != nil {
		// an uncertain reply, a lost connection or a refusal: the batch may be stored
		rec(id, "info", items(-1, ";s=info")...)
		die(1, "produce %v to p%d: %v", names, part, err)
	}
	base := int64(-1)
	if res != nil {
		for _, c := range res.Confirmations {
			if c.PartitionId == part {
				base = int64(c.BaseOffset)
			}
		}
	}
	rec(id, "ok", items(base, ";s=ok")...)
	event("ig.send_acked", "a send to t was acknowledged")
	note("produced %v p%d@%d", names, part, base)
}

// ---- consume ----

// commit stores a checkpoint of group g and records it: ok when the store was acknowledged
func commit(r *reader.PartitionReader, proc int64, cp iggcon.Checkpoint) error {
	arg := fmt.Sprintf("p%d@%d", cp.PartitionID(), cp.Offset())
	rec(proc, "invoke", "commit", topicName, groupName, arg)
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := r.Commit(ctx, cp); err != nil {
		rec(proc, "info", "commit", topicName, groupName, arg)
		return err
	}
	rec(proc, "ok", "commit", topicName, groupName, arg)
	if proc < 1000000 { // not the drain's stores
		event("ig.commit_acked", "a store of group g's offset was acknowledged")
	}
	return nil
}

// event writes a console event for reactions (A=<id>...): a power cut or a kill right after an
// acknowledgement
func event(id, msg string) {
	if histOut != nil {
		histOut.WriteString(fmt.Sprintf(`HYDO-ASSERT {"t":"sometimes","id":"%s","ok":true,"msg":"%s"}`+"\n", id, msg))
	}
}

func groupReader(cli iggcon.Client) (*reader.PartitionReader, error) {
	opts := iggcon.WaitPollOptions(500 * time.Millisecond)
	opts.Count = 50
	opts.Commit = iggcon.ManualCommit()
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	return reader.NewPartitionReader(ctx, cli, sid, tid, iggcon.NewGroupConsumer(gid),
		reader.WithPollOptions(opts), reader.WithPrefetchMessages(50))
}

func consume(id, seed int64) {
	max := 10 + int(seed%40)
	cli := mustConnect(8)
	defer cli.Close()
	r, err := groupReader(cli)
	if err != nil {
		die(1, "consume: join g: %v", err)
	}
	rec(id, "invoke", "sub", topicName, groupName, "from=start")
	rec(id, "ok", "sub", topicName, groupName, "from=start")
	last := map[uint32]iggcon.Checkpoint{}
	got := 0
	end := uptime() + 6
	for got < max && uptime() < end {
		ctx, cancel := context.WithTimeout(context.Background(), 1500*time.Millisecond)
		rec(id, "invoke", "recv", topicName, groupName)
		b, err := r.Next(ctx)
		cancel()
		if err != nil || b == nil || len(b.Messages) == 0 {
			rec(id, "ok", "recv", topicName, groupName)
			if err != nil && !errors.Is(err, context.DeadlineExceeded) {
				note("consume: next: %v", err)
			}
			continue
		}
		recvBatch(id, groupName, b)
		if cp, ok := b.Checkpoint(); ok {
			last[cp.PartitionID()] = cp
		}
		got += len(b.Messages)
	}
	var keys []uint32
	for p := range last {
		keys = append(keys, p)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i] < keys[j] })
	var cerr error
	for _, p := range keys {
		if err := commit(r, id, last[p]); err != nil && cerr == nil {
			cerr = err
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := r.Close(ctx); err != nil {
		note("consume: close: %v", err)
	}
	if cerr != nil {
		die(1, "consume: read %d, store: %v", got, cerr)
	}
	note("consumed %d from %v", got, keys)
}

// ---- drain ----

// readAll reads every partition of t from offset 0 with a standalone consumer that stores nothing
// (the group -), until the partitions' current offsets hold still and nothing new arrives for a
// quiet window. An error or a refusal is "not yet".
func drainAll(proc int64, deadline float64) bool {
	const quiet = 8.0
	cli, err := connect(deadline)
	if err != nil {
		note("drain -: connect: %v", err)
		return false
	}
	defer cli.Close()
	next := map[uint32]uint64{}
	read := 0
	lastData := uptime()
	stable, stableSince := "", 0.0
	why := "no partitions yet"
	consumer := iggcon.NewSingleConsumer(ident(uint32(9001)))
	opts := iggcon.DefaultPollOptions()
	opts.Count = 200
	opts.Commit = iggcon.ManualCommit()
	for uptime() < deadline {
		ids, err := partitionIDs(cli, tid)
		if err != nil {
			why, lastData, stable = "topic: "+err.Error(), uptime(), ""
			time.Sleep(time.Second)
			continue
		}
		any := false
		for _, p := range ids {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			rec(proc, "invoke", "recv", topicName, "-")
			b, err := cli.PollMessages(ctx, sid, tid, p, consumer, iggcon.OffsetPollingStrategy(next[p]), opts)
			cancel()
			if err != nil {
				rec(proc, "info", "recv", topicName, "-")
				why, lastData, stable = fmt.Sprintf("poll p%d: %v", p, err), uptime(), ""
				continue
			}
			if len(b.Messages) == 0 {
				rec(proc, "ok", "recv", topicName, "-")
				continue
			}
			recvBatch(proc, "-", b)
			read += len(b.Messages)
			next[p] = b.Messages[len(b.Messages)-1].Header.Offset + 1
			any = true
		}
		if any {
			lastData = uptime()
			continue
		}
		ends, err := currentOffsets(cli)
		if err != nil {
			why, lastData, stable = "current offsets: "+err.Error(), uptime(), ""
			time.Sleep(time.Second)
			continue
		}
		if ends != stable {
			stable, stableSince = ends, uptime()
		}
		if uptime()-stableSince >= quiet && uptime()-lastData >= quiet {
			rec(proc, "ok", "drained", topicName, "-")
			note("drained -: read %d, current offsets %s", read, ends)
			return true
		}
		why = "waiting for a quiet window"
		time.Sleep(time.Second)
	}
	note("drain -: no end within the deadline (read %d): %s", read, why)
	return false
}

func currentOffsets(cli iggcon.Client) (string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	td, err := cli.GetTopic(ctx, sid, tid)
	if err != nil {
		return "", err
	}
	var parts []string
	for _, p := range td.Partitions {
		parts = append(parts, fmt.Sprintf("p%d:%d/%d", p.Id, p.CurrentOffset, p.MessagesCount))
	}
	sort.Strings(parts)
	return strings.Join(parts, " "), nil
}

// drainGroup reads group g from its stored offsets and stores what it reads, until the reader
// holds every partition and nothing arrives for a quiet window.
func drainGroup(proc int64, deadline float64) bool {
	const quiet = 10.0
	read := 0
	why := "not started"
	for uptime() < deadline {
		cli, err := connect(deadline)
		if err != nil {
			why = "connect: " + err.Error()
			break
		}
		r, err := groupReader(cli)
		if err != nil {
			why = "join g: " + err.Error()
			cli.Close()
			time.Sleep(time.Second)
			continue
		}
		rec(proc, "invoke", "sub", topicName, groupName, "from=start")
		rec(proc, "ok", "sub", topicName, groupName, "from=start")
		lastData := uptime()
		failed := false
		for uptime() < deadline {
			ctx, cancel := context.WithTimeout(context.Background(), time.Second)
			rec(proc, "invoke", "recv", topicName, groupName)
			b, err := r.Next(ctx)
			cancel()
			if err != nil || b == nil || len(b.Messages) == 0 {
				rec(proc, "ok", "recv", topicName, groupName)
				if err != nil && !errors.Is(err, context.DeadlineExceeded) {
					why, lastData = "next: "+err.Error(), uptime()
					var re *reader.ReadError
					if !errors.As(err, &re) {
						failed = true // the reader stopped: join again
						break
					}
				}
				if len(r.Partitions()) < partitions {
					why, lastData = fmt.Sprintf("the member holds %v of %d partitions", r.Partitions(), partitions), uptime()
					continue
				}
				if uptime()-lastData >= quiet {
					ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
					r.Close(ctx)
					cancel()
					cli.Close()
					rec(proc, "ok", "drained", topicName, groupName)
					note("drained %s: read %d", groupName, read)
					return true
				}
				continue
			}
			recvBatch(proc, groupName, b)
			read += len(b.Messages)
			lastData = uptime()
			if cp, ok := b.Checkpoint(); ok {
				if err := commit(r, proc, cp); err != nil {
					why = "store: " + err.Error()
				}
			}
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		r.Close(ctx)
		cancel()
		cli.Close()
		if !failed {
			break
		}
		time.Sleep(time.Second)
	}
	note("drain %s: no end within the deadline (read %d): %s", groupName, read, why)
	return false
}

func drain() {
	start := uptime()
	ok1 := drainAll(1000001, start+120)
	ok2 := drainGroup(1000002, start+230)
	if !ok1 || !ok2 {
		os.Exit(1)
	}
}

// ---- expire: the topic x ----

// offsetNow is the wall clock and its offset from the uptime, in us: the offset changes only with
// a clock step and with a new boot
func offsetNow() (wall, off int64) {
	up := int64(uptime() * 1e6)
	wall = time.Now().UnixMicro()
	return wall, wall - up
}

// bootID names this boot of the guest
func bootID() string {
	b, err := os.ReadFile("/proc/sys/kernel/random/boot_id")
	if err != nil {
		return "-"
	}
	return strings.ReplaceAll(strings.TrimSpace(string(b)), "-", "")
}

// expire sends 16 messages of 8 KiB to x in one batch. An acknowledged batch appends one line per
// message to $HYDO_ACKED: x:<name>:<t0>:<t1>:<o1>:<boot>, the wall clock before the send and after
// the answer, the wall clock's offset from the uptime after the answer (all in us), and the boot.
func expire(id, seed int64) {
	cli := mustConnect(8)
	defer cli.Close()
	ids, err := partitionIDs(cli, xid)
	if err != nil {
		die(1, "expire: partitions of x: %v", err)
	}
	pad := strings.Repeat(string(rune('a'+seed%26)), 8192)
	var msgs []iggcon.IggyMessage
	var names []string
	for i := 0; i < 16; i++ {
		name := fmt.Sprintf("x%d_%d", id, i)
		m, err := iggcon.NewIggyMessage([]byte(name+" "+pad), iggcon.WithID(msgID(id, i)))
		if err != nil {
			die(2, "message: %v", err)
		}
		msgs = append(msgs, m)
		names = append(names, name)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	t0 := time.Now().UnixMicro()
	_, err = cli.SendMessages(ctx, sid, xid, iggcon.PartitionId(ids[0]), msgs)
	t1, o1 := offsetNow()
	if err != nil {
		die(1, "expire %s..: %v", names[0], err)
	}
	for _, n := range names {
		appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("x:%s:%d:%d:%d:%s", n, t0, t1, o1, bootID()))
	}
	note("expire: sent %s..%s at %d", names[0], names[len(names)-1], t0)
}

type xsent struct{ t0, t1 int64 }

type xmsg struct {
	name string
	off  uint64
	ts   uint64
}

// xView is what a judgment of x reads: the acknowledged messages, the clock records of the
// commands (see judgeEarly), the present messages and the segment files.
type xView struct {
	acked   map[string]xsent
	maxOff  int64 // the largest offset of the wall clock from the uptime recorded in this boot
	maxPrev int64 // the latest wall clock recorded in an earlier boot
	present []xmsg
	segs    []uint64
}

func xAcked() (acked map[string]xsent, maxOff, maxPrev int64) {
	acked = map[string]xsent{}
	maxOff, maxPrev = int64(-1<<62), int64(-1<<62)
	boot := bootID()
	clock := func(wall, off int64, b string) {
		if b == boot {
			maxOff = max(maxOff, off)
		} else {
			maxPrev = max(maxPrev, wall)
		}
	}
	for _, l := range readLines(os.Getenv("HYDO_ACKED")) {
		f := strings.Split(l, ":")
		switch {
		case len(f) == 6 && f[0] == "x":
			t0, _ := strconv.ParseInt(f[2], 10, 64)
			t1, _ := strconv.ParseInt(f[3], 10, 64)
			o1, _ := strconv.ParseInt(f[4], 10, 64)
			acked[f[1]] = xsent{t0, t1}
			clock(t1, o1, f[5])
		case len(f) == 4 && f[0] == "xp":
			w, _ := strconv.ParseInt(f[1], 10, 64)
			o, _ := strconv.ParseInt(f[2], 10, 64)
			clock(w, o, f[3])
		}
	}
	return acked, maxOff, maxPrev
}

// readX reads x from offset 0 and lists its segment files (the last one is the active segment)
func readX(cli iggcon.Client) ([]xmsg, []uint64, error) {
	ids, err := partitionIDs(cli, xid)
	if err != nil {
		return nil, nil, err
	}
	part := ids[0]
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	td, err := cli.GetTopic(ctx, sid, xid)
	var sd *iggcon.StreamDetails
	if err == nil {
		sd, err = cli.GetStream(ctx, sid)
	}
	cancel()
	if err != nil {
		return nil, nil, err
	}
	consumer := iggcon.NewSingleConsumer(ident(uint32(9002)))
	opts := iggcon.DefaultPollOptions()
	opts.Count = 100
	opts.Commit = iggcon.ManualCommit()
	var out []xmsg
	next := uint64(0)
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		b, err := cli.PollMessages(ctx, sid, xid, part, consumer, iggcon.OffsetPollingStrategy(next), opts)
		cancel()
		if err != nil {
			return nil, nil, err
		}
		if len(b.Messages) == 0 {
			break
		}
		for _, m := range b.Messages {
			name, _, _ := strings.Cut(string(m.Payload), " ")
			out = append(out, xmsg{name, m.Header.Offset, m.Header.Timestamp})
		}
		next = b.Messages[len(b.Messages)-1].Header.Offset + 1
	}
	dir := filepath.Join(env("IGGY_DATA", "/data/vol/iggy/data"), "streams", fmt.Sprint(sd.Id), "topics", fmt.Sprint(td.Id), "partitions", fmt.Sprint(part))
	es, _ := os.ReadDir(dir)
	var segs []uint64
	for _, e := range es {
		if n, ok := strings.CutSuffix(e.Name(), ".log"); ok {
			if v, err := strconv.ParseUint(n, 10, 64); err == nil {
				segs = append(segs, v)
			}
		}
	}
	sort.Slice(segs, func(i, j int) bool { return segs[i] < segs[j] })
	return out, segs, nil
}

// judgeEarly lists the acknowledged messages of x that are gone before their expiry. Iggy deletes
// a sealed segment once its newest message's timestamp plus the expiry is not after the wall
// clock (core/partitions/src/segment.rs, is_expired), and a message's timestamp is not before the
// wall clock at its send. So a message sent at t0 may be gone only once the wall clock reached
// t0 + expiry. The clock at the deletion is not known, so the judgment takes the latest clock the
// run can show. In this boot that is the uptime now plus the largest offset of the wall clock from
// the uptime that a command recorded: the clock as if no step back had happened, with every step
// forward that a command saw. An earlier boot (before a power cut) shows its latest recorded wall
// clock plus 10 s, the most between a record and the cut. 2 s of slack cover a step forward that
// no command saw before the cleaner ran.
func judgeEarly(v xView) (early []string, judged, gone int, latest int64) {
	exp := expirySecs() * 1_000_000
	const slack = 2_000_000
	_, off := offsetNow()
	latest = max(int64(uptime()*1e6)+max(v.maxOff, off), v.maxPrev+10_000_000)
	have := map[string]bool{}
	for _, m := range v.present {
		have[m.name] = true
	}
	var names []string
	for n := range v.acked {
		names = append(names, n)
	}
	sort.Strings(names)
	for _, n := range names {
		s := v.acked[n]
		if !have[n] {
			gone++
		}
		if s.t1 < s.t0 {
			continue // the clock stepped back during the send: its timestamp is not known
		}
		judged++
		if !have[n] && s.t0+exp > latest+slack {
			early = append(early, fmt.Sprintf("%s sent at %d is gone at %d, %d s before its expiry of %d s",
				n, s.t0, latest, (s.t0+exp-latest)/1_000_000, exp/1_000_000))
		}
	}
	return early, judged, gone, latest
}

// judgeKept lists the sealed segments that Iggy should have deleted more than the grace ago. The
// cleaner deletes "the oldest sealed segments": it walks them from the oldest and stops at the
// first one that has not expired (leading_expired_end), so only that leading run counts. A
// segment counts as present when the read returned a message of it.
func judgeKept(v xView, now int64) []string {
	exp := expirySecs() * 1_000_000
	const grace = 30_000_000
	var kept []string
	for i := 0; i+1 < len(v.segs); i++ { // the sealed segments, oldest first
		var newest uint64
		n := 0
		for _, m := range v.present {
			if m.off >= v.segs[i] && m.off < v.segs[i+1] {
				newest = max(newest, m.ts)
				n++
			}
		}
		if n == 0 {
			continue
		}
		if int64(newest)+exp > now {
			break // not expired: the cleaner stops here
		}
		if int64(newest)+exp+grace <= now {
			kept = append(kept, fmt.Sprintf("the sealed segment from offset %d holds %d messages; its newest message has timestamp %d, %d s past its expiry",
				v.segs[i], n, newest, (now-int64(newest)-exp)/1_000_000))
		}
	}
	return kept
}

// xprobe reads x during the run and fails (exit 100, a violation of the composer) when an
// acknowledged message is gone before its expiry. It records its clock ("xp:<wall>:<offset>:<boot>").
func xprobe() {
	cli, err := connect(uptime() + 5)
	if err != nil {
		os.Exit(1) // the server is down: not a judgment
	}
	defer cli.Close()
	acked, maxOff, maxPrev := xAcked()
	present, segs, err := readX(cli)
	if err != nil {
		note("xprobe: read x: %v", err)
		os.Exit(1)
	}
	wall, off := offsetNow()
	appendLine(os.Getenv("HYDO_ACKED"), fmt.Sprintf("xp:%d:%d:%s", wall, off, bootID()))
	early, judged, gone, _ := judgeEarly(xView{acked, maxOff, maxPrev, present, segs})
	if len(early) > 0 {
		for _, e := range early[:min(len(early), 5)] {
			note("xprobe: gone before its expiry: %s", e)
		}
		note("xprobe: %d of %d acknowledged messages of x are gone before their expiry", len(early), judged)
		os.Exit(100)
	}
	fmt.Printf("xprobe: %d judged, %d gone, segments %v\n", judged, gone, segs)
}

// checkExpiry is the final check of x: no acknowledged message gone before its expiry, and no
// sealed segment of the leading expired run kept more than 30 s past its expiry. The cleaner runs
// every data_maintenance.messages.interval (5 s here), so the check waits up to 30 s of uptime
// for a late deletion. It exits 2 (cannot check) when the server does not answer.
func checkExpiry() {
	acked, maxOff, maxPrev := xAcked()
	start := uptime()
	cli, err := connect(start + 15)
	if err != nil {
		die(2, "check-expiry: connect: %v", err)
	}
	defer cli.Close()
	var v xView
	var kept []string
	for {
		present, segs, err := readX(cli)
		if err != nil {
			if uptime()-start > 40 {
				die(2, "check-expiry: read x: %v", err)
			}
			time.Sleep(time.Second)
			continue
		}
		v = xView{acked, maxOff, maxPrev, present, segs}
		kept = judgeKept(v, time.Now().UnixMicro())
		if len(kept) == 0 || uptime()-start > 40 {
			break
		}
		time.Sleep(2 * time.Second)
	}
	early, judged, gone, latest := judgeEarly(v)
	// to the console too: an OK run keeps a check's output in the guest
	note("x: %d acknowledged messages judged, %d of them gone, %d messages present, segment files %v, latest clock %d",
		judged, gone, len(v.present), v.segs, latest)
	for _, k := range kept[:min(len(kept), 5)] {
		note("x: kept: %s", k)
	}
	for _, e := range early[:min(len(early), 5)] {
		note("x: gone before its expiry: %s", e)
	}
	switch {
	case len(early) > 0:
		fmt.Printf("%d acknowledged messages of x are gone before their expiry\n", len(early))
		os.Exit(1)
	case len(kept) > 0:
		fmt.Printf("%d sealed segments of x stay 30 s past their expiry\n", len(kept))
		os.Exit(1)
	}
	fmt.Printf("x: no message gone before its expiry, no sealed segment kept past it\n")
}

// ---- offsets, topic ----

func offsets() {
	cli := mustConnect(10)
	defer cli.Close()
	ids, err := partitionIDs(cli, tid)
	if err != nil {
		die(2, "offsets: %v", err)
	}
	for _, p := range ids {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		pp := p
		o, err := cli.GetConsumerOffset(ctx, iggcon.NewGroupConsumer(gid), sid, tid, &pp)
		cancel()
		if err != nil || o == nil {
			fmt.Printf("p%d: %v\n", p, err)
			continue
		}
		fmt.Printf("p%d stored %d current %d\n", p, o.StoredOffset, o.CurrentOffset)
	}
}

func topic(name string) {
	cli := mustConnect(10)
	defer cli.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	td, err := cli.GetTopic(ctx, sid, ident(name))
	if err != nil {
		die(2, "topic: %v", err)
	}
	fmt.Printf("topic %s id %d messages %d expiry %d options %v\n", td.Name, td.Id, td.MessagesCount, td.MessageExpiry, td.Options)
	for _, p := range td.Partitions {
		fmt.Printf("p%d segments %d current %d messages %d size %d\n", p.Id, p.SegmentsCount, p.CurrentOffset, p.MessagesCount, p.SizeBytes)
	}
}

func main() {
	if os.Getenv("HYDO_HIST_OUT") != "off" {
		path := env("HYDO_HIST_OUT", "/dev/console")
		if f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644); err == nil {
			histOut = f
		}
	}
	if len(os.Args) < 2 {
		die(2, "usage: iggyc ready|create|produce|consume|expire|drain|xprobe|check-expiry|offsets|topic ...")
	}
	arg := func(i int) int64 {
		if len(os.Args) <= i {
			die(2, "missing argument")
		}
		return atoi(os.Args[i])
	}
	switch os.Args[1] {
	case "ready":
		ready()
	case "create":
		create()
	case "produce":
		produce(arg(2), arg(3))
	case "consume":
		consume(arg(2), arg(3))
	case "expire":
		expire(arg(2), arg(3))
	case "drain":
		drain()
	case "xprobe":
		xprobe()
	case "check-expiry":
		checkExpiry()
	case "offsets":
		offsets()
	case "topic":
		if len(os.Args) < 3 {
			die(2, "topic <name>")
		}
		topic(os.Args[2])
	default:
		die(2, "unknown command %s", os.Args[1])
	}
}
