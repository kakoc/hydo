// ledger: the Rust example's account server in Go, to show function contracts (`// hydo:`
// comments, docs/design/contracts.md) on a Go program. The same three roles and protocol:
//
//	ledger serve <port>                     one account, opening balance 1,000,000
//	ledger debit <port> <req-id> <amount>   prints the new balance; exit 1 without an answer
//	ledger balance <port>                   prints the balance (readiness and the check)
//	ledger local <n>                        n debits on a ledger of its own, in this process
//	                                        (what a contract costs per call)
//
// The planted bug is the same: the server remembers applied request ids per connection, and a
// client's retry arrives on a new connection.
package main

import (
	"bufio"
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"time"
)

// Ledger is the account.
type Ledger struct {
	balance uint64
	lastReq uint64 // the highest request id applied so far
	applied uint64
}

// Conn is what the server keeps per connection.
type Conn struct {
	lastReq uint64
}

// apply applies the debit reqID of amount, once per request id, and returns the new balance.
// hydo: requires amount > 0
// hydo: ensures reqID > old(l.lastReq) || l.balance == old(l.balance)
// hydo: ensures reqID <= old(l.lastReq) || l.balance == old(l.balance) - amount
// hydo: ensures result == l.balance
// hydo: input amount in 1..=10_000
//
//go:noinline
func apply(l *Ledger, c *Conn, reqID uint64, amount uint64) uint64 {
	if reqID <= c.lastReq {
		// a retry on this connection: applied already
		return l.balance
	}
	c.lastReq = reqID
	l.balance -= amount
	if reqID > l.lastReq {
		l.lastReq = reqID
	}
	l.applied++
	return l.balance
}

func serve(port string) {
	ln, err := net.Listen("tcp", "127.0.0.1:"+port)
	if err != nil {
		fmt.Fprintln(os.Stderr, "listen:", err)
		os.Exit(1)
	}
	l := &Ledger{balance: 1_000_000}
	for {
		s, err := ln.Accept()
		if err != nil {
			continue
		}
		c := &Conn{}
		r := bufio.NewScanner(s)
		for r.Scan() {
			f := strings.Fields(r.Text())
			answer := "ERR unknown request"
			switch {
			case len(f) == 3 && f[0] == "DEBIT":
				id, e1 := strconv.ParseUint(f[1], 10, 64)
				amount, e2 := strconv.ParseUint(f[2], 10, 64)
				if e1 != nil || e2 != nil {
					answer = "ERR bad numbers"
				} else {
					answer = fmt.Sprintf("OK %d", apply(l, c, id, amount))
				}
			case len(f) == 1 && f[0] == "BALANCE":
				answer = fmt.Sprintf("OK %d", l.balance)
			}
			if _, err := fmt.Fprintln(s, answer); err != nil {
				break
			}
		}
		s.Close()
	}
}

// ask sends one request and returns its answer within wait.
func ask(port, req string, wait time.Duration) (string, bool) {
	s, err := net.Dial("tcp", "127.0.0.1:"+port)
	if err != nil {
		return "", false
	}
	defer s.Close()
	s.SetReadDeadline(time.Now().Add(wait))
	if _, err := fmt.Fprintln(s, req); err != nil {
		return "", false
	}
	line, err := bufio.NewReader(s).ReadString('\n')
	if err != nil || !strings.HasPrefix(line, "OK ") {
		return "", false
	}
	return strings.TrimSpace(line[3:]), true
}

func main() {
	a := os.Args
	switch {
	case len(a) == 3 && a[1] == "serve":
		serve(a[2])
	case len(a) == 5 && a[1] == "debit":
		req := "DEBIT " + a[3] + " " + a[4]
		b, ok := ask(a[2], req, 1000*time.Millisecond)
		if !ok {
			b, ok = ask(a[2], req, 3000*time.Millisecond)
		}
		if !ok {
			os.Exit(1)
		}
		fmt.Println(b)
	case len(a) == 3 && a[1] == "local":
		n, _ := strconv.ParseUint(a[2], 10, 64)
		l := &Ledger{balance: ^uint64(0)}
		for id := uint64(1); id <= n; id++ {
			apply(l, &Conn{}, id, 1)
		}
		fmt.Println(l.balance)
	case len(a) == 3 && a[1] == "balance":
		b, ok := ask(a[2], "BALANCE", 2000*time.Millisecond)
		if !ok {
			os.Exit(1)
		}
		fmt.Println(b)
	default:
		fmt.Fprintln(os.Stderr, "usage: ledger serve <port> | debit <port> <req-id> <amount> | balance <port>")
		os.Exit(2)
	}
}
