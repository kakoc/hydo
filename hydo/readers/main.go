// hydo-read and hydo-check: ready-made store readers and set-style checks for hydo
// invariants (hvx/onboard/MANIFEST.md, "Reading the stores").
//
//	hydo-read  <store> <args...>     print what is in a table / on a topic / under a key,
//	                                 as JSON Lines, sorted, one record per line
//	hydo-check <check> <options...>  compare such readings: subset, equal, exactly-once,
//	                                 disjoint, no-duplicates, count, never-wrong-value
//
// One static binary; it runs as hydo-check when called by that name (a symlink) or as
// `hydo-read check ...`. Exit status, for both: 0 = ok / the property holds, 1 = the
// property is violated (hydo-check only), 2 = could not read a store or bad usage (an
// invariant that exits 2 "could not be evaluated now", MANIFEST.md).
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	exitOK       = 0
	exitViolated = 1
	exitCannot   = 2
)

// Record is one row / message / key. Values are strings, nil (SQL NULL, a missing key),
// int64, []any or map[string]any. JSON output sorts the keys.
type Record map[string]any

// cannotRead: the store is down, refused the request, or returned an error.
type cannotRead struct{ err error }

func (c cannotRead) Error() string { return "could not read: " + c.err.Error() }

// usageError: the command line is wrong.
type usageError struct{ msg string }

func (u usageError) Error() string { return "usage: " + u.msg }

func usagef(f string, a ...any) error { return usageError{fmt.Sprintf(f, a...)} }
func cannot(err error) error        { return cannotRead{err} }
func cannotf(f string, a ...any) error {
	return cannotRead{fmt.Errorf(f, a...)}
}

const readUsage = `hydo-read <store> <args...> [--timeout <s>] [--base64]
  mysql    <dsn> <query>                 rows as {"<column>": "<value>"|null}
  postgres <dsn> <query>                 (dsn: user:pw@tcp(h:p)/db or mysql://user@h:p/db;
                                          postgres://user@h:p/db?sslmode=disable)
  kafka    <bootstrap> <topic>           every record from the log start to the high
                                          watermark: partition, offset, key, value, timestamp_ms
  redis    <addr> get <key>...           {key, value|null}
  redis    <addr> scan <pattern>         {key, type, value} for every matching key
  redis    <addr> hgetall|smembers|lrange|zrange <key>
  redis    <addr> cmd <word>...          one raw command, {reply}
  etcd     <endpoint> get <key> [--prefix] [--serializable]   {key, value, create_revision,
                                          mod_revision, version}
  etcd     <endpoint> status             {endpoint, revision, raft_index, raft_term}
  file     <path>                        one {line} per non-empty line
  jsonl    <path>                        hydo-read output saved earlier
  range    <from> <to>                   {n} for n in [from, to)
  hydo     acked|failed|told-failed|unknown   ids from $HYDO_ACKED / $HYDO_FAILED / $HYDO_OUTCOMES
Output: JSON Lines, keys sorted, lines sorted bytewise (kafka, lrange and zrange keep the
store's order). Exit 0 ok, 2 could not read the store or bad usage.`

func main() {
	args := os.Args[1:]
	if filepath.Base(os.Args[0]) == "hydo-check" {
		os.Exit(checkMain(args))
	}
	if len(args) > 0 && args[0] == "check" {
		os.Exit(checkMain(args[1:]))
	}
	os.Exit(readMain(args))
}

// readOpts are the options every store takes.
type readOpts struct {
	timeout time.Duration
	base64  bool
}

// splitReadOpts removes --timeout/--base64 from args (anywhere).
func splitReadOpts(args []string) ([]string, readOpts, error) {
	o := readOpts{timeout: 30 * time.Second}
	var rest []string
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--timeout":
			if i+1 >= len(args) {
				return nil, o, usagef("--timeout needs seconds")
			}
			var s float64
			if _, err := fmt.Sscanf(args[i+1], "%g", &s); err != nil || s <= 0 {
				return nil, o, usagef("--timeout %q: want seconds > 0", args[i+1])
			}
			o.timeout = time.Duration(s * float64(time.Second))
			i++
		case "--base64":
			o.base64 = true
		default:
			rest = append(rest, args[i])
		}
	}
	return rest, o, nil
}

func readMain(args []string) int {
	if len(args) == 0 || args[0] == "-h" || args[0] == "--help" {
		fmt.Fprintln(os.Stderr, readUsage)
		if len(args) == 0 {
			return exitCannot
		}
		return exitOK
	}
	recs, ordered, err := readStore(args)
	if err != nil {
		fmt.Fprintf(os.Stderr, "hydo-read: %s: %v\n", args[0], err)
		if _, ok := err.(usageError); ok {
			fmt.Fprintln(os.Stderr, readUsage)
		}
		return exitCannot
	}
	lines := encodeAll(recs, ordered)
	w := bytes.Buffer{}
	for _, l := range lines {
		w.WriteString(l)
		w.WriteByte('\n')
	}
	os.Stdout.Write(w.Bytes())
	return exitOK
}

func encodeAll(recs []Record, ordered bool) []string {
	lines := make([]string, len(recs))
	for i, r := range recs {
		lines[i] = encode(r)
	}
	if !ordered {
		sort.Strings(lines)
	}
	return lines
}

func encode(v any) string {
	var b bytes.Buffer
	e := json.NewEncoder(&b)
	e.SetEscapeHTML(false)
	e.Encode(v)
	return strings.TrimRight(b.String(), "\n")
}

// readStore runs one store reader. ordered = the records are in the store's own order.
func readStore(args []string) ([]Record, bool, error) {
	args, o, err := splitReadOpts(args)
	if err != nil {
		return nil, false, err
	}
	if len(args) == 0 {
		return nil, false, usagef("no store")
	}
	store, a := args[0], args[1:]
	switch store {
	case "mysql", "postgres":
		if len(a) != 2 {
			return nil, false, usagef("%s <dsn> <query> (quote the query)", store)
		}
		r, err := readSQL(store, a[0], a[1], o)
		return r, false, err
	case "kafka":
		if len(a) != 2 {
			return nil, false, usagef("kafka <bootstrap> <topic>")
		}
		r, err := readKafka(a[0], a[1], o)
		return r, true, err
	case "redis":
		return readRedis(a, o)
	case "etcd":
		r, err := readEtcd(a, o)
		return r, false, err
	case "file":
		if len(a) != 1 {
			return nil, false, usagef("file <path>")
		}
		r, err := readFile(a[0])
		return r, false, err
	case "jsonl":
		if len(a) != 1 {
			return nil, false, usagef("jsonl <path>")
		}
		r, err := readJSONL(a[0])
		return r, true, err
	case "range":
		if len(a) != 2 {
			return nil, false, usagef("range <from> <to>")
		}
		var from, to int64
		if _, err := fmt.Sscanf(a[0]+" "+a[1], "%d %d", &from, &to); err != nil || to < from || to-from > 10000000 {
			return nil, false, usagef("range %s %s: two integers, from <= to", a[0], a[1])
		}
		var r []Record
		for n := from; n < to; n++ {
			r = append(r, Record{"n": n})
		}
		return r, true, nil
	case "hydo":
		if len(a) != 1 {
			return nil, false, usagef("hydo acked|failed|told-failed|unknown")
		}
		r, err := readHydo(a[0])
		return r, false, err
	}
	return nil, false, usagef("unknown store %q", store)
}

func readFile(path string) ([]Record, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, cannot(err)
	}
	var r []Record
	for _, l := range strings.Split(string(b), "\n") {
		l = strings.TrimRight(l, "\r")
		if strings.TrimSpace(l) == "" {
			continue
		}
		r = append(r, Record{"line": l})
	}
	return r, nil
}

func readJSONL(path string) ([]Record, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, cannot(err)
	}
	var r []Record
	for i, l := range strings.Split(string(b), "\n") {
		if strings.TrimSpace(l) == "" {
			continue
		}
		d := json.NewDecoder(strings.NewReader(l))
		d.UseNumber()
		var rec Record
		if err := d.Decode(&rec); err != nil {
			return nil, cannotf("%s line %d: %v", path, i+1, err)
		}
		for k, v := range rec {
			if n, ok := v.(json.Number); ok {
				if x, err := n.Int64(); err == nil {
					rec[k] = x
				} else {
					rec[k] = n.String()
				}
			}
		}
		r = append(r, rec)
	}
	return r, nil
}

// readHydo: the hydo client-side files every manifest command sees (MANIFEST.md).
//
//	acked        $HYDO_ACKED: every id the client saw acknowledged
//	failed       $HYDO_FAILED: ids whose last outcome was a definite failure
//	told-failed  failed minus acked: told "failed" and never acknowledged later
//	unknown      ids whose last outcome in $HYDO_OUTCOMES is "unknown" (empty after a power cut)
func readHydo(what string) ([]Record, error) {
	ids := func(env string) (map[string]bool, []string, error) {
		p := os.Getenv(env)
		if p == "" {
			return nil, nil, cannotf("$%s is not set (not running under hydo-init?)", env)
		}
		recs, err := readFile(p)
		if err != nil {
			return nil, nil, err
		}
		set := map[string]bool{}
		var list []string
		for _, r := range recs {
			id := strings.Fields(r["line"].(string))[0]
			if !set[id] {
				set[id] = true
				list = append(list, id)
			}
		}
		return set, list, nil
	}
	out := func(list []string) []Record {
		r := make([]Record, 0, len(list))
		for _, id := range list {
			r = append(r, Record{"id": id})
		}
		return r
	}
	switch what {
	case "acked", "failed":
		env := map[string]string{"acked": "HYDO_ACKED", "failed": "HYDO_FAILED"}[what]
		_, list, err := ids(env)
		return out(list), err
	case "told-failed":
		acked, _, err := ids("HYDO_ACKED")
		if err != nil {
			return nil, err
		}
		_, failed, err := ids("HYDO_FAILED")
		if err != nil {
			return nil, err
		}
		var list []string
		for _, id := range failed {
			if !acked[id] {
				list = append(list, id)
			}
		}
		return out(list), nil
	case "unknown":
		p := os.Getenv("HYDO_OUTCOMES")
		if p == "" {
			return nil, cannotf("$HYDO_OUTCOMES is not set (not running under hydo-init?)")
		}
		recs, err := readFile(p)
		if err != nil {
			return nil, err
		}
		last := map[string]string{}
		var order []string
		for _, r := range recs {
			f := strings.Fields(r["line"].(string))
			if len(f) < 2 {
				continue
			}
			if _, seen := last[f[0]]; !seen {
				order = append(order, f[0])
			}
			last[f[0]] = f[1]
		}
		var list []string
		for _, id := range order {
			if last[id] == "unknown" {
				list = append(list, id)
			}
		}
		return out(list), nil
	}
	return nil, usagef("hydo %q: acked, failed, told-failed or unknown", what)
}
