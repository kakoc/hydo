// Package valuegen fills protobuf messages with valid random values from a seeded stream.
//
// "Valid" means valid for the schema: every value fits its field's type (int32 in range, an
// enum value that is declared, at most one member of a oneof, distinct map keys, required
// proto2 fields set, nesting bounded). Business rules ("amount > 0", "an id that exists")
// come from hints (spec.FieldHint): ranges, choices, lengths, unique values, constants, and
// values taken from earlier responses ("from").
//
// Determinism: the result is a pure function of (descriptor, hints, the random stream, and
// what Env returns). Fields are visited in descriptor order and map keys are generated, not
// iterated, so nothing depends on Go's map order.
package valuegen

import (
	"encoding/base64"
	"errors"
	"fmt"
	"math"
	"strconv"
	"strings"

	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/dynamicpb"

	"hydogrpc/internal/rng"
	"hydogrpc/internal/spec"
)

// ErrSkip means a field's value must come from an earlier call (hint `from`, fallback skip)
// and no such value exists yet: the operation is not made now.
var ErrSkip = errors.New("valuegen: no earlier value for a `from` field yet")

// Env supplies what the generator cannot derive from the seed alone.
type Env interface {
	// Unique returns an integer no other call of Unique in this run returned.
	Unique() uint64
	// Take returns a value recorded for a `from` source (its canonical string form, see
	// Canonical), choosing among several with r; ok=false if there is none.
	Take(f *spec.From, r *rng.R) (string, bool)
}

// Gen generates one message type's values.
type Gen struct {
	D      spec.Defaults
	Hints  map[string]spec.FieldHint
	Oneofs map[string][]string
	Env    Env
}

// Message returns a new message of type md with every field generated.
func (g *Gen) Message(md protoreflect.MessageDescriptor, r *rng.R) (*dynamicpb.Message, error) {
	m := dynamicpb.NewMessage(md)
	if err := g.fill(m, "", 0, r); err != nil {
		return nil, err
	}
	return m, nil
}

func join(prefix, name string) string {
	if prefix == "" {
		return name
	}
	return prefix + "." + name
}

func (g *Gen) fill(m protoreflect.Message, prefix string, depth int, r *rng.R) error {
	md := m.Descriptor()
	if wkt(md, m, r) {
		return nil
	}
	// oneofs first: decide which member (if any) is set
	chosen := map[protoreflect.FieldNumber]bool{}
	ods := md.Oneofs()
	for i := 0; i < ods.Len(); i++ {
		od := ods.Get(i)
		if od.IsSynthetic() {
			continue // proto3 `optional`: handled as a field with presence
		}
		var members []protoreflect.FieldDescriptor
		allowed := g.Oneofs[join(prefix, string(od.Name()))]
		for j := 0; j < od.Fields().Len(); j++ {
			fd := od.Fields().Get(j)
			if h, ok := g.Hints[join(prefix, string(fd.Name()))]; ok && h.Omit {
				continue
			}
			if len(allowed) > 0 && !contains(allowed, string(fd.Name())) {
				continue
			}
			if fd.Kind() == protoreflect.MessageKind && depth+1 > g.D.Depth && !g.hinted(join(prefix, string(fd.Name()))) {
				continue
			}
			members = append(members, fd)
		}
		if len(members) == 0 || (g.D.OneofNone > 0 && r.Chance(g.D.OneofNone)) {
			continue
		}
		chosen[members[r.Intn(len(members))].Number()] = true
	}
	fds := md.Fields()
	for i := 0; i < fds.Len(); i++ {
		fd := fds.Get(i)
		path := join(prefix, string(fd.Name()))
		h := g.Hints[path]
		if h.Omit && fd.Cardinality() != protoreflect.Required {
			continue
		}
		if od := fd.ContainingOneof(); od != nil && !od.IsSynthetic() {
			if !chosen[fd.Number()] {
				continue
			}
		} else if !g.present(fd, path, h, depth, r) {
			continue
		}
		if err := g.field(m, fd, path, h, depth, r); err != nil {
			return err
		}
	}
	return nil
}

// hinted: the hint fixes the value (so presence is not left to chance)
func (g *Gen) hinted(path string) bool {
	h, ok := g.Hints[path]
	return ok && (h.Value != nil || len(h.Choice) > 0 || h.From != nil || h.Unique || len(h.Range) > 0)
}

func (g *Gen) present(fd protoreflect.FieldDescriptor, path string, h spec.FieldHint, depth int, r *rng.R) bool {
	if fd.Cardinality() == protoreflect.Required {
		return true
	}
	if fd.IsList() || fd.IsMap() {
		return true // the length decides (0 = absent)
	}
	isMsg := fd.Kind() == protoreflect.MessageKind || fd.Kind() == protoreflect.GroupKind
	if isMsg && depth+1 > g.D.Depth && !g.hinted(path) {
		return false
	}
	if !fd.HasPresence() {
		return true // proto3 implicit scalar: always has a value (maybe the zero value)
	}
	if h.Prob != nil {
		return r.Chance(*h.Prob)
	}
	if g.hinted(path) {
		return true
	}
	if isMsg {
		return r.Chance(g.D.MessageProb)
	}
	return r.Chance(g.D.OptionalProb)
}

func (g *Gen) field(m protoreflect.Message, fd protoreflect.FieldDescriptor, path string, h spec.FieldHint, depth int, r *rng.R) error {
	switch {
	case fd.IsMap():
		n := g.length(h, g.D.MapLen, r)
		mp := m.Mutable(fd).Map()
		kfd, vfd := fd.MapKey(), fd.MapValue()
		kh, vh := g.Hints[path+".key"], g.Hints[path+".value"]
		for tries := 0; mp.Len() < n && tries < 8*n+8; tries++ {
			kv, err := g.scalar(kfd, path+".key", kh, r)
			if err != nil {
				return err
			}
			mk := kv.MapKey()
			if mp.Has(mk) {
				continue
			}
			if vfd.Kind() == protoreflect.MessageKind {
				vm := mp.NewValue()
				if err := g.fill(vm.Message(), path+".value", depth+1, r); err != nil {
					return err
				}
				mp.Set(mk, vm)
			} else {
				vv, err := g.scalar(vfd, path+".value", vh, r)
				if err != nil {
					return err
				}
				mp.Set(mk, vv)
			}
		}
	case fd.IsList():
		n := g.length(h, g.D.RepeatedLen, r)
		l := m.Mutable(fd).List()
		for k := 0; k < n; k++ {
			if fd.Kind() == protoreflect.MessageKind || fd.Kind() == protoreflect.GroupKind {
				if depth+1 > g.D.Depth {
					break
				}
				e := l.NewElement()
				if err := g.fill(e.Message(), path, depth+1, r); err != nil {
					return err
				}
				l.Append(e)
			} else {
				v, err := g.scalar(fd, path, h, r)
				if err != nil {
					return err
				}
				l.Append(v)
			}
		}
	case fd.Kind() == protoreflect.MessageKind || fd.Kind() == protoreflect.GroupKind:
		sub := m.Mutable(fd).Message()
		if err := g.fill(sub, path, depth+1, r); err != nil {
			return err
		}
	default:
		v, err := g.scalar(fd, path, h, r)
		if err != nil {
			return err
		}
		m.Set(fd, v)
	}
	return nil
}

func (g *Gen) length(h spec.FieldHint, def [2]int, r *rng.R) int {
	lo, hi := def[0], def[1]
	if len(h.Len) == 2 {
		lo, hi = h.Len[0], h.Len[1]
	}
	return int(r.Range(int64(lo), int64(hi)))
}

func contains(xs []string, x string) bool {
	for _, y := range xs {
		if y == x {
			return true
		}
	}
	return false
}

// scalar generates one non-message value of fd's kind.
func (g *Gen) scalar(fd protoreflect.FieldDescriptor, path string, h spec.FieldHint, r *rng.R) (protoreflect.Value, error) {
	if h.From != nil {
		if s, ok := g.Env.Take(h.From, r); ok {
			return Parse(fd, s)
		}
		if h.From.Fallback != "generate" {
			return protoreflect.Value{}, ErrSkip
		}
	}
	if h.Value != nil {
		return Parse(fd, *h.Value)
	}
	if len(h.Choice) > 0 {
		return Parse(fd, h.Choice[r.Intn(len(h.Choice))])
	}
	if h.Unique {
		return g.unique(fd, h)
	}
	switch fd.Kind() {
	case protoreflect.BoolKind:
		return protoreflect.ValueOfBool(r.Intn(2) == 1), nil
	case protoreflect.EnumKind:
		return g.enum(fd, r), nil
	case protoreflect.Int32Kind, protoreflect.Sint32Kind, protoreflect.Sfixed32Kind:
		lo, hi, err := intRange(h, math.MinInt32, math.MaxInt32)
		if err != nil {
			return protoreflect.Value{}, fmt.Errorf("%s: %v", path, err)
		}
		return protoreflect.ValueOfInt32(int32(signed(r, lo, hi, len(h.Range) == 2))), nil
	case protoreflect.Int64Kind, protoreflect.Sint64Kind, protoreflect.Sfixed64Kind:
		lo, hi, err := intRange(h, math.MinInt64, math.MaxInt64)
		if err != nil {
			return protoreflect.Value{}, fmt.Errorf("%s: %v", path, err)
		}
		return protoreflect.ValueOfInt64(signed(r, lo, hi, len(h.Range) == 2)), nil
	case protoreflect.Uint32Kind, protoreflect.Fixed32Kind:
		lo, hi, err := uintRange(h, math.MaxUint32)
		if err != nil {
			return protoreflect.Value{}, fmt.Errorf("%s: %v", path, err)
		}
		return protoreflect.ValueOfUint32(uint32(unsigned(r, lo, hi, len(h.Range) == 2))), nil
	case protoreflect.Uint64Kind, protoreflect.Fixed64Kind:
		lo, hi, err := uintRange(h, math.MaxUint64)
		if err != nil {
			return protoreflect.Value{}, fmt.Errorf("%s: %v", path, err)
		}
		return protoreflect.ValueOfUint64(unsigned(r, lo, hi, len(h.Range) == 2)), nil
	case protoreflect.FloatKind, protoreflect.DoubleKind:
		f, err := float(h, r, fd.Kind() == protoreflect.FloatKind)
		if err != nil {
			return protoreflect.Value{}, fmt.Errorf("%s: %v", path, err)
		}
		if fd.Kind() == protoreflect.FloatKind {
			return protoreflect.ValueOfFloat32(float32(f)), nil
		}
		return protoreflect.ValueOfFloat64(f), nil
	case protoreflect.StringKind:
		n := g.length(h, g.D.StringLen, r)
		return protoreflect.ValueOfString(h.Prefix + randString(r, h.Format, n)), nil
	case protoreflect.BytesKind:
		n := g.length(h, g.D.BytesLen, r)
		b := make([]byte, n)
		for i := range b {
			b[i] = byte(r.Uint64())
		}
		return protoreflect.ValueOfBytes(b), nil
	}
	return protoreflect.Value{}, fmt.Errorf("%s: unsupported kind %v", path, fd.Kind())
}

func (g *Gen) unique(fd protoreflect.FieldDescriptor, h spec.FieldHint) (protoreflect.Value, error) {
	n := g.Env.Unique()
	switch fd.Kind() {
	case protoreflect.StringKind:
		return protoreflect.ValueOfString(h.Prefix + strconv.FormatUint(n, 10)), nil
	case protoreflect.BytesKind:
		return protoreflect.ValueOfBytes([]byte(h.Prefix + strconv.FormatUint(n, 10))), nil
	}
	base := "1"
	if len(h.Range) == 2 {
		base = h.Range[0]
	}
	lo, err := strconv.ParseInt(base, 10, 64)
	if err != nil {
		return protoreflect.Value{}, fmt.Errorf("unique: range %q: %v", base, err)
	}
	// the first unique value is the range's lower bound (default 1), then lo+1, ...
	v := strconv.FormatInt(lo+int64(n)-1, 10)
	if len(h.Range) == 2 {
		hi, _ := strconv.ParseInt(h.Range[1], 10, 64)
		if lo+int64(n)-1 > hi {
			return protoreflect.Value{}, fmt.Errorf("unique: more values than the range [%s, %s] holds", h.Range[0], h.Range[1])
		}
	}
	return Parse(fd, v)
}

// enum: a declared value; NAME_UNSPECIFIED / number 0 named *UNSPECIFIED is left out (the
// proto style guide's "no value" marker, usually rejected by servers) unless it is the only one.
func (g *Gen) enum(fd protoreflect.FieldDescriptor, r *rng.R) protoreflect.Value {
	vs := fd.Enum().Values()
	var ok []protoreflect.EnumNumber
	for i := 0; i < vs.Len(); i++ {
		v := vs.Get(i)
		if v.Number() == 0 && strings.HasSuffix(string(v.Name()), "UNSPECIFIED") {
			continue
		}
		ok = append(ok, v.Number())
	}
	if len(ok) == 0 {
		return protoreflect.ValueOfEnum(vs.Get(0).Number())
	}
	return protoreflect.ValueOfEnum(ok[r.Intn(len(ok))])
}

func intRange(h spec.FieldHint, min, max int64) (int64, int64, error) {
	if len(h.Range) != 2 {
		return min, max, nil
	}
	lo, err := strconv.ParseInt(h.Range[0], 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("range %q: %v", h.Range[0], err)
	}
	hi, err := strconv.ParseInt(h.Range[1], 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("range %q: %v", h.Range[1], err)
	}
	if lo < min || hi > max || lo > hi {
		return 0, 0, fmt.Errorf("range [%d, %d] does not fit the field (%d..%d)", lo, hi, min, max)
	}
	return lo, hi, nil
}

func uintRange(h spec.FieldHint, max uint64) (uint64, uint64, error) {
	if len(h.Range) != 2 {
		return 0, max, nil
	}
	lo, err := strconv.ParseUint(h.Range[0], 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("range %q: %v", h.Range[0], err)
	}
	hi, err := strconv.ParseUint(h.Range[1], 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("range %q: %v", h.Range[1], err)
	}
	if hi > max || lo > hi {
		return 0, 0, fmt.Errorf("range [%d, %d] does not fit the field (0..%d)", lo, hi, max)
	}
	return lo, hi, nil
}

// signed: with an explicit range, uniform in it. Without one, mostly small non-negative
// values (what most APIs accept), sometimes larger ones, the edges, and a few negatives.
func signed(r *rng.R, lo, hi int64, explicit bool) int64 {
	if explicit {
		return r.Range(lo, hi)
	}
	switch p := r.Intn(100); {
	case p < 70:
		return r.Range(0, 100)
	case p < 85:
		return r.Range(0, 1000000)
	case p < 95:
		edges := []int64{0, 1, hi}
		return edges[r.Intn(len(edges))]
	default:
		return r.Range(-100, -1)
	}
}

func unsigned(r *rng.R, lo, hi uint64, explicit bool) uint64 {
	if explicit {
		return r.URange(lo, hi)
	}
	switch p := r.Intn(100); {
	case p < 70:
		return r.URange(0, 100)
	case p < 90:
		return r.URange(0, 1000000)
	default:
		edges := []uint64{0, 1, hi}
		return edges[r.Intn(len(edges))]
	}
}

func float(h spec.FieldHint, r *rng.R, f32 bool) (float64, error) {
	if len(h.Range) == 2 {
		lo, err := strconv.ParseFloat(h.Range[0], 64)
		if err != nil {
			return 0, fmt.Errorf("range %q: %v", h.Range[0], err)
		}
		hi, err := strconv.ParseFloat(h.Range[1], 64)
		if err != nil {
			return 0, fmt.Errorf("range %q: %v", h.Range[1], err)
		}
		if lo > hi {
			return 0, fmt.Errorf("range [%v, %v] is empty", lo, hi)
		}
		return lo + r.Float()*(hi-lo), nil
	}
	switch p := r.Intn(100); {
	case p < 70:
		return math.Round(r.Float()*10000) / 100, nil // 0.00 .. 100.00
	case p < 90:
		return (r.Float()*2 - 1) * 1e6, nil
	default:
		edges := []float64{0, 1, 0.5}
		return edges[r.Intn(len(edges))], nil
	}
}

const (
	alnum  = "abcdefghijklmnopqrstuvwxyz0123456789"
	lower  = "abcdefghijklmnopqrstuvwxyz"
	digits = "0123456789"
	hexd   = "0123456789abcdef"
)

func randString(r *rng.R, format string, n int) string {
	set := alnum
	switch format {
	case "lower":
		set = lower
	case "digits":
		set = digits
	case "hex":
		set = hexd
	case "uuid":
		b := make([]byte, 16)
		for i := range b {
			b[i] = byte(r.Uint64())
		}
		b[6] = b[6]&0x0f | 0x40
		b[8] = b[8]&0x3f | 0x80
		return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
	}
	var sb strings.Builder
	for i := 0; i < n; i++ {
		sb.WriteByte(set[r.Intn(len(set))])
	}
	return sb.String()
}

// Formats lists the accepted string formats.
var Formats = []string{"", "alnum", "lower", "digits", "hex", "uuid"}

// Canonical renders a scalar value as the string form Parse reads (pools, choices, records).
func Canonical(fd protoreflect.FieldDescriptor, v protoreflect.Value) string {
	switch fd.Kind() {
	case protoreflect.BoolKind:
		return strconv.FormatBool(v.Bool())
	case protoreflect.EnumKind:
		if ev := fd.Enum().Values().ByNumber(v.Enum()); ev != nil {
			return string(ev.Name())
		}
		return strconv.FormatInt(int64(v.Enum()), 10)
	case protoreflect.Int32Kind, protoreflect.Sint32Kind, protoreflect.Sfixed32Kind,
		protoreflect.Int64Kind, protoreflect.Sint64Kind, protoreflect.Sfixed64Kind:
		return strconv.FormatInt(v.Int(), 10)
	case protoreflect.Uint32Kind, protoreflect.Fixed32Kind, protoreflect.Uint64Kind, protoreflect.Fixed64Kind:
		return strconv.FormatUint(v.Uint(), 10)
	case protoreflect.FloatKind:
		return strconv.FormatFloat(v.Float(), 'g', -1, 32)
	case protoreflect.DoubleKind:
		return strconv.FormatFloat(v.Float(), 'g', -1, 64)
	case protoreflect.StringKind:
		return v.String()
	case protoreflect.BytesKind:
		return base64.StdEncoding.EncodeToString(v.Bytes())
	}
	return ""
}

// Parse reads a value of fd's kind from its string form: decimal numbers, true/false, enum
// value names (or numbers), base64 for bytes (or the raw text if it is not valid base64).
func Parse(fd protoreflect.FieldDescriptor, s string) (protoreflect.Value, error) {
	bad := func(err error) (protoreflect.Value, error) {
		return protoreflect.Value{}, fmt.Errorf("field %s: value %q: %v", fd.FullName(), s, err)
	}
	switch fd.Kind() {
	case protoreflect.BoolKind:
		b, err := strconv.ParseBool(s)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfBool(b), nil
	case protoreflect.EnumKind:
		if ev := fd.Enum().Values().ByName(protoreflect.Name(s)); ev != nil {
			return protoreflect.ValueOfEnum(ev.Number()), nil
		}
		n, err := strconv.ParseInt(s, 10, 32)
		if err != nil || fd.Enum().Values().ByNumber(protoreflect.EnumNumber(n)) == nil {
			return bad(fmt.Errorf("not a value of enum %s", fd.Enum().FullName()))
		}
		return protoreflect.ValueOfEnum(protoreflect.EnumNumber(n)), nil
	case protoreflect.Int32Kind, protoreflect.Sint32Kind, protoreflect.Sfixed32Kind:
		n, err := strconv.ParseInt(s, 10, 32)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfInt32(int32(n)), nil
	case protoreflect.Int64Kind, protoreflect.Sint64Kind, protoreflect.Sfixed64Kind:
		n, err := strconv.ParseInt(s, 10, 64)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfInt64(n), nil
	case protoreflect.Uint32Kind, protoreflect.Fixed32Kind:
		n, err := strconv.ParseUint(s, 10, 32)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfUint32(uint32(n)), nil
	case protoreflect.Uint64Kind, protoreflect.Fixed64Kind:
		n, err := strconv.ParseUint(s, 10, 64)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfUint64(n), nil
	case protoreflect.FloatKind:
		f, err := strconv.ParseFloat(s, 32)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfFloat32(float32(f)), nil
	case protoreflect.DoubleKind:
		f, err := strconv.ParseFloat(s, 64)
		if err != nil {
			return bad(err)
		}
		return protoreflect.ValueOfFloat64(f), nil
	case protoreflect.StringKind:
		return protoreflect.ValueOfString(s), nil
	case protoreflect.BytesKind:
		if b, err := base64.StdEncoding.DecodeString(s); err == nil {
			return protoreflect.ValueOfBytes(b), nil
		}
		return protoreflect.ValueOfBytes([]byte(s)), nil
	}
	return bad(fmt.Errorf("kind %v has no string form", fd.Kind()))
}

// wkt fills the well-known types whose generic fill would be meaningless. It returns true if
// it handled m.
func wkt(md protoreflect.MessageDescriptor, m protoreflect.Message, r *rng.R) bool {
	switch md.FullName() {
	case "google.protobuf.Timestamp":
		// 2026-01-01 .. +30 days, whole seconds: the guest clock starts at 2026-01-01
		m.Set(md.Fields().ByName("seconds"), protoreflect.ValueOfInt64(1767225600+r.Range(0, 30*86400)))
		return true
	case "google.protobuf.Duration":
		m.Set(md.Fields().ByName("seconds"), protoreflect.ValueOfInt64(r.Range(0, 3600)))
		return true
	case "google.protobuf.Any":
		return true // empty: there is no type to put in it
	case "google.protobuf.Struct", "google.protobuf.ListValue":
		return true // an empty JSON object / list: valid and small
	case "google.protobuf.Value":
		m.Set(md.Fields().ByName("null_value"), protoreflect.ValueOfEnum(0)) // JSON null
		return true
	}
	return false
}
