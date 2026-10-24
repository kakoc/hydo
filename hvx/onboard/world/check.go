package main

import (
	"bufio"
	"fmt"
	"io"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

// ctlMain: one command to the running world over its control socket; prints the answer.
func ctlMain(args []string) int {
	if len(args) == 0 {
		usage()
	}
	c, err := net.DialTimeout("unix", sockPath(), 5*time.Second)
	if err != nil {
		fmt.Fprintln(os.Stderr, "hydo-world ctl:", err)
		return 1
	}
	defer c.Close()
	fmt.Fprintln(c, strings.Join(args, " "))
	b, _ := io.ReadAll(bufio.NewReader(c))
	s := string(b)
	if args[0] == "dump" || args[0] == "summary" { // one per line, without the END line
		s = strings.TrimSuffix(strings.TrimSuffix(s, "\n"), "END")
	}
	fmt.Print(s)
	if strings.HasPrefix(s, "error") {
		return 1
	}
	return 0
}

// ---------------------------------------------------------------- side-effect checks
//
//	check at-most-once     <ep>.<route> [--by ref|idem]       no group has two effects
//	check exactly-once     <ep>.<route> [--ref FMT] [--acked FILE]   at most once, and every
//	                                                           acknowledged id has its effect
//	check effect-for-acked <ep>.<route> [--ref FMT] [--acked FILE]   every acknowledged id has
//	                                                           its effect (use as `eventually`)
//	check none-after       <ep>.<route> --after event:<name>|<ep>.<route>   no effect on a ref
//	                                                           after that ref's first <after> record
//	check count            <ep>.<route>                        print how many effects (always 0)
//
// FMT turns an acknowledged id (one line of $HYDO_ACKED) into the ref the effect carries:
// {id} is replaced by the id (default "{id}"). Exit status: 0 holds, 1 violated, 2 cannot be
// decided (no world log, or a power cut's carried log was too long). The last line is a
// HYDO-ORACLE line, which the generic init uses as the invariant's details.

type rec map[string]string

func readLog() ([]rec, bool, error) {
	b, err := os.ReadFile(logPath())
	if err != nil {
		return nil, false, err
	}
	var out []rec
	trunc := false
	for _, ln := range strings.Split(string(b), "\n") {
		r := parseRecord(ln)
		if r == nil {
			continue
		}
		if r["kind"] == "meta" && r["trunc"] == "1" {
			trunc = true
		}
		out = append(out, r)
	}
	sort.SliceStable(out, func(i, j int) bool { return atoi(out[i]["seq"]) < atoi(out[j]["seq"]) })
	return out, trunc, nil
}

func atoi(s string) int64 { n, _ := strconv.ParseInt(s, 10, 64); return n }

func say(code int, format string, a ...any) int {
	word := map[int]string{0: "OK", 1: "VIOLATION", 2: "ERROR"}[code]
	fmt.Printf("HYDO-ORACLE: %s %s\n", word, fmt.Sprintf(format, a...))
	return code
}

func isEffect(r rec, target string) bool {
	if r["fx"] != "1" {
		return false
	}
	ep, route, _ := strings.Cut(target, ".")
	if strings.HasPrefix(target, "event:") {
		return r["kind"] == "event" && r["route"] == strings.TrimPrefix(target, "event:")
	}
	return r["kind"] == "req" && r["ep"] == ep && (route == "" || r["route"] == route)
}

func ackedRefs(file, format string) ([]string, error) {
	b, err := os.ReadFile(file)
	if err != nil {
		return nil, err
	}
	var out []string
	seen := map[string]bool{}
	for _, id := range strings.Fields(string(b)) {
		if strings.HasPrefix(id, "w:") || strings.Contains(id, "=") || seen[id] {
			continue
		}
		seen[id] = true
		out = append(out, strings.ReplaceAll(format, "{id}", id))
	}
	return out, nil
}

func checkMain(args []string) int {
	if len(args) < 2 {
		usage()
	}
	kind, target := args[0], args[1]
	opt := map[string]string{"--by": "ref", "--ref": "{id}", "--acked": os.Getenv("HYDO_ACKED"), "--after": ""}
	for i := 2; i+1 < len(args); i += 2 {
		if _, ok := opt[args[i]]; !ok {
			return say(2, "check: unknown option %s", args[i])
		}
		opt[args[i]] = args[i+1]
	}
	recs, trunc, err := readLog()
	if err != nil {
		return say(2, "%s: no world log (%v)", target, err)
	}
	if trunc {
		return say(2, "%s: the world log carried across the power cut was too long (more than %d bytes); cannot decide", target, carryMax)
	}
	effects := map[string][]int64{}
	var order []string
	n := 0
	for _, r := range recs {
		if !isEffect(r, target) {
			continue
		}
		n++
		g := r[opt["--by"]]
		if _, ok := effects[g]; !ok {
			order = append(order, g)
		}
		effects[g] = append(effects[g], atoi(r["seq"]))
	}
	atMostOnce := func() int {
		for _, g := range order {
			if g == "" {
				continue // no ref/key: cannot be grouped
			}
			if s := effects[g]; len(s) > 1 {
				return say(1, "%s: %s %s took effect %d times (records %s)", target, opt["--by"], g, len(s), joinInts(s))
			}
		}
		return -1
	}
	forAcked := func() int {
		refs, err := ackedRefs(opt["--acked"], opt["--ref"])
		if err != nil {
			return say(2, "%s: acknowledged ids: %v", target, err)
		}
		for _, ref := range refs {
			if len(effects[ref]) == 0 {
				return say(1, "%s: acknowledged %s has no effect", target, ref)
			}
		}
		return say(0, "%s: all %d acknowledged have their effect (%d effects)", target, len(refs), n)
	}
	switch kind {
	case "count":
		return say(0, "%s: %d effects, %d refs", target, n, len(order))
	case "at-most-once":
		if rc := atMostOnce(); rc >= 0 {
			return rc
		}
		return say(0, "%s: %d effects, every %s at most once", target, n, opt["--by"])
	case "exactly-once":
		if rc := atMostOnce(); rc >= 0 {
			return rc
		}
		return forAcked()
	case "effect-for-acked":
		return forAcked()
	case "none-after":
		if opt["--after"] == "" {
			return say(2, "check none-after needs --after event:<name> or <ep>.<route>")
		}
		first := map[string]int64{}
		for _, r := range recs {
			if isEffect(r, opt["--after"]) && r["ref"] != "" {
				if _, ok := first[r["ref"]]; !ok {
					first[r["ref"]] = atoi(r["seq"])
				}
			}
		}
		for _, r := range recs {
			if isEffect(r, target) {
				if f, ok := first[r["ref"]]; ok && atoi(r["seq"]) > f {
					return say(1, "%s: ref %s took effect (record %s) after %s (record %d)", target, r["ref"], r["seq"], opt["--after"], f)
				}
			}
		}
		return say(0, "%s: no effect after %s (%d refs had it)", target, opt["--after"], len(first))
	}
	return say(2, "check: unknown kind %s", kind)
}

func joinInts(s []int64) string {
	p := make([]string, len(s))
	for i, x := range s {
		p[i] = strconv.FormatInt(x, 10)
	}
	return strings.Join(p, ",")
}
