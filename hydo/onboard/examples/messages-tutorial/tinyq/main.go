// tinyq: a tiny message queue for the tutorial in docs/ONBOARD.md. It is the system under test
// and its client in one program:
//
//	tinyq serve DIR PORT     the queue: one channel, one consumer group, a log file in DIR
//	tinyq ping PORT          exit 0 when the queue answers (the readiness check)
//	tinyq send PROC ID KEY   send message ID with ordering key KEY, as client process PROC
//	tinyq take PROC          receive the oldest message that is not acknowledged, and acknowledge it
//	tinyq drain PROC         take until the queue is empty, then record that the drain ended
//
// The queue keeps every accepted message and every acknowledgement in an append-only log and
// syncs it to disk before it answers, unless /tmp/tinyq-sync says "never" (the tutorial's knob).
// A send repeats a message id only as a retry: the queue stores an id once. A receive hands out
// the oldest message that is not acknowledged, so a message comes back until it is acknowledged.
//
// The client writes what it did and what the queue answered as hydo history records on the guest
// console, one before each request and one after the answer (`hydo guide messages`):
//
//	HYDO-HIST queue <proc> invoke <ns> send orders m7;k=g1
//	HYDO-HIST queue <proc> ok <ns> send orders m7;k=g1;o=7
package main

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"
	"unsafe"
)

const channel = "orders"

func main() {
	if len(os.Args) < 3 {
		fmt.Fprintln(os.Stderr, "usage: tinyq serve DIR PORT | ping PORT | send PROC ID KEY | take PROC | drain PROC")
		os.Exit(2)
	}
	a := os.Args[2:]
	switch os.Args[1] {
	case "serve":
		serve(a[0], a[1])
	case "ping":
		_, err := ask("PING")
		exit(err == nil)
	case "send":
		exit(send(a[0], a[1], a[2]))
	case "take":
		_, ok := take(a[0])
		exit(ok)
	case "drain":
		exit(drain(a[0]))
	default:
		fmt.Fprintln(os.Stderr, "tinyq: unknown command", os.Args[1])
		os.Exit(2)
	}
}

func exit(ok bool) {
	if ok {
		os.Exit(0)
	}
	os.Exit(1)
}

// ---------------------------------------------------------------- the queue

type msg struct {
	off      int
	id, key  string
	acked    bool
	attempts int
}

type queue struct {
	mu   sync.Mutex
	log  *os.File
	msgs []*msg
	byID map[string]*msg
}

// replay reads the log: `S <offset> <id> <key>` for a message, `A <offset>` for an
// acknowledgement. A torn last line (a power cut in the middle of a write) is ignored.
func (q *queue) replay(path string) {
	f, err := os.Open(path)
	if err != nil {
		return
	}
	defer f.Close()
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		w := strings.Fields(sc.Text())
		if len(w) == 4 && w[0] == "S" {
			m := &msg{off: len(q.msgs) + 1, id: w[2], key: w[3]}
			q.msgs = append(q.msgs, m)
			q.byID[m.id] = m
		} else if len(w) == 2 && w[0] == "A" {
			if n, err := strconv.Atoi(w[1]); err == nil && n >= 1 && n <= len(q.msgs) {
				q.msgs[n-1].acked = true
			}
		}
	}
}

// write appends one line to the log and syncs it, unless the knob says "never".
func (q *queue) write(line string) error {
	if _, err := q.log.WriteString(line + "\n"); err != nil {
		return err
	}
	if b, _ := os.ReadFile("/tmp/tinyq-sync"); strings.TrimSpace(string(b)) == "never" {
		return nil
	}
	return q.log.Sync()
}

func (q *queue) handle(req string) string {
	q.mu.Lock()
	defer q.mu.Unlock()
	w := strings.Fields(req)
	switch {
	case len(w) == 1 && w[0] == "PING":
		return "PONG"
	case len(w) == 3 && w[0] == "SEND":
		if m, ok := q.byID[w[1]]; ok {
			return fmt.Sprintf("OK %d", m.off) // a retry: the message is stored once
		}
		m := &msg{off: len(q.msgs) + 1, id: w[1], key: w[2]}
		if err := q.write(fmt.Sprintf("S %d %s %s", m.off, m.id, m.key)); err != nil {
			return "ERR " + err.Error()
		}
		q.msgs = append(q.msgs, m)
		q.byID[m.id] = m
		return fmt.Sprintf("OK %d", m.off)
	case len(w) == 1 && w[0] == "RECV":
		for _, m := range q.msgs {
			if !m.acked {
				m.attempts++
				return fmt.Sprintf("MSG %d %s %s %d", m.off, m.id, m.key, m.attempts)
			}
		}
		return "EMPTY"
	case len(w) == 2 && w[0] == "ACK":
		n, err := strconv.Atoi(w[1])
		if err != nil || n < 1 || n > len(q.msgs) {
			return "ERR no such message"
		}
		if !q.msgs[n-1].acked {
			if err := q.write(fmt.Sprintf("A %d", n)); err != nil {
				return "ERR " + err.Error()
			}
			q.msgs[n-1].acked = true
		}
		return "OK"
	}
	return "ERR bad request"
}

func serve(dir, port string) {
	path := dir + "/log"
	q := &queue{byID: map[string]*msg{}}
	q.replay(path)
	f, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tinyq:", err)
		os.Exit(1)
	}
	q.log = f
	l, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		fmt.Fprintln(os.Stderr, "tinyq:", err)
		os.Exit(1)
	}
	fmt.Printf("tinyq: %d messages in the log, listening on %s\n", len(q.msgs), port)
	for {
		c, err := l.Accept()
		if err != nil {
			continue
		}
		go func(c net.Conn) {
			defer c.Close()
			line, err := bufio.NewReader(c).ReadString('\n')
			if err != nil {
				return
			}
			fmt.Fprintln(c, q.handle(strings.TrimSpace(line)))
		}(c)
	}
}

// ---------------------------------------------------------------- the client

const port = "7700"

// errNotSent: the request never reached the queue, so it had no effect.
type errNotSent struct{ error }

// ask sends one request and returns the answer.
func ask(req string) (string, error) {
	c, err := net.DialTimeout("tcp", "127.0.0.1:"+port, 2*time.Second)
	if err != nil {
		return "", errNotSent{err}
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(3 * time.Second))
	if _, err := fmt.Fprintln(c, req); err != nil {
		return "", err
	}
	line, err := bufio.NewReader(c).ReadString('\n')
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(line), nil
}

// monoNS is CLOCK_MONOTONIC in ns, the time hydo's history records carry.
func monoNS() int64 {
	var ts syscall.Timespec
	if _, _, e := syscall.Syscall(syscall.SYS_CLOCK_GETTIME, 1, uintptr(unsafe.Pointer(&ts)), 0); e != 0 {
		return 0
	}
	return ts.Nano()
}

var console, _ = os.OpenFile("/dev/console", os.O_WRONLY|os.O_APPEND, 0)

// record writes one history record in one write() to the guest console.
func record(proc, typ, f string, args ...string) {
	line := fmt.Sprintf("HYDO-HIST queue %s %s %d %s %s\n", proc, typ, monoNS(), f, strings.Join(args, " "))
	if console != nil {
		console.WriteString(line)
	}
}

// outcome is the record type of a failed request: fail when it never reached the queue, info
// when the queue may have acted on it.
func outcome(err error) string {
	if _, ok := err.(errNotSent); ok {
		return "fail"
	}
	return "info"
}

func send(proc, id, key string) bool {
	item := "m" + id + ";k=" + key
	for try := 0; try < 2; try++ { // one retry of an unknown outcome, with the same message id
		record(proc, "invoke", "send", channel, item)
		ans, err := ask("SEND m" + id + " " + key)
		if err == nil && strings.HasPrefix(ans, "OK ") {
			record(proc, "ok", "send", channel, item+";o="+ans[3:])
			return true
		}
		if err == nil {
			record(proc, "fail", "send", channel, item) // the queue refused it
			return false
		}
		record(proc, outcome(err), "send", channel, item)
		if outcome(err) == "fail" {
			return false
		}
		time.Sleep(500 * time.Millisecond)
	}
	return false
}

// take receives one message and acknowledges it. It returns whether the queue was empty, and
// whether every request was answered.
func take(proc string) (empty bool, ok bool) {
	record(proc, "invoke", "recv", channel, "-")
	ans, err := ask("RECV")
	if err != nil {
		record(proc, outcome(err), "recv", channel, "-")
		return false, false
	}
	if ans == "EMPTY" {
		record(proc, "ok", "recv", channel, "-")
		return true, true
	}
	w := strings.Fields(ans) // MSG <offset> <id> <key> <attempt>
	if len(w) != 5 || w[0] != "MSG" {
		record(proc, "fail", "recv", channel, "-")
		return false, false
	}
	record(proc, "ok", "recv", channel, "-", fmt.Sprintf("%s;k=%s;o=%s;n=%s", w[2], w[3], w[1], w[4]))
	record(proc, "invoke", "ack", channel, "-", w[2])
	ans, err = ask("ACK " + w[1])
	switch {
	case err != nil:
		record(proc, outcome(err), "ack", channel, "-", w[2])
		return false, false
	case ans != "OK":
		record(proc, "fail", "ack", channel, "-", w[2])
		return false, false
	}
	record(proc, "ok", "ack", channel, "-", w[2])
	return false, true
}

// drain takes until the queue is empty (it waits for a queue that is restarting), then writes
// the record that tells hydo the drain read everything.
func drain(proc string) bool {
	deadline := time.Now().Add(60 * time.Second)
	for time.Now().Before(deadline) {
		empty, ok := take(proc)
		if empty {
			record(proc, "ok", "drained", channel, "-")
			return true
		}
		if !ok {
			time.Sleep(500 * time.Millisecond)
		}
	}
	return false
}
