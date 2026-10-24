package client

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"

	"hydogrpc/internal/record"
	"hydogrpc/internal/rng"
	"hydogrpc/internal/spec"
)

// State is what several client processes of one run share, kept in files under one
// directory (default /tmp/hydo/grpc): an operation counter (op ids and unique values) and the
// value pools that `from` hints read. Every update is a short critical section under flock,
// so concurrent composer drivers see one consistent sequence (deterministic under hydo, which
// schedules the processes deterministically).
type State struct {
	Dir string
}

// NewState creates the directory if needed.
func NewState(dir string) (*State, error) {
	if err := os.MkdirAll(filepath.Join(dir, "pool"), 0755); err != nil {
		return nil, err
	}
	return &State{Dir: dir}, nil
}

func (s *State) locked(name string, fn func(f *os.File) error) error {
	f, err := os.OpenFile(filepath.Join(s.Dir, name), os.O_CREATE|os.O_RDWR, 0644)
	if err != nil {
		return err
	}
	defer f.Close()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX); err != nil {
		return err
	}
	defer syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
	return fn(f)
}

// Next returns the next value of a named counter (1, 2, ...).
func (s *State) Next(name string) uint64 {
	var n uint64
	err := s.locked(name, func(f *os.File) error {
		b := make([]byte, 32)
		k, _ := f.ReadAt(b, 0)
		n, _ = strconv.ParseUint(strings.TrimSpace(string(b[:k])), 10, 64)
		n++
		if err := f.Truncate(0); err != nil {
			return err
		}
		_, err := f.WriteAt([]byte(strconv.FormatUint(n, 10)+"\n"), 0)
		return err
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "hydo-grpc: counter %s: %v\n", name, err)
	}
	return n
}

func poolFile(pool string) string {
	return "pool/" + strings.NewReplacer("/", "_", " ", "_").Replace(pool)
}

// Put appends values to a pool.
func (s *State) Put(pool string, values []string) {
	if len(values) == 0 {
		return
	}
	var b strings.Builder
	for _, v := range values {
		b.WriteString(record.Enc(v))
		b.WriteByte('\n')
	}
	s.locked(poolFile(pool), func(f *os.File) error {
		if _, err := f.Seek(0, 2); err != nil {
			return err
		}
		_, err := f.WriteString(b.String())
		return err
	})
}

// Take picks one value of a pool: the newest (pick latest) or a random one.
func (s *State) Take(f *spec.From, r *rng.R) (string, bool) {
	var vals []string
	s.locked(poolFile(f.Pool()), func(fh *os.File) error {
		sc := bufio.NewScanner(fh)
		sc.Buffer(make([]byte, 64*1024), 1<<20)
		for sc.Scan() {
			if v, err := record.Dec(sc.Text()); err == nil {
				vals = append(vals, v)
			}
		}
		return nil
	})
	if len(vals) == 0 {
		return "", false
	}
	if f.Pick == "latest" {
		return vals[len(vals)-1], true
	}
	return vals[r.Intn(len(vals))], true
}
