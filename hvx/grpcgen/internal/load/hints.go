package load

import (
	"bytes"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"unicode"

	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"gopkg.in/yaml.v3"

	"hydogrpc/internal/outcome"
	"hydogrpc/internal/spec"
	"hydogrpc/internal/valuegen"
)

// Hints is the user's hints file (YAML or JSON). Every key is optional; unknown keys are
// errors. See MANIFEST.md, "gRPC services".
type Hints struct {
	Target     string             `yaml:"target"`
	DeadlineMs int                `yaml:"deadline_ms"`
	Seed       *uint64            `yaml:"seed"`
	Services   []string           `yaml:"services"` // only these services (full or short names)
	Steps      *spec.Steps        `yaml:"steps"`
	Drive      *spec.Drive        `yaml:"drive"`
	Record     *spec.Record       `yaml:"record"`
	Defaults   *DefaultsHint      `yaml:"defaults"`
	RPCs       map[string]RPCHint `yaml:"rpcs"`
	Scenarios  []ScenarioHint     `yaml:"scenarios"`
}

// DefaultsHint overrides spec.Defaults.
type DefaultsHint struct {
	StringLen    []int    `yaml:"string_len"`
	BytesLen     []int    `yaml:"bytes_len"`
	RepeatedLen  []int    `yaml:"repeated_len"`
	MapLen       []int    `yaml:"map_len"`
	Depth        *int     `yaml:"depth"`
	MessageProb  *float64 `yaml:"message_prob"`
	OptionalProb *float64 `yaml:"optional_prob"`
	OneofNone    *float64 `yaml:"oneof_none"`
}

// RPCHint is one method's hints.
type RPCHint struct {
	Weight     *int                 `yaml:"weight"`
	DeadlineMs int                  `yaml:"deadline_ms"`
	Target     string               `yaml:"target"`
	Retry      *RetryHint           `yaml:"retry"`
	OutcomeID  string               `yaml:"outcome_id"`
	Classify   map[string]string    `yaml:"classify"`
	Fields     map[string]FieldHint `yaml:"fields"`
	Oneofs     map[string][]string  `yaml:"oneofs"`
	StreamLen  []int                `yaml:"stream_len"`
}

// RetryHint: extra attempts after an unknown outcome.
type RetryHint struct {
	Attempts  int   `yaml:"attempts"`
	BackoffMs int   `yaml:"backoff_ms"`
	AtEnd     *bool `yaml:"at_end"`
}

// FieldHint is the YAML form of spec.FieldHint (values may be numbers, strings or booleans).
type FieldHint struct {
	Value  interface{}   `yaml:"value"`
	Choice []interface{} `yaml:"choice"`
	Range  []interface{} `yaml:"range"`
	Len    []int         `yaml:"len"`
	Unique bool          `yaml:"unique"`
	Prefix string        `yaml:"prefix"`
	Format string        `yaml:"format"`
	Omit   bool          `yaml:"omit"`
	Prob   *float64      `yaml:"prob"`
	From   interface{}   `yaml:"from"` // "Create.id" or {rpc, side, path, pick, fallback}
}

// ScenarioHint is a fixed call sequence.
type ScenarioHint struct {
	Name   string   `yaml:"name"`
	Weight int      `yaml:"weight"`
	Calls  []string `yaml:"calls"`
}

// ReadHints parses a hints file ("" = no hints).
func ReadHints(path string) (*Hints, error) {
	h := &Hints{}
	if path == "" {
		return h, nil
	}
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	d := yaml.NewDecoder(bytes.NewReader(b))
	d.KnownFields(true)
	if err := d.Decode(h); err != nil && err.Error() != "EOF" {
		return nil, fmt.Errorf("%s: %v", path, err)
	}
	return h, nil
}

// Build turns the schema and the hints into a plan.
func Build(files *protoregistry.Files, h *Hints, sources []string) (*spec.Plan, error) {
	p := &spec.Plan{
		Format: spec.Format, Target: "127.0.0.1:50051", DeadlineMs: 2000, Seed: 1,
		Steps: spec.Steps{Calls: 2, Concurrency: 1}, Drive: spec.Drive{Calls: 4},
		Defaults: spec.DefaultDefaults(), Record: spec.Record{Console: "brief"}, Sources: sources,
	}
	if h.Target != "" {
		p.Target = h.Target
	}
	if h.DeadlineMs > 0 {
		p.DeadlineMs = h.DeadlineMs
	}
	if h.Seed != nil {
		p.Seed = *h.Seed
	}
	if h.Steps != nil {
		if h.Steps.Calls < 1 || h.Steps.Calls > 1000 || h.Steps.Concurrency < 1 || h.Steps.Concurrency > 64 {
			return nil, fmt.Errorf("steps: calls must be 1..1000 and concurrency 1..64")
		}
		p.Steps = *h.Steps
	}
	if h.Drive != nil {
		if h.Drive.Calls < 1 || h.Drive.Calls > 1000 {
			return nil, fmt.Errorf("drive: calls must be 1..1000")
		}
		p.Drive = *h.Drive
	}
	if h.Record != nil {
		switch h.Record.Console {
		case "off", "brief", "full":
			p.Record = *h.Record
		default:
			return nil, fmt.Errorf("record.console: %q (off, brief, full)", h.Record.Console)
		}
	}
	if err := applyDefaults(&p.Defaults, h.Defaults); err != nil {
		return nil, err
	}

	// every service in the schema, in a fixed order
	var svcs []protoreflect.ServiceDescriptor
	files.RangeFiles(func(fd protoreflect.FileDescriptor) bool {
		for i := 0; i < fd.Services().Len(); i++ {
			svcs = append(svcs, fd.Services().Get(i))
		}
		return true
	})
	sort.Slice(svcs, func(i, j int) bool { return svcs[i].FullName() < svcs[j].FullName() })
	want := map[string]bool{}
	for _, s := range h.Services {
		want[s] = true
	}
	var mds []protoreflect.MethodDescriptor
	for _, s := range svcs {
		if skipService(string(s.FullName())) {
			continue
		}
		if len(want) > 0 && !want[string(s.FullName())] && !want[string(s.Name())] {
			continue
		}
		delete(want, string(s.FullName()))
		delete(want, string(s.Name()))
		for i := 0; i < s.Methods().Len(); i++ {
			mds = append(mds, s.Methods().Get(i))
		}
	}
	if len(want) > 0 {
		return nil, fmt.Errorf("services: %s not in the schema", strings.Join(sortedKeys(want), ", "))
	}
	if len(mds) == 0 {
		return nil, fmt.Errorf("the schema has no services (nothing to drive)")
	}
	// command names: the method in snake case, or service_method where two collide
	count := map[string]int{}
	for _, m := range mds {
		count[snake(string(m.Name()))]++
	}
	for _, m := range mds {
		full := string(m.Parent().FullName()) + "/" + string(m.Name())
		short := snake(string(m.Name()))
		if count[short] > 1 {
			short = snake(string(m.Parent().Name())) + "_" + short
		}
		p.RPCs = append(p.RPCs, spec.RPC{
			Name: full, Path: "/" + full, Short: short,
			Input: string(m.Input().FullName()), Output: string(m.Output().FullName()),
			ClientStreaming: m.IsStreamingClient(), ServerStreaming: m.IsStreamingServer(),
			Weight: 1, DeadlineMs: p.DeadlineMs, StreamLen: [2]int{1, 3},
		})
	}
	byMD := map[string]protoreflect.MethodDescriptor{}
	for _, m := range mds {
		byMD[string(m.Parent().FullName())+"/"+string(m.Name())] = m
	}

	used := map[string]bool{}
	for _, key := range sortedKeys(h.RPCs) {
		rh := h.RPCs[key]
		r, err := p.Find(key)
		if err != nil {
			return nil, fmt.Errorf("rpcs.%s: %v", key, err)
		}
		if used[r.Name] {
			return nil, fmt.Errorf("rpcs.%s: %s has hints twice", key, r.Name)
		}
		used[r.Name] = true
		if err := applyRPC(p, r, byMD[r.Name], rh, files); err != nil {
			return nil, fmt.Errorf("rpcs.%s: %v", key, err)
		}
	}
	for _, sh := range h.Scenarios {
		if sh.Name == "" || len(sh.Calls) == 0 {
			return nil, fmt.Errorf("scenarios: every scenario needs a name and calls")
		}
		s := spec.Scenario{Name: sh.Name, Weight: sh.Weight}
		if s.Weight == 0 {
			s.Weight = 1
		}
		for _, c := range sh.Calls {
			r, err := p.Find(c)
			if err != nil {
				return nil, fmt.Errorf("scenarios.%s: %v", sh.Name, err)
			}
			s.Calls = append(s.Calls, r.Name)
		}
		p.Scenarios = append(p.Scenarios, s)
	}
	return p, nil
}

func sortedKeys[V any](m map[string]V) []string {
	ks := make([]string, 0, len(m))
	for k := range m {
		ks = append(ks, k)
	}
	sort.Strings(ks)
	return ks
}

func snake(s string) string {
	var b strings.Builder
	rs := []rune(s)
	for i, c := range rs {
		if unicode.IsUpper(c) {
			if i > 0 && (unicode.IsLower(rs[i-1]) || unicode.IsDigit(rs[i-1]) || (i+1 < len(rs) && unicode.IsLower(rs[i+1]) && unicode.IsUpper(rs[i-1]))) {
				b.WriteByte('_')
			}
			b.WriteRune(unicode.ToLower(c))
		} else {
			b.WriteRune(c)
		}
	}
	return b.String()
}

func pair(xs []int, what string, min int) ([2]int, error) {
	if len(xs) != 2 || xs[0] < min || xs[1] < xs[0] {
		return [2]int{}, fmt.Errorf("%s: want [lo, hi] with %d <= lo <= hi", what, min)
	}
	return [2]int{xs[0], xs[1]}, nil
}

func applyDefaults(d *spec.Defaults, h *DefaultsHint) error {
	if h == nil {
		return nil
	}
	var err error
	for _, x := range []struct {
		v    []int
		dst  *[2]int
		name string
	}{{h.StringLen, &d.StringLen, "string_len"}, {h.BytesLen, &d.BytesLen, "bytes_len"},
		{h.RepeatedLen, &d.RepeatedLen, "repeated_len"}, {h.MapLen, &d.MapLen, "map_len"}} {
		if x.v != nil {
			if *x.dst, err = pair(x.v, "defaults."+x.name, 0); err != nil {
				return err
			}
		}
	}
	if h.Depth != nil {
		if *h.Depth < 0 || *h.Depth > 32 {
			return fmt.Errorf("defaults.depth: 0..32")
		}
		d.Depth = *h.Depth
	}
	for _, x := range []struct {
		v    *float64
		dst  *float64
		name string
	}{{h.MessageProb, &d.MessageProb, "message_prob"}, {h.OptionalProb, &d.OptionalProb, "optional_prob"}, {h.OneofNone, &d.OneofNone, "oneof_none"}} {
		if x.v != nil {
			if *x.v < 0 || *x.v > 1 {
				return fmt.Errorf("defaults.%s: 0..1", x.name)
			}
			*x.dst = *x.v
		}
	}
	return nil
}

func applyRPC(p *spec.Plan, r *spec.RPC, md protoreflect.MethodDescriptor, h RPCHint, files *protoregistry.Files) error {
	if h.Weight != nil {
		if *h.Weight < 0 {
			return fmt.Errorf("weight: >= 0")
		}
		r.Weight = *h.Weight
	}
	if h.DeadlineMs > 0 {
		r.DeadlineMs = h.DeadlineMs
	}
	r.Target = h.Target
	if h.Retry != nil {
		if h.Retry.Attempts < 0 || h.Retry.Attempts > 100 || h.Retry.BackoffMs < 0 {
			return fmt.Errorf("retry: attempts 0..100, backoff_ms >= 0")
		}
		r.Retry = spec.Retry{Attempts: h.Retry.Attempts, BackoffMs: h.Retry.BackoffMs, AtEnd: true}
		if h.Retry.AtEnd != nil {
			r.Retry.AtEnd = *h.Retry.AtEnd
		}
	}
	if h.StreamLen != nil {
		var err error
		if r.StreamLen, err = pair(h.StreamLen, "stream_len", 0); err != nil {
			return err
		}
	}
	in := md.Input()
	if h.OutcomeID != "" {
		fd, rep, err := resolve(in, h.OutcomeID)
		if err != nil {
			return fmt.Errorf("outcome_id: %v", err)
		}
		if rep || fd.Kind() == protoreflect.MessageKind {
			return fmt.Errorf("outcome_id: %s must be a single scalar field", h.OutcomeID)
		}
		r.OutcomeID = h.OutcomeID
	}
	for _, c := range sortedKeys(h.Classify) {
		if _, ok := outcome.ParseCode(c); !ok {
			return fmt.Errorf("classify: %q is not a gRPC code name (%s)", c, strings.Join(outcome.CodeNames(), " "))
		}
		if !outcome.ValidOverride(h.Classify[c]) {
			return fmt.Errorf("classify.%s: %q (ok, fail, info)", c, h.Classify[c])
		}
	}
	r.Classify = h.Classify
	for _, path := range sortedKeys(h.Oneofs) {
		od, err := resolveOneof(in, path)
		if err != nil {
			return fmt.Errorf("oneofs.%s: %v", path, err)
		}
		for _, m := range h.Oneofs[path] {
			if f := od.Fields().ByName(protoreflect.Name(m)); f == nil {
				return fmt.Errorf("oneofs.%s: %q is not a member of the oneof", path, m)
			}
		}
	}
	r.Oneofs = h.Oneofs
	if len(h.Fields) > 0 {
		r.Fields = map[string]spec.FieldHint{}
	}
	for _, path := range sortedKeys(h.Fields) {
		fd, _, err := resolve(in, path)
		if err != nil {
			return fmt.Errorf("fields.%s: %v", path, err)
		}
		fh, err := fieldHint(p, fd, h.Fields[path], files)
		if err != nil {
			return fmt.Errorf("fields.%s: %v", path, err)
		}
		r.Fields[path] = fh
		if fh.From != nil && fh.From.Fallback != "generate" {
			r.Needs = appendUniq(r.Needs, fh.From.Pool())
		}
	}
	return nil
}

func appendUniq(xs []string, x string) []string {
	for _, y := range xs {
		if y == x {
			return xs
		}
	}
	return append(xs, x)
}

// resolve walks a field path ("items.sku", "tags.key") from md. rep is true if any step is a
// repeated or map field.
func resolve(md protoreflect.MessageDescriptor, path string) (protoreflect.FieldDescriptor, bool, error) {
	segs := strings.Split(path, ".")
	cur := md
	rep := false
	var fd protoreflect.FieldDescriptor
	for i := 0; i < len(segs); i++ {
		if cur == nil {
			return nil, false, fmt.Errorf("%s: %s is not a message", path, strings.Join(segs[:i], "."))
		}
		fd = cur.Fields().ByName(protoreflect.Name(segs[i]))
		if fd == nil {
			return nil, false, fmt.Errorf("%s: no field %q in %s (fields: %s)", path, segs[i], cur.FullName(), fieldNames(cur))
		}
		if fd.IsList() {
			rep = true
		}
		if fd.IsMap() {
			rep = true
			if i+1 < len(segs) {
				i++
				switch segs[i] {
				case "key":
					fd = fd.MapKey()
				case "value":
					fd = fd.MapValue()
				default:
					return nil, false, fmt.Errorf("%s: after the map field %s comes key or value", path, segs[i-1])
				}
			}
		}
		cur = fd.Message()
	}
	return fd, rep, nil
}

func resolveOneof(md protoreflect.MessageDescriptor, path string) (protoreflect.OneofDescriptor, error) {
	cur := md
	segs := strings.Split(path, ".")
	if len(segs) > 1 {
		fd, _, err := resolve(md, strings.Join(segs[:len(segs)-1], "."))
		if err != nil {
			return nil, err
		}
		if cur = fd.Message(); cur == nil {
			return nil, fmt.Errorf("%s is not a message", strings.Join(segs[:len(segs)-1], "."))
		}
	}
	od := cur.Oneofs().ByName(protoreflect.Name(segs[len(segs)-1]))
	if od == nil || od.IsSynthetic() {
		return nil, fmt.Errorf("no oneof %q in %s", segs[len(segs)-1], cur.FullName())
	}
	return od, nil
}

func fieldNames(md protoreflect.MessageDescriptor) string {
	var ns []string
	for i := 0; i < md.Fields().Len(); i++ {
		ns = append(ns, string(md.Fields().Get(i).Name()))
	}
	return strings.Join(ns, ", ")
}

func str(v interface{}) (string, error) {
	switch x := v.(type) {
	case string:
		return x, nil
	case bool:
		return strconv.FormatBool(x), nil
	case int:
		return strconv.Itoa(x), nil
	case int64:
		return strconv.FormatInt(x, 10), nil
	case uint64:
		return strconv.FormatUint(x, 10), nil
	case float64:
		return strconv.FormatFloat(x, 'g', -1, 64), nil
	}
	return "", fmt.Errorf("%v (%T) is not a scalar value", v, v)
}

func numeric(k protoreflect.Kind) bool {
	switch k {
	case protoreflect.BoolKind, protoreflect.StringKind, protoreflect.BytesKind, protoreflect.EnumKind,
		protoreflect.MessageKind, protoreflect.GroupKind:
		return false
	}
	return true
}

func integer(k protoreflect.Kind) bool {
	return numeric(k) && k != protoreflect.FloatKind && k != protoreflect.DoubleKind
}

func fieldHint(p *spec.Plan, fd protoreflect.FieldDescriptor, h FieldHint, files *protoregistry.Files) (spec.FieldHint, error) {
	var out spec.FieldHint
	k := fd.Kind()
	isMsg := k == protoreflect.MessageKind || k == protoreflect.GroupKind
	scalarOnly := func(what string) error {
		if isMsg || fd.IsMap() {
			return fmt.Errorf("%s needs a scalar or enum field, %s is %s", what, fd.Name(), kindName(fd))
		}
		return nil
	}
	if h.Value != nil {
		if err := scalarOnly("value"); err != nil {
			return out, err
		}
		s, err := str(h.Value)
		if err != nil {
			return out, fmt.Errorf("value: %v", err)
		}
		if _, err := valuegen.Parse(fd, s); err != nil {
			return out, err
		}
		out.Value = &s
	}
	for _, c := range h.Choice {
		if err := scalarOnly("choice"); err != nil {
			return out, err
		}
		s, err := str(c)
		if err != nil {
			return out, fmt.Errorf("choice: %v", err)
		}
		if _, err := valuegen.Parse(fd, s); err != nil {
			return out, err
		}
		out.Choice = append(out.Choice, s)
	}
	if h.Range != nil {
		if !numeric(k) || fd.IsMap() {
			return out, fmt.Errorf("range needs a number field, %s is %s", fd.Name(), kindName(fd))
		}
		if len(h.Range) != 2 {
			return out, fmt.Errorf("range: want [lo, hi]")
		}
		for _, v := range h.Range {
			s, err := str(v)
			if err != nil {
				return out, fmt.Errorf("range: %v", err)
			}
			if _, err := valuegen.Parse(fd, s); err != nil {
				return out, fmt.Errorf("range: %v", err)
			}
			out.Range = append(out.Range, s)
		}
		// valuegen checks the order and bounds; check it here too for an early error
		if integer(k) {
			if strings.HasPrefix(out.Range[0], "-") || strings.HasPrefix(out.Range[1], "-") {
				a, _ := strconv.ParseInt(out.Range[0], 10, 64)
				b, _ := strconv.ParseInt(out.Range[1], 10, 64)
				if a > b {
					return out, fmt.Errorf("range: lo > hi")
				}
			} else {
				a, _ := strconv.ParseUint(out.Range[0], 10, 64)
				b, _ := strconv.ParseUint(out.Range[1], 10, 64)
				if a > b {
					return out, fmt.Errorf("range: lo > hi")
				}
			}
		} else {
			a, _ := strconv.ParseFloat(out.Range[0], 64)
			b, _ := strconv.ParseFloat(out.Range[1], 64)
			if a > b {
				return out, fmt.Errorf("range: lo > hi")
			}
		}
	}
	if h.Len != nil {
		if !(fd.IsList() || fd.IsMap() || k == protoreflect.StringKind || k == protoreflect.BytesKind) {
			return out, fmt.Errorf("len needs a string, bytes, repeated or map field, %s is %s", fd.Name(), kindName(fd))
		}
		pr, err := pair(h.Len, "len", 0)
		if err != nil {
			return out, err
		}
		out.Len = []int{pr[0], pr[1]}
	}
	if h.Unique {
		if !(k == protoreflect.StringKind || k == protoreflect.BytesKind || integer(k)) || fd.IsMap() {
			return out, fmt.Errorf("unique needs a string, bytes or integer field, %s is %s", fd.Name(), kindName(fd))
		}
		out.Unique = true
	}
	if h.Prefix != "" {
		if k != protoreflect.StringKind && k != protoreflect.BytesKind {
			return out, fmt.Errorf("prefix needs a string field")
		}
		out.Prefix = h.Prefix
	}
	if h.Format != "" {
		ok := false
		for _, f := range valuegen.Formats {
			ok = ok || f == h.Format
		}
		if !ok || k != protoreflect.StringKind {
			return out, fmt.Errorf("format %q: a string field and one of alnum lower digits hex uuid", h.Format)
		}
		out.Format = h.Format
	}
	out.Omit = h.Omit
	if h.Prob != nil {
		if *h.Prob < 0 || *h.Prob > 1 {
			return out, fmt.Errorf("prob: 0..1")
		}
		out.Prob = h.Prob
	}
	if h.From != nil {
		if err := scalarOnly("from"); err != nil {
			return out, err
		}
		f, err := parseFrom(h.From)
		if err != nil {
			return out, fmt.Errorf("from: %v", err)
		}
		src, err := p.Find(f.RPC)
		if err != nil {
			return out, fmt.Errorf("from: %v", err)
		}
		f.RPC = src.Name
		mdName := src.Output
		if f.Side == "request" {
			mdName = src.Input
		}
		d, err := files.FindDescriptorByName(protoreflect.FullName(mdName))
		if err != nil {
			return out, fmt.Errorf("from: %v", err)
		}
		sfd, _, err := resolve(d.(protoreflect.MessageDescriptor), f.Path)
		if err != nil {
			return out, fmt.Errorf("from: %v", err)
		}
		if !compatible(sfd, fd) {
			return out, fmt.Errorf("from: %s %s.%s is %s, this field is %s", src.Name, f.Side, f.Path, kindName(sfd), kindName(fd))
		}
		out.From = f
	}
	return out, nil
}

func kindName(fd protoreflect.FieldDescriptor) string {
	s := fd.Kind().String()
	switch {
	case fd.IsMap():
		return "a map"
	case fd.IsList():
		s = "repeated " + s
	}
	if m := fd.Message(); m != nil && !fd.IsMap() {
		s += " " + string(m.FullName())
	}
	if e := fd.Enum(); e != nil {
		s += " " + string(e.FullName())
	}
	return s
}

func compatible(src, dst protoreflect.FieldDescriptor) bool {
	sk, dk := src.Kind(), dst.Kind()
	switch {
	case src.IsMap() || dst.IsMap():
		return false
	case dk == protoreflect.StringKind:
		return sk != protoreflect.MessageKind && sk != protoreflect.GroupKind && sk != protoreflect.BytesKind
	case dk == protoreflect.EnumKind:
		return sk == protoreflect.EnumKind && src.Enum().FullName() == dst.Enum().FullName()
	case integer(dk):
		return integer(sk)
	case numeric(dk):
		return numeric(sk)
	}
	return sk == dk
}

func parseFrom(v interface{}) (*spec.From, error) {
	f := &spec.From{Side: "response", Pick: "random", Fallback: "skip"}
	switch x := v.(type) {
	case string:
		// "<rpc>.<path>", "<rpc>.response.<path>", "<rpc>.request.<path>"; <rpc> may be
		// "pkg.Service/Method" (the part after the first '.' that follows the '/') or a method name
		cut := 0
		if i := strings.Index(x, "/"); i >= 0 {
			cut = i
		}
		j := strings.Index(x[cut:], ".")
		if j < 0 {
			return nil, fmt.Errorf("%q: want <rpc>.<field path>", x)
		}
		f.RPC, f.Path = x[:cut+j], x[cut+j+1:]
		for _, side := range []string{"response", "request"} {
			if strings.HasPrefix(f.Path, side+".") {
				f.Side, f.Path = side, f.Path[len(side)+1:]
			}
		}
	case map[string]interface{}:
		for _, k := range sortedKeys(x) {
			s, err := str(x[k])
			if err != nil {
				return nil, fmt.Errorf("%s: %v", k, err)
			}
			switch k {
			case "rpc":
				f.RPC = s
			case "path":
				f.Path = s
			case "side":
				f.Side = s
			case "pick":
				f.Pick = s
			case "fallback":
				f.Fallback = s
			default:
				return nil, fmt.Errorf("unknown key %q (rpc, path, side, pick, fallback)", k)
			}
		}
	default:
		return nil, fmt.Errorf("want a string or a mapping")
	}
	if f.RPC == "" || f.Path == "" {
		return nil, fmt.Errorf("needs an rpc and a field path")
	}
	if f.Side != "response" && f.Side != "request" {
		return nil, fmt.Errorf("side %q (request, response)", f.Side)
	}
	if f.Pick != "random" && f.Pick != "latest" {
		return nil, fmt.Errorf("pick %q (random, latest)", f.Pick)
	}
	if f.Fallback != "skip" && f.Fallback != "generate" {
		return nil, fmt.Errorf("fallback %q (skip, generate)", f.Fallback)
	}
	return f, nil
}
