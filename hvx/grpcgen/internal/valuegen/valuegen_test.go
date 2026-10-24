package valuegen_test

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/dynamicpb"

	"hydogrpc/internal/load"
	"hydogrpc/internal/rng"
	"hydogrpc/internal/spec"
	"hydogrpc/internal/valuegen"
)

func files(t *testing.T) *protoregistry.Files {
	t.Helper()
	set, _, err := load.FromProtoDir("../../testdata/kitchen", nil)
	if err != nil {
		t.Fatal(err)
	}
	f, err := protodesc.NewFiles(set)
	if err != nil {
		t.Fatal(err)
	}
	return f
}

func msg(t *testing.T, f *protoregistry.Files, name string) protoreflect.MessageDescriptor {
	d, err := f.FindDescriptorByName(protoreflect.FullName(name))
	if err != nil {
		t.Fatal(err)
	}
	return d.(protoreflect.MessageDescriptor)
}

type env struct {
	n    uint64
	pool map[string][]string
}

func (e *env) Unique() uint64 { e.n++; return e.n }
func (e *env) Take(f *spec.From, r *rng.R) (string, bool) {
	vs := e.pool[f.Pool()]
	if len(vs) == 0 {
		return "", false
	}
	return vs[r.Intn(len(vs))], true
}

func gen(t *testing.T, md protoreflect.MessageDescriptor, hints map[string]spec.FieldHint, oneofs map[string][]string, seed uint64, n int) []*dynamicpb.Message {
	t.Helper()
	g := &valuegen.Gen{D: spec.DefaultDefaults(), Hints: hints, Oneofs: oneofs, Env: &env{}}
	r := rng.New(seed)
	var out []*dynamicpb.Message
	for i := 0; i < n; i++ {
		m, err := g.Message(md, r)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, m)
	}
	return out
}

func wire(t *testing.T, ms []*dynamicpb.Message) []byte {
	var b bytes.Buffer
	for _, m := range ms {
		x, err := proto.MarshalOptions{Deterministic: true}.Marshal(m)
		if err != nil {
			t.Fatal(err)
		}
		fmt.Fprintf(&b, "%d:", len(x))
		b.Write(x)
	}
	return b.Bytes()
}

// The same seed gives the same messages, byte for byte; another seed gives others.
func TestDeterministic(t *testing.T) {
	f := files(t)
	md := msg(t, f, "kitchen.Sink")
	a := wire(t, gen(t, md, nil, nil, 42, 300))
	b := wire(t, gen(t, md, nil, nil, 42, 300))
	c := wire(t, gen(t, md, nil, nil, 43, 300))
	if !bytes.Equal(a, b) {
		t.Fatal("same seed, different messages")
	}
	if bytes.Equal(a, c) {
		t.Fatal("different seeds, same messages")
	}
	// independent streams from one seed do not collide
	if rng.Derive(1, 2, 3) == rng.Derive(1, 3, 2) || rng.Derive(1, 2) == rng.Derive(2, 1) {
		t.Fatal("Derive is symmetric in its labels")
	}
}

// Every generated message is valid for its schema.
func TestValid(t *testing.T) {
	f := files(t)
	md := msg(t, f, "kitchen.Sink")
	seen := map[string]bool{}
	for _, m := range gen(t, md, nil, nil, 7, 2000) {
		if err := proto.CheckInitialized(m); err != nil {
			t.Fatalf("required field missing: %v", err) // legacy.Old has required fields
		}
		// round-trips through JSON and the wire format (catches invalid UTF-8, bad enums, ...)
		js, err := protojson.Marshal(m)
		if err != nil {
			t.Fatalf("protojson: %v", err)
		}
		back := dynamicpb.NewMessage(md)
		if err := protojson.Unmarshal(js, back); err != nil {
			t.Fatalf("protojson back: %v (%s)", err, js)
		}
		if !proto.Equal(m, back) {
			t.Fatalf("JSON round trip changed the message: %s", js)
		}
		b, _ := proto.Marshal(m)
		back = dynamicpb.NewMessage(md)
		if err := proto.Unmarshal(b, back); err != nil || !proto.Equal(m, back) {
			t.Fatalf("wire round trip: %v", err)
		}
		checkMsg(t, m.ProtoReflect(), 0, seen)
	}
	for _, want := range []string{"choice:c_str", "choice:c_leaf", "choice:c_num", "opt:set", "opt:unset",
		"color:RED", "color:GREEN", "color:BLUE", "map:ms", "map:ml", "map:mb", "neg", "edge-max"} {
		if !seen[want] {
			t.Errorf("never generated %s", want)
		}
	}
}

func checkMsg(t *testing.T, m protoreflect.Message, depth int, seen map[string]bool) {
	md := m.Descriptor()
	if depth > spec.DefaultDefaults().Depth+1 {
		t.Fatalf("nesting deeper than the limit in %s", md.FullName())
	}
	ods := md.Oneofs()
	for i := 0; i < ods.Len(); i++ {
		od := ods.Get(i)
		if od.IsSynthetic() {
			if m.Has(od.Fields().Get(0)) {
				seen[string(od.Fields().Get(0).Name())+":set"] = true
			} else {
				seen[string(od.Fields().Get(0).Name())+":unset"] = true
			}
			continue
		}
		set := 0
		for j := 0; j < od.Fields().Len(); j++ {
			if m.Has(od.Fields().Get(j)) {
				set++
				seen["choice:"+string(od.Fields().Get(j).Name())] = true
			}
		}
		if set > 1 {
			t.Fatalf("oneof %s has %d members set", od.FullName(), set)
		}
	}
	m.Range(func(fd protoreflect.FieldDescriptor, v protoreflect.Value) bool {
		switch {
		case fd.IsMap():
			seen["map:"+string(fd.Name())] = true
			v.Map().Range(func(k protoreflect.MapKey, mv protoreflect.Value) bool {
				if fd.MapValue().Message() != nil {
					checkMsg(t, mv.Message(), depth+1, seen)
				}
				return true
			})
		case fd.IsList():
			for i := 0; i < v.List().Len(); i++ {
				if fd.Message() != nil {
					checkMsg(t, v.List().Get(i).Message(), depth+1, seen)
				} else {
					checkScalar(t, fd, v.List().Get(i), seen)
				}
			}
		case fd.Message() != nil:
			checkMsg(t, v.Message(), depth+1, seen)
		default:
			checkScalar(t, fd, v, seen)
		}
		return true
	})
}

func checkScalar(t *testing.T, fd protoreflect.FieldDescriptor, v protoreflect.Value, seen map[string]bool) {
	switch fd.Kind() {
	case protoreflect.EnumKind:
		ev := fd.Enum().Values().ByNumber(v.Enum())
		if ev == nil {
			t.Fatalf("%s: undeclared enum value %d", fd.FullName(), v.Enum())
		}
		if strings.HasSuffix(string(ev.Name()), "UNSPECIFIED") {
			t.Fatalf("%s: generated the UNSPECIFIED value", fd.FullName())
		}
		if fd.Name() == "color" {
			seen["color:"+string(ev.Name())] = true
		}
	case protoreflect.Int32Kind, protoreflect.Sint32Kind, protoreflect.Sfixed32Kind:
		if v.Int() < math.MinInt32 || v.Int() > math.MaxInt32 {
			t.Fatalf("%s: %d out of int32", fd.FullName(), v.Int())
		}
		if v.Int() < 0 {
			seen["neg"] = true
		}
		if v.Int() == math.MaxInt32 {
			seen["edge-max"] = true
		}
	case protoreflect.Uint32Kind, protoreflect.Fixed32Kind:
		if v.Uint() > math.MaxUint32 {
			t.Fatalf("%s: %d out of uint32", fd.FullName(), v.Uint())
		}
	case protoreflect.FloatKind, protoreflect.DoubleKind:
		if math.IsNaN(v.Float()) || math.IsInf(v.Float(), 0) {
			t.Fatalf("%s: %v", fd.FullName(), v.Float())
		}
	}
}

func str(s string) *string { return &s }

// Hints narrow the values: ranges, choices, lengths, constants, unique values, formats,
// omitted fields, allowed oneof members, values from earlier calls.
func TestHints(t *testing.T) {
	f := files(t)
	md := msg(t, f, "kitchen.Sink")
	p := 1.0
	hints := map[string]spec.FieldHint{
		"i32":           {Range: []string{"10", "20"}},
		"u64":           {Range: []string{"5", "5"}},
		"db":            {Range: []string{"-1.5", "1.5"}},
		"s":             {Choice: []string{"x", "y"}},
		"by":            {Len: []int{4, 4}},
		"color":         {Choice: []string{"BLUE"}},
		"leaf":          {Prob: &p},
		"leaf.name":     {Unique: true, Prefix: "id-"},
		"rl":            {Len: []int{2, 2}},
		"rl.n":          {Value: str("-7")},
		"ri":            {Len: []int{0, 0}},
		"ms":            {Len: []int{3, 3}},
		"ms.key":        {Format: "digits", Len: []int{6, 6}},
		"ms.value":      {Range: []string{"1", "3"}},
		"opt":           {Omit: true},
		"tree":          {Omit: true},
		"wrapped.value": {Format: "uuid"},
	}
	seenNames := map[string]bool{}
	for _, m := range gen(t, md, hints, map[string][]string{"choice": {"c_num"}}, 9, 500) {
		r := m.ProtoReflect()
		get := func(n string) protoreflect.Value { return r.Get(md.Fields().ByName(protoreflect.Name(n))) }
		if v := get("i32").Int(); v < 10 || v > 20 {
			t.Fatalf("i32 %d not in [10,20]", v)
		}
		if get("u64").Uint() != 5 {
			t.Fatal("u64 not 5")
		}
		if v := get("db").Float(); v < -1.5 || v > 1.5 {
			t.Fatalf("db %v", v)
		}
		if s := get("s").String(); s != "x" && s != "y" {
			t.Fatalf("s %q", s)
		}
		if len(get("by").Bytes()) != 4 {
			t.Fatal("by length")
		}
		if get("color").Enum() != 5 {
			t.Fatal("color not BLUE")
		}
		if !r.Has(md.Fields().ByName("leaf")) {
			t.Fatal("leaf with prob 1 unset")
		}
		name := get("leaf").Message().Get(msg(t, f, "kitchen.Leaf").Fields().ByName("name")).String()
		if !strings.HasPrefix(name, "id-") || seenNames[name] {
			t.Fatalf("leaf.name %q not unique / no prefix", name)
		}
		seenNames[name] = true
		rl := get("rl").List()
		if rl.Len() != 2 || rl.Get(0).Message().Get(msg(t, f, "kitchen.Leaf").Fields().ByName("n")).Int() != -7 {
			t.Fatal("rl hints")
		}
		if get("ri").List().Len() != 0 {
			t.Fatal("ri not empty")
		}
		ms := get("ms").Map()
		if ms.Len() != 3 {
			t.Fatalf("ms has %d entries", ms.Len())
		}
		ms.Range(func(k protoreflect.MapKey, v protoreflect.Value) bool {
			if len(k.String()) != 6 || strings.Trim(k.String(), "0123456789") != "" || v.Int() < 1 || v.Int() > 3 {
				t.Fatalf("ms entry %q=%d", k.String(), v.Int())
			}
			return true
		})
		if r.Has(md.Fields().ByName("opt")) || r.Has(md.Fields().ByName("tree")) {
			t.Fatal("omitted field set")
		}
		if !r.Has(md.Fields().ByName("c_num")) {
			t.Fatal("oneof member outside the allowed set")
		}
		if r.Has(md.Fields().ByName("wrapped")) {
			u := get("wrapped").Message().Get(msg(t, f, "google.protobuf.StringValue").Fields().ByName("value")).String()
			if len(u) != 36 || u[14] != '4' {
				t.Fatalf("not a uuid: %q", u)
			}
		}
	}
}

func TestFrom(t *testing.T) {
	f := files(t)
	md := msg(t, f, "kitchen.Ids")
	from := &spec.From{RPC: "kitchen.Kitchen/Put", Side: "response", Path: "id", Fallback: "skip", Pick: "random"}
	hints := map[string]spec.FieldHint{"id": {From: from}}
	g := &valuegen.Gen{D: spec.DefaultDefaults(), Hints: hints, Env: &env{pool: map[string][]string{}}}
	if _, err := g.Message(md, rng.New(1)); !errors.Is(err, valuegen.ErrSkip) {
		t.Fatalf("empty pool: want ErrSkip, got %v", err)
	}
	e := &env{pool: map[string][]string{from.Pool(): {"a", "b", "c"}}}
	g.Env = e
	got := map[string]bool{}
	r := rng.New(2)
	for i := 0; i < 100; i++ {
		m, err := g.Message(md, r)
		if err != nil {
			t.Fatal(err)
		}
		got[m.ProtoReflect().Get(md.Fields().ByName("id")).String()] = true
	}
	if len(got) != 3 || !got["a"] || !got["b"] || !got["c"] {
		t.Fatalf("pool values not used: %v", got)
	}
	gen := *from
	gen.Fallback = "generate"
	g = &valuegen.Gen{D: spec.DefaultDefaults(), Hints: map[string]spec.FieldHint{"id": {From: &gen}}, Env: &env{pool: map[string][]string{}}}
	if _, err := g.Message(md, rng.New(1)); err != nil {
		t.Fatalf("fallback generate: %v", err)
	}
}

func TestCanonicalParse(t *testing.T) {
	f := files(t)
	md := msg(t, f, "kitchen.Sink")
	for _, m := range gen(t, md, nil, nil, 3, 200) {
		m.ProtoReflect().Range(func(fd protoreflect.FieldDescriptor, v protoreflect.Value) bool {
			if fd.IsList() || fd.IsMap() || fd.Message() != nil {
				return true
			}
			s := valuegen.Canonical(fd, v)
			back, err := valuegen.Parse(fd, s)
			if err != nil {
				t.Fatalf("%s: %v", fd.Name(), err)
			}
			if !back.Equal(v) {
				t.Fatalf("%s: %v -> %q -> %v", fd.Name(), v, s, back)
			}
			return true
		})
	}
}
