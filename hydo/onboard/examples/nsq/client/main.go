// nsqc: the test client of the NSQ example, for hydo's workload. It records every operation as a
// HYDO-HIST record of the history "jobs" and prints events for reactions.
//
//	nsqc pub <id>          publish one message over HTTP (nsqd:4151/pub)
//	nsqc consume <proc>    take one message over TCP, hold it 2.5 s, then FIN it
//	nsqc drain             take every message left, then read back each id's final state
//	nsqc probe             publish one probe message, retrying until 60 s pass
//	nsqc ready <url>       exit 0 when GET <url> answers 200 (the readiness checks)
package main

import (
	"bufio"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/kakoc/hydo/sdk/go/hydohist"
	"golang.org/x/sys/unix"
)

const (
	topic   = "jobs"
	channel = "work"
	httpURL = "http://nsqd:4151"
	tcpAddr = "nsqd:4150"
	delLog  = "/data/client/delivered"
	// the messages whose FIN got no answer in time
	finUnknown = "/data/client/fin-unknown"
)

var h = hydohist.New("jobs")

func event(id string, msg string) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	fmt.Fprintf(f, "HYDO-ASSERT {\"t\":\"sometimes\",\"id\":%q,\"ok\":true,\"msg\":%q}\n", id, msg)
	f.Close()
}

func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: nsqc pub <id> | consume <proc> | drain | probe | ready <url>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "pub":
		os.Exit(pub(os.Args[2]))
	case "consume":
		os.Exit(consume(os.Args[2]))
	case "drain":
		os.Exit(drain())
	case "probe":
		os.Exit(probe())
	case "ready":
		os.Exit(ready(os.Args[2]))
	}
	os.Exit(2)
}

// ready asks a service's /ping over HTTP, so that a service counts as ready only when it serves.
func ready(url string) int {
	c := http.Client{Timeout: 2 * time.Second}
	resp, err := c.Get(url)
	if err != nil {
		return 1
	}
	defer resp.Body.Close()
	if resp.StatusCode != 200 {
		return 1
	}
	return 0
}

func procOf(id string) int {
	var n int
	fmt.Sscanf(strings.TrimLeft(id, "mp"), "%d", &n)
	return n
}

func publish(body string, timeout time.Duration) (int, error) {
	c := http.Client{Timeout: timeout}
	resp, err := c.Post(httpURL+"/pub?topic="+topic, "text/plain", strings.NewReader(body))
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	b, _ := io.ReadAll(resp.Body)
	if resp.StatusCode != 200 || strings.TrimSpace(string(b)) != "OK" {
		return resp.StatusCode, fmt.Errorf("%d %s", resp.StatusCode, strings.TrimSpace(string(b)))
	}
	return 200, nil
}

func outcome(id, what string) {
	if p := os.Getenv("HYDO_OUTCOMES"); p != "" {
		if f, err := os.OpenFile(p, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644); err == nil {
			fmt.Fprintf(f, "%s %s\n", id, what)
			f.Close()
		}
	}
}

func pub(id string) int {
	op := h.Invoke(procOf(id), "send", topic, id)
	code, err := publish(id, 5*time.Second)
	if err == nil {
		op.Ok(topic, id)
		outcome(id, "ack")
		event("nsq.pub_acked", "nsqd answered OK to a publish")
		return 0
	}
	if code == 0 && isRefused(err) {
		op.Fail()
		outcome(id, "failed")
		return 1
	}
	op.Info()
	outcome(id, "unknown")
	fmt.Println("pub", id, err)
	return 1
}

func isRefused(err error) bool {
	return strings.Contains(err.Error(), "connection refused") || strings.Contains(err.Error(), "no such host")
}

// conn speaks NSQ's TCP protocol V2.
type conn struct {
	c net.Conn
	r *bufio.Reader
}

func dial() (*conn, error) {
	c, err := net.DialTimeout("tcp", tcpAddr, 3*time.Second)
	if err != nil {
		return nil, err
	}
	cn := &conn{c: c, r: bufio.NewReader(c)}
	if _, err := c.Write([]byte("  V2")); err != nil {
		c.Close()
		return nil, err
	}
	if err := cn.cmd("SUB " + topic + " " + channel); err != nil {
		c.Close()
		return nil, err
	}
	typ, data, err := cn.frame(5 * time.Second)
	if err != nil || typ != 0 || string(data) != "OK" {
		c.Close()
		return nil, fmt.Errorf("SUB: %v %d %q", err, typ, data)
	}
	if err := cn.cmd("RDY 1"); err != nil {
		c.Close()
		return nil, err
	}
	return cn, nil
}

func (cn *conn) cmd(s string) error {
	cn.c.SetWriteDeadline(time.Now().Add(3 * time.Second))
	_, err := cn.c.Write([]byte(s + "\n"))
	return err
}

func (cn *conn) frame(timeout time.Duration) (int32, []byte, error) {
	cn.c.SetReadDeadline(time.Now().Add(timeout))
	var size int32
	if err := binary.Read(cn.r, binary.BigEndian, &size); err != nil {
		return 0, nil, err
	}
	buf := make([]byte, size)
	if _, err := io.ReadFull(cn.r, buf); err != nil {
		return 0, nil, err
	}
	typ := int32(binary.BigEndian.Uint32(buf[:4]))
	return typ, buf[4:], nil
}

type msg struct {
	id       string // the NSQ message id
	attempts uint16
	body     string
}

// next waits up to timeout for a message, answering heartbeats.
func (cn *conn) next(timeout time.Duration) (*msg, error) {
	end := time.Now().Add(timeout)
	for {
		left := time.Until(end)
		if left <= 0 {
			return nil, nil
		}
		typ, data, err := cn.frame(left)
		if err != nil {
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				return nil, nil
			}
			return nil, err
		}
		switch typ {
		case 0:
			if string(data) == "_heartbeat_" {
				cn.cmd("NOP")
			}
		case 1:
			return nil, fmt.Errorf("error frame: %s", data)
		case 2:
			if len(data) < 26 {
				return nil, fmt.Errorf("short message")
			}
			return &msg{attempts: binary.BigEndian.Uint16(data[8:10]), id: string(data[10:26]), body: string(data[26:])}, nil
		}
	}
}

// fin acknowledges a message and waits for the server to have processed it:
// FIN has no answer, so a CLS after it must come back as CLOSE_WAIT.
func (cn *conn) fin(id string) error {
	if err := cn.cmd("FIN " + id); err != nil {
		return err
	}
	if err := cn.cmd("CLS"); err != nil {
		return err
	}
	for {
		typ, data, err := cn.frame(5 * time.Second)
		if err != nil {
			return err
		}
		if typ == 1 {
			return fmt.Errorf("%s", data)
		}
		if typ == 0 && string(data) == "CLOSE_WAIT" {
			return nil
		}
	}
}

func item(m *msg) string {
	return fmt.Sprintf("%s;h=%s;n=%d", m.body, m.id, m.attempts)
}

func remember(id string) { appendLine(delLog, id) }

// appendLine appends one line to a file of the data disk and syncs it.
func appendLine(path, line string) {
	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return
	}
	fmt.Fprintln(f, line)
	f.Sync()
	f.Close()
}

// lines reads a file of appendLine's lines as a set.
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

func consume(p string) int {
	var proc int
	fmt.Sscanf(p, "%d", &proc)
	proc += 50000
	op := h.Invoke(proc, "recv", topic, channel)
	cn, err := dial()
	if err != nil {
		op.Info()
		return 1
	}
	defer cn.c.Close()
	m, err := cn.next(3 * time.Second)
	if err != nil {
		op.Info()
		return 1
	}
	if m == nil {
		op.Ok(topic, channel)
		return 0
	}
	if strings.HasPrefix(m.body, "probe") {
		op.Ok(topic, channel)
		cn.fin(m.id)
		return 0
	}
	op.Ok(topic, channel, item(m))
	event("nsq.msg_received", "a consumer holds a message in flight")
	time.Sleep(2500 * time.Millisecond) // the work
	a := h.Invoke(proc, "ack", topic, channel, item(m))
	if err := cn.fin(m.id); err != nil {
		if strings.Contains(err.Error(), "E_FIN_FAILED") {
			a.Fail()
		} else {
			a.Info()
			// No answer in time (nsqd paused or slow): nsqd may still take the FIN, so
			// the message's final state is unknown. A closed connection is no such case.
			if ne, ok := err.(net.Error); ok && ne.Timeout() {
				appendLine(finUnknown, m.body)
			}
		}
		return 1
	}
	a.Ok(topic, channel, item(m))
	remember(m.body) // the effect: the message was processed and acknowledged
	return 0
}

func uptime() float64 {
	b, _ := os.ReadFile("/proc/uptime")
	var u float64
	fmt.Sscanf(string(b), "%f", &u)
	return u
}

func drain() int {
	const proc = 100000
	// The group "work" is the topic's only channel, and nsqd hands a topic's messages to its
	// first channel also when they were published before the channel was made. So the group
	// exists from the start, and no-loss judges every acknowledged message, also one that no
	// consumer received before a loss. (The records of a first_ command stay in provisioning.)
	h.Invoke(proc, "sub", topic, channel, "from=start").Ok()
	deadline := uptime() + 250
	quiet := 70.0 // above nsqd's --msg-timeout of 60 s
	last := uptime()
	seen := map[string]bool{}
	var cn *conn
	for uptime() < deadline {
		if cn == nil {
			c, err := dial()
			if err != nil {
				last = uptime()
				time.Sleep(time.Second)
				continue
			}
			cn = c
		}
		op := h.Invoke(proc, "recv", topic, channel)
		m, err := cn.next(2 * time.Second)
		if err != nil {
			op.Info()
			cn.c.Close()
			cn = nil
			last = uptime()
			continue
		}
		if m == nil {
			op.Ok(topic, channel)
			if uptime()-last >= quiet {
				drained(proc)
				break
			}
			continue
		}
		last = uptime()
		if strings.HasPrefix(m.body, "probe") {
			op.Ok(topic, channel)
			cn.cmd("FIN " + m.id)
			continue
		}
		op.Ok(topic, channel, item(m))
		seen[m.body] = true
		remember(m.body)
		a := h.Invoke(proc, "ack", topic, channel, item(m))
		if err := cn.cmd("FIN " + m.id); err != nil {
			a.Info()
		} else {
			a.Ok(topic, channel, item(m))
		}
		cn.cmd("RDY 1")
	}
	if uptime() >= deadline {
		fmt.Println("drain: no quiet window within the deadline")
		return 1
	}
	readBack()
	return 0
}

// readBack writes one status record per id the workload sent: s=ok and f=1 when a
// consumer processed and acknowledged the message, s=info and f=0 when none did. A message
// whose FIN got no answer in time and that no consumer finished later is not read back: nsqd
// may have taken that FIN, so its final state is unknown (the claim leaves it undecided).
func readBack() {
	delivered := lines(delLog)
	unknown := lines(finUnknown)
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
	const proc = 100001
	for i := 0; i < len(ids); i += 20 {
		j := i + 20
		if j > len(ids) {
			j = len(ids)
		}
		args := []string{topic}
		inv := []string{topic}
		for _, id := range ids[i:j] {
			inv = append(inv, id)
			if delivered[id] {
				args = append(args, id+";s=ok;f=1")
			} else {
				args = append(args, id+";s=info;f=0")
			}
		}
		h.Invoke(proc, "status", inv...).Ok(args...)
	}
}

func probe() int {
	end := uptime() + 60
	for uptime() < end {
		if _, err := publish(fmt.Sprintf("probe%d", time.Now().UnixNano()), 5*time.Second); err == nil {
			return 0
		} else {
			fmt.Println("probe:", err)
		}
		time.Sleep(time.Second)
	}
	fmt.Println("probe: no publish succeeded within 60 s after the faults")
	return 1
}

// drained writes the completion record "drained <channel> <group>" alone: the
// Go recorder has no call for a record without an invocation.
func drained(proc int) {
	f, err := os.OpenFile("/dev/console", os.O_WRONLY, 0)
	if err != nil {
		return
	}
	fmt.Fprintf(f, "HYDO-HIST jobs %d ok %d drained %s %s\n", proc, mono(), topic, channel)
	f.Close()
}

func mono() int64 {
	var ts unix.Timespec
	unix.ClockGettime(unix.CLOCK_MONOTONIC, &ts)
	return ts.Nano()
}
