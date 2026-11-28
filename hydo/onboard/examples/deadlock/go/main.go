// bank-go: a planted deadlock for `hydo inspect stacks` (docs/design/inspect-stacks.md).
//
// Two goroutines share two locks. The worker takes accountsMu, works on the accounts for a
// while, then takes journalMu to write the transfer down. The watchdog wakes every 10 ms; when
// it sees a gap of more than 200 ms since its last wake (the process was paused), it checks the
// journal against the accounts, and it takes the two locks in the other order. A pause while the
// worker holds accountsMu (most of the time) makes the two wait on each other for good: the
// service stops writing its log and its count, and no error is printed.
package main

import (
	"fmt"
	"os"
	"strconv"
	"sync"
	"time"
)

var (
	accountsMu sync.Mutex
	journalMu  sync.Mutex
	balances   [4]int64
	journal    []string
	count      int
)

//go:noinline
func transfer(i int) {
	accountsMu.Lock()
	defer accountsMu.Unlock()
	balances[i%4]--
	balances[(i+1)%4]++
	time.Sleep(20 * time.Millisecond) // the accounts' update takes a while
	time.Sleep(5 * time.Millisecond)
	appendJournal(fmt.Sprintf("transfer %d", i))
}

//go:noinline
func appendJournal(line string) {
	journalMu.Lock()
	defer journalMu.Unlock()
	journal = append(journal, line)
	count++
	_ = os.WriteFile("/tmp/bank-go.count", []byte(strconv.Itoa(count)), 0o644)
	fmt.Println(line)
}

//go:noinline
func checkJournal(gap time.Duration) {
	journalMu.Lock()
	defer journalMu.Unlock()
	fmt.Printf("watchdog: no wake for %v, checking the journal against the accounts\n", gap)
	time.Sleep(5 * time.Millisecond)
	accountsMu.Lock() // the other order
	defer accountsMu.Unlock()
	var sum int64
	for _, b := range balances {
		sum += b
	}
	fmt.Printf("watchdog: %d entries, balances sum to %d\n", len(journal), sum)
}

func worker() {
	for i := 0; ; i++ {
		transfer(i)
		time.Sleep(time.Millisecond)
	}
}

func watchdog() {
	last := time.Now()
	for {
		time.Sleep(10 * time.Millisecond)
		if gap := time.Since(last); gap > 200*time.Millisecond {
			checkJournal(gap)
		}
		last = time.Now()
	}
}

func main() {
	go worker()
	go watchdog()
	for {
		time.Sleep(time.Second)
	}
}
