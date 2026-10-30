package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
)

const checkUsage = `hydo-check <check> <sources> [options]
checks (A = --left/--in, B = --right; items are compared as strings):
  subset            every item of A is in B
  equal             A and B hold the same items (as sets)
  exactly-once      every item of A occurs exactly once in B (B counted as a multiset)
  disjoint          no item of A is in B
  no-duplicates     no item occurs twice in --in A
  count             the number of records of --in A: --eq N | --min N | --max N
  never-wrong-value --cache C --truth T: every key C holds has T's value for that key; a key
                    missing from C is fine; a key T does not have is wrong (--allow-orphans: fine)
a source is ONE argument: "[name=]<hydo-read store and args> [projection]", e.g.
  'acked=hydo acked'   'kafka 127.0.0.1:9092 orders'
  'mysql hydo:@tcp(127.0.0.1:3306)/ "SELECT id FROM shop.orders WHERE paid = 1"'
projection (inside the source string):
  --field F           the item is field F (default: the only field; kafka/redis/etcd: key;
                      file: line; hydo: id; range: n; else a field named key)
  --item 'T'          the item is the template T with {F} replaced by field F, e.g.
                      --item 'k:{id}=v{id}' or --item '{key}={value}' (compares keys and values)
  --key F --value F   for never-wrong-value (default key / value)
  --strip-prefix P    remove P from the start of items and keys that have it
  --add-prefix P      then put P in front
options:
  --unreadable error|violation   a source that cannot be read: exit 2 (default) or 1
  --oracle            end with a "HYDO-ORACLE: OK|VIOLATION|ERROR <explanation>" line
  --id ID [--type T] [--msg M]   also print a HYDO-ASSERT record (T default always)
  --console PATH      write the HYDO-ASSERT record there instead of stdout (e.g. /dev/ttyS0)
  --show N            list up to N offending items (default 5)
  --timeout S         per source read (default 30)
The last output line is the explanation. Exit 0 holds, 1 violated, 2 could not evaluate.`

type source struct {
	spec    string
	name    string
	args    []string // hydo-read arguments
	field   string
	item    string // template: {field} replaced by the record's field
	key     string
	value   string
	strip   string
	add     string
	records []Record
}

func isName(s string) bool {
	if s == "" || !(s[0] >= 'a' && s[0] <= 'z' || s[0] >= 'A' && s[0] <= 'Z') {
		return false
	}
	for _, c := range s {
		if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == '_' || c == '-' || c == '.') {
			return false
		}
	}
	return true
}

func parseSource(spec string) (*source, error) {
	w, err := shellWords(spec)
	if err != nil {
		return nil, usagef("source %q: %v", spec, err)
	}
	if len(w) == 0 {
		return nil, usagef("empty source")
	}
	s := &source{spec: spec}
	if i := strings.Index(w[0], "="); i > 0 && isName(w[0][:i]) {
		s.name = w[0][:i]
		if w[0][i+1:] == "" {
			w = w[1:]
		} else {
			w[0] = w[0][i+1:]
		}
	}
	for i := 0; i < len(w); i++ {
		var dst *string
		switch w[i] {
		case "--field":
			dst = &s.field
		case "--item":
			dst = &s.item
		case "--key":
			dst = &s.key
		case "--value":
			dst = &s.value
		case "--strip-prefix":
			dst = &s.strip
		case "--add-prefix":
			dst = &s.add
		}
		if dst == nil {
			s.args = append(s.args, w[i])
			continue
		}
		if i+1 >= len(w) {
			return nil, usagef("source %q: %s needs a value", spec, w[i])
		}
		*dst = w[i+1]
		i++
	}
	if len(s.args) == 0 {
		return nil, usagef("source %q: no store", spec)
	}
	if s.name == "" {
		s.name = s.args[0]
	}
	return s, nil
}

func (s *source) load(timeout time.Duration) error {
	args := append([]string{}, s.args...)
	own := false
	for _, a := range args {
		own = own || a == "--timeout"
	}
	if !own { // a --timeout inside the source wins over hydo-check's --timeout
		args = append(args, "--timeout", strconv.FormatFloat(timeout.Seconds(), 'g', -1, 64))
	}
	recs, _, err := readStore(args)
	s.records = recs
	return err
}

func (s *source) fix(x string) string {
	if s.strip != "" {
		x = strings.TrimPrefix(x, s.strip)
	}
	return s.add + x
}

// field renders a record field as an item string; ok=false for a missing field or null.
func field(r Record, f string) (string, bool) {
	v, present := r[f]
	if !present || v == nil {
		return "", false
	}
	switch x := v.(type) {
	case string:
		return x, true
	case int64:
		return strconv.FormatInt(x, 10), true
	}
	return encode(v), true
}

func (s *source) defaultField() string {
	switch s.args[0] {
	case "kafka", "redis", "etcd":
		return "key"
	case "file":
		return "line"
	case "hydo":
		return "id"
	case "range":
		return "n"
	}
	return ""
}

// items: one item per record (records whose field is null/missing are skipped and counted).
func (s *source) items() ([]string, int, error) {
	var out []string
	skipped := 0
	for _, r := range s.records {
		if s.item != "" {
			v, err := s.render(r)
			if err != nil {
				return nil, 0, err
			}
			out = append(out, s.fix(v))
			continue
		}
		f := s.field
		if f == "" {
			if len(r) == 1 {
				for k := range r {
					f = k
				}
			} else if f = s.defaultField(); f == "" {
				if _, has := r["key"]; has {
					f = "key"
				}
			}
			if f == "" {
				return nil, 0, usagef("source %s: records have %d fields; say which one with --field", s.name, len(r))
			}
		}
		v, ok := field(r, f)
		if !ok {
			if _, has := r[f]; !has && s.field != "" {
				return nil, 0, usagef("source %s: no field %q in %s", s.name, f, encode(r))
			}
			skipped++
			continue
		}
		out = append(out, s.fix(v))
	}
	return out, skipped, nil
}

// render fills the --item template: {name} becomes the record's field (a missing field or
// null is an error / empty), {{ and }} are literal braces.
func (s *source) render(r Record) (string, error) {
	var b strings.Builder
	t := s.item
	for i := 0; i < len(t); i++ {
		switch {
		case strings.HasPrefix(t[i:], "{{"):
			b.WriteByte('{')
			i++
		case strings.HasPrefix(t[i:], "}}"):
			b.WriteByte('}')
			i++
		case t[i] == '{':
			j := strings.IndexByte(t[i:], '}')
			if j < 0 {
				return "", usagef("source %s: --item %q: unclosed {", s.name, t)
			}
			name := t[i+1 : i+j]
			if _, has := r[name]; !has {
				return "", usagef("source %s: --item %q: no field %q in %s", s.name, t, name, encode(r))
			}
			v, _ := field(r, name)
			b.WriteString(v)
			i += j
		default:
			b.WriteByte(t[i])
		}
	}
	return b.String(), nil
}

type kv struct{ k, v string }

// pairs: (key, value) per record; a null value (a missing key) is skipped.
func (s *source) pairs() ([]kv, error) {
	kf, vf := s.key, s.value
	if kf == "" {
		kf = "key"
	}
	if vf == "" {
		vf = "value"
	}
	var out []kv
	for _, r := range s.records {
		k, ok := field(r, kf)
		if !ok {
			return nil, usagef("source %s: no key field %q in %s (use --key)", s.name, kf, encode(r))
		}
		if _, has := r[vf]; !has {
			return nil, usagef("source %s: no value field %q in %s (use --value)", s.name, vf, encode(r))
		}
		v, ok := field(r, vf)
		if !ok {
			continue
		}
		out = append(out, kv{s.fix(k), v})
	}
	return out, nil
}

// itemLess: natural order -- runs of digits compare as numbers, everything else bytewise --
// so 2 < 10 and c4 < c10 < order:2; ties (e.g. 007 and 7) are broken bytewise. Deterministic.
func itemLess(a, b string) bool {
	i, j := 0, 0
	for i < len(a) && j < len(b) {
		da, db := isDigit(a[i]), isDigit(b[j])
		if da && db {
			si, sj := i, j
			for i < len(a) && isDigit(a[i]) {
				i++
			}
			for j < len(b) && isDigit(b[j]) {
				j++
			}
			x := strings.TrimLeft(a[si:i], "0")
			y := strings.TrimLeft(b[sj:j], "0")
			if len(x) != len(y) {
				return len(x) < len(y)
			}
			if x != y {
				return x < y
			}
			continue
		}
		if a[i] != b[j] {
			return a[i] < b[j]
		}
		i++
		j++
	}
	if len(a)-i != len(b)-j {
		return len(a)-i < len(b)-j
	}
	return a < b
}

func isDigit(c byte) bool { return c >= '0' && c <= '9' }

func sortItems(s []string) { sort.SliceStable(s, func(i, j int) bool { return itemLess(s[i], s[j]) }) }

func counts(items []string) map[string]int {
	m := map[string]int{}
	for _, x := range items {
		m[x]++
	}
	return m
}

func keys(m map[string]int) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sortItems(out)
	return out
}

func plural(n int, one, many string) string {
	if n == 1 {
		return "1 " + one
	}
	return fmt.Sprintf("%d %s", n, many)
}

func list(items []string, show int) string {
	if len(items) == 0 {
		return ""
	}
	n := len(items)
	if n > show {
		return strings.Join(items[:show], ", ") + fmt.Sprintf(" (+%d more)", n-show)
	}
	return strings.Join(items, ", ")
}

type verdict struct {
	ok    bool
	first string // the first offending item (empty when ok)
	text  string // the explanation line
}

type checkOpts struct {
	left, right, cache, truth string
	eq, min, max              *int64
	allowOrphans              bool
	unreadableViolation       bool
	oracle                    bool
	id, typ, msg, console     string
	show                      int
	timeout                   time.Duration
}

func checkMain(args []string) int {
	if len(args) == 0 || args[0] == "-h" || args[0] == "--help" {
		fmt.Fprintln(os.Stderr, checkUsage)
		if len(args) == 0 {
			return exitCannot
		}
		return exitOK
	}
	name := args[0]
	o, err := parseCheckOpts(args[1:])
	if err != nil {
		fmt.Printf("hydo-check %s: %v\n", name, err)
		return exitCannot
	}
	v, err := runCheck(name, o)
	if err != nil {
		rc := exitCannot
		kind := "ERROR"
		text := fmt.Sprintf("%s could not be evaluated: %v", name, err)
		if _, usage := err.(usageError); usage {
			fmt.Fprintln(os.Stderr, checkUsage)
		} else if o.unreadableViolation {
			rc, kind = exitViolated, "VIOLATION"
			text = fmt.Sprintf("%s violated: %v", name, err)
		}
		if rc == exitViolated { // "could not evaluate" is not an evaluation: no record
			emit(o, name, false, "", text)
		}
		if o.oracle {
			fmt.Printf("HYDO-ORACLE: %s %s\n", kind, text)
		} else {
			fmt.Println(text)
		}
		return rc
	}
	emit(o, name, v.ok, v.first, v.text)
	if o.oracle {
		k := "OK"
		if !v.ok {
			k = "VIOLATION"
		}
		fmt.Printf("HYDO-ORACLE: %s %s\n", k, v.text)
	} else {
		fmt.Println(v.text)
	}
	if v.ok {
		return exitOK
	}
	return exitViolated
}

func parseCheckOpts(a []string) (*checkOpts, error) {
	o := &checkOpts{typ: "always", show: 5, timeout: 30 * time.Second}
	num := func(s string) (*int64, error) {
		n, err := strconv.ParseInt(s, 10, 64)
		if err != nil {
			return nil, usagef("%q is not an integer", s)
		}
		return &n, nil
	}
	for i := 0; i < len(a); i++ {
		flag := a[i]
		val := func() (string, error) {
			if i+1 >= len(a) {
				return "", usagef("%s needs a value", flag)
			}
			i++
			return a[i], nil
		}
		var s string
		var err error
		switch flag {
		case "--allow-orphans", "--oracle":
			if flag == "--oracle" {
				o.oracle = true
			} else {
				o.allowOrphans = true
			}
			continue
		}
		if s, err = val(); err != nil {
			return nil, err
		}
		switch flag {
		case "--left", "--in":
			o.left = s
		case "--right":
			o.right = s
		case "--cache":
			o.cache = s
		case "--truth":
			o.truth = s
		case "--eq":
			o.eq, err = num(s)
		case "--min":
			o.min, err = num(s)
		case "--max":
			o.max, err = num(s)
		case "--unreadable":
			switch s {
			case "error":
			case "violation":
				o.unreadableViolation = true
			default:
				return nil, usagef("--unreadable error|violation")
			}
		case "--id":
			o.id = s
		case "--type":
			o.typ = s
		case "--msg":
			o.msg = s
		case "--console":
			o.console = s
		case "--show":
			var n *int64
			if n, err = num(s); err == nil {
				o.show = int(*n)
			}
		case "--timeout":
			f, e := strconv.ParseFloat(s, 64)
			if e != nil || f <= 0 {
				return nil, usagef("--timeout %q: seconds > 0", s)
			}
			o.timeout = time.Duration(f * float64(time.Second))
		default:
			return nil, usagef("unknown option %s", flag)
		}
		if err != nil {
			return nil, err
		}
	}
	return o, nil
}

func loadAll(o *checkOpts, specs ...string) ([]*source, error) {
	var out []*source
	for _, sp := range specs {
		s, err := parseSource(sp)
		if err != nil {
			return nil, err
		}
		out = append(out, s)
	}
	for _, s := range out {
		if err := s.load(o.timeout); err != nil {
			if _, u := err.(usageError); u {
				return nil, usagef("source %s: %v", s.name, err)
			}
			return nil, fmt.Errorf("could not read %s (%s): %v", s.name, s.args[0], strings.TrimPrefix(err.Error(), "could not read: "))
		}
	}
	return out, nil
}

func runCheck(name string, o *checkOpts) (verdict, error) {
	need := func(what ...string) error {
		m := map[string]string{"--left": o.left, "--right": o.right, "--cache": o.cache, "--truth": o.truth}
		for _, w := range what {
			if m[w] == "" {
				return usagef("%s needs %s", name, strings.Join(what, " and "))
			}
		}
		return nil
	}
	switch name {
	case "subset", "equal", "exactly-once", "disjoint":
		if err := need("--left", "--right"); err != nil {
			return verdict{}, err
		}
		src, err := loadAll(o, o.left, o.right)
		if err != nil {
			return verdict{}, err
		}
		return setCheck(name, src[0], src[1], o)
	case "no-duplicates", "count":
		if err := need("--left"); err != nil {
			return verdict{}, usagef("%s needs --in", name)
		}
		if name == "count" && o.eq == nil && o.min == nil && o.max == nil {
			return verdict{}, usagef("count needs --eq, --min or --max")
		}
		src, err := loadAll(o, o.left)
		if err != nil {
			return verdict{}, err
		}
		return oneCheck(name, src[0], o)
	case "never-wrong-value":
		if err := need("--cache", "--truth"); err != nil {
			return verdict{}, err
		}
		src, err := loadAll(o, o.cache, o.truth)
		if err != nil {
			return verdict{}, err
		}
		return valueCheck(src[0], src[1], o)
	}
	return verdict{}, usagef("unknown check %q", name)
}

func setCheck(name string, l, r *source, o *checkOpts) (verdict, error) {
	li, _, err := l.items()
	if err != nil {
		return verdict{}, err
	}
	ri, _, err := r.items()
	if err != nil {
		return verdict{}, err
	}
	lc, rc := counts(li), counts(ri)
	lk := keys(lc)
	var bad []string
	switch name {
	case "subset", "equal":
		for _, x := range lk {
			if rc[x] == 0 {
				bad = append(bad, x)
			}
		}
		var extra []string
		if name == "equal" {
			for _, x := range keys(rc) {
				if lc[x] == 0 {
					extra = append(extra, x)
				}
			}
		}
		if len(bad) == 0 && len(extra) == 0 {
			if name == "equal" {
				return verdict{true, "", fmt.Sprintf("equal holds: %s and %s hold the same %s",
					l.name, r.name, plural(len(lk), "item", "items"))}, nil
			}
			return verdict{true, "", fmt.Sprintf("subset holds: all %s of %s are in %s (%s has %s)",
				plural(len(lk), "item", "items"), l.name, r.name, r.name, plural(len(rc), "item", "items"))}, nil
		}
		if len(bad) > 0 {
			return verdict{false, bad[0], fmt.Sprintf("%s violated: %s of %d in %s not in %s: %s",
				name, plural(len(bad), "item", "items"), len(lk), l.name, r.name, list(bad, o.show))}, nil
		}
		return verdict{false, extra[0], fmt.Sprintf("equal violated: %s in %s not in %s: %s",
			plural(len(extra), "item", "items"), r.name, l.name, list(extra, o.show))}, nil
	case "exactly-once":
		var missing, dup []string
		for _, x := range lk {
			switch {
			case rc[x] == 0:
				missing = append(missing, x)
				bad = append(bad, x)
			case rc[x] > 1:
				dup = append(dup, fmt.Sprintf("%s (x%d)", x, rc[x]))
				bad = append(bad, x)
			}
		}
		if len(bad) == 0 {
			return verdict{true, "", fmt.Sprintf("exactly-once holds: each of the %s of %s occurs once in %s (%s has %s)",
				plural(len(lk), "item", "items"), l.name, r.name, r.name, plural(len(ri), "record", "records"))}, nil
		}
		first := bad[0]
		head := fmt.Sprintf("%s is in %s %d times", first, r.name, rc[first])
		var parts []string
		if len(missing) > 0 {
			parts = append(parts, fmt.Sprintf("missing %d: %s", len(missing), list(missing, o.show)))
		}
		if len(dup) > 0 {
			parts = append(parts, fmt.Sprintf("duplicated %d: %s", len(dup), list(dup, o.show)))
		}
		return verdict{false, first, fmt.Sprintf("exactly-once violated: %s %s (of %d in %s); %s",
			l.name, head, len(lk), l.name, strings.Join(parts, "; "))}, nil
	case "disjoint":
		for _, x := range lk {
			if rc[x] > 0 {
				bad = append(bad, x)
			}
		}
		if len(bad) == 0 {
			return verdict{true, "", fmt.Sprintf("disjoint holds: none of the %s of %s is in %s",
				plural(len(lk), "item", "items"), l.name, r.name)}, nil
		}
		return verdict{false, bad[0], fmt.Sprintf("disjoint violated: %s of %s %s also in %s: %s",
			plural(len(bad), "item", "items"), l.name, map[bool]string{true: "is", false: "are"}[len(bad) == 1], r.name, list(bad, o.show))}, nil
	}
	return verdict{}, usagef("unknown check %q", name)
}

func oneCheck(name string, s *source, o *checkOpts) (verdict, error) {
	if name == "count" {
		n := int64(len(s.records))
		var want []string
		ok := true
		if o.eq != nil {
			want = append(want, fmt.Sprintf("== %d", *o.eq))
			ok = ok && n == *o.eq
		}
		if o.min != nil {
			want = append(want, fmt.Sprintf(">= %d", *o.min))
			ok = ok && n >= *o.min
		}
		if o.max != nil {
			want = append(want, fmt.Sprintf("<= %d", *o.max))
			ok = ok && n <= *o.max
		}
		w := strings.Join(want, " and ")
		if ok {
			return verdict{true, "", fmt.Sprintf("count holds: %s has %s (want %s)", s.name, plural(int(n), "record", "records"), w)}, nil
		}
		first := ""
		ex := ""
		if n > 0 {
			lines := encodeAll(s.records, false)
			first = lines[0]
			ex = "; first: " + first
		}
		return verdict{false, first, fmt.Sprintf("count violated: %s has %s, want %s%s", s.name, plural(int(n), "record", "records"), w, ex)}, nil
	}
	items, _, err := s.items()
	if err != nil {
		return verdict{}, err
	}
	c := counts(items)
	var dup []string
	first := ""
	for _, x := range keys(c) {
		if c[x] > 1 {
			if first == "" {
				first = x
			}
			dup = append(dup, fmt.Sprintf("%s (x%d)", x, c[x]))
		}
	}
	if len(dup) == 0 {
		return verdict{true, "", fmt.Sprintf("no-duplicates holds: %s distinct in %s", plural(len(c), "item", "items"), s.name)}, nil
	}
	return verdict{false, first, fmt.Sprintf("no-duplicates violated: %s occurs %d times in %s; %s repeated: %s",
		first, c[first], s.name, plural(len(dup), "item", "items"), list(dup, o.show))}, nil
}

func valueCheck(cache, truth *source, o *checkOpts) (verdict, error) {
	cp, err := cache.pairs()
	if err != nil {
		return verdict{}, err
	}
	tp, err := truth.pairs()
	if err != nil {
		return verdict{}, err
	}
	tv := map[string][]string{}
	for _, p := range tp {
		tv[p.k] = append(tv[p.k], p.v)
	}
	type badKV struct{ k, text string }
	var bad []badKV
	seen := map[string]bool{}
	sort.SliceStable(cp, func(i, j int) bool {
		if cp[i].k != cp[j].k {
			return itemLess(cp[i].k, cp[j].k)
		}
		return cp[i].v < cp[j].v
	})
	for _, p := range cp {
		t, has := tv[p.k]
		switch {
		case !has && !o.allowOrphans:
			if !seen[p.k] {
				bad = append(bad, badKV{p.k, fmt.Sprintf("%s=%s but %s has no such key", p.k, p.v, truth.name)})
			}
		case has && len(t) > 1:
			if !seen[p.k] {
				bad = append(bad, badKV{p.k, fmt.Sprintf("%s has %d values for %s: %s", truth.name, len(t), p.k, strings.Join(t, ", "))})
			}
		case has && t[0] != p.v:
			bad = append(bad, badKV{p.k, fmt.Sprintf("%s=%s but %s has %s", p.k, p.v, truth.name, t[0])})
		default:
			continue
		}
		seen[p.k] = true
	}
	if len(bad) == 0 {
		return verdict{true, "", fmt.Sprintf("never-wrong-value holds: all %s of %s agree with %s (%s has %s)",
			plural(len(cp), "value", "values"), cache.name, truth.name, truth.name, plural(len(tv), "key", "keys"))}, nil
	}
	texts := make([]string, len(bad))
	for i, b := range bad {
		texts[i] = b.text
	}
	return verdict{false, bad[0].k, fmt.Sprintf("never-wrong-value violated: %s wrong in %s: %s",
		plural(len(bad), "value", "values"), cache.name, list(texts, o.show))}, nil
}

// emit prints the HYDO-ASSERT record when --id is given (hydo/ASSERTIONS.md wire format).
func emit(o *checkOpts, name string, ok bool, first, text string) {
	if o.id == "" {
		return
	}
	msg := o.msg
	if msg == "" {
		msg = name
	}
	d := map[string]any{"check": name, "explanation": text}
	if first != "" {
		d["first"] = first
	}
	line := "HYDO-ASSERT " + encode(map[string]any{"t": o.typ, "id": o.id, "ok": ok, "msg": msg, "details": d}) + "\n"
	decl := "HYDO-ASSERT-DECL " + encode(map[string]any{"t": o.typ, "id": o.id, "msg": msg}) + "\n"
	if o.console != "" {
		if f, err := os.OpenFile(o.console, os.O_WRONLY|os.O_APPEND, 0); err == nil {
			f.Write([]byte(decl))
			f.Write([]byte(line))
			f.Close()
			return
		}
	}
	os.Stdout.WriteString(decl + line)
}

// shellWords splits like a POSIX shell (no expansion): '...' literal, "..." with \" \\ \$ \`,
// backslash outside quotes escapes the next character.
func shellWords(s string) ([]string, error) {
	var out []string
	var cur strings.Builder
	have := false
	for i := 0; i < len(s); i++ {
		c := s[i]
		switch {
		case c == '\'':
			j := strings.IndexByte(s[i+1:], '\'')
			if j < 0 {
				return nil, fmt.Errorf("unterminated '")
			}
			cur.WriteString(s[i+1 : i+1+j])
			i += j + 1
			have = true
		case c == '"':
			i++
			for ; i < len(s) && s[i] != '"'; i++ {
				if s[i] == '\\' && i+1 < len(s) && strings.IndexByte("\"\\$`", s[i+1]) >= 0 {
					i++
				}
				cur.WriteByte(s[i])
			}
			if i >= len(s) {
				return nil, fmt.Errorf("unterminated \"")
			}
			have = true
		case c == '\\' && i+1 < len(s):
			i++
			cur.WriteByte(s[i])
			have = true
		case c == ' ' || c == '\t' || c == '\n':
			if have {
				out = append(out, cur.String())
				cur.Reset()
				have = false
			}
		default:
			cur.WriteByte(c)
			have = true
		}
	}
	if have {
		out = append(out, cur.String())
	}
	return out, nil
}
