package load

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoregistry"
)

func demo(t *testing.T) *protoregistry.Files {
	set, names, err := FromProtoDir("../../demo/proto", nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(names) != 1 || names[0] != "orders/v1/orders.proto" {
		t.Fatalf("names %v", names)
	}
	f, err := protodesc.NewFiles(set)
	if err != nil {
		t.Fatal(err)
	}
	return f
}

func hints(t *testing.T, y string) *Hints {
	p := filepath.Join(t.TempDir(), "h.yaml")
	os.WriteFile(p, []byte(y), 0644)
	h, err := ReadHints(p)
	if err != nil {
		t.Fatalf("ReadHints: %v", err)
	}
	return h
}

func TestBuildDemo(t *testing.T) {
	h, err := ReadHints("../../demo/hydo/hints.yaml")
	if err != nil {
		t.Fatal(err)
	}
	p, err := Build(demo(t), h, nil)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, r := range p.RPCs {
		names = append(names, r.Short+"="+r.Name)
	}
	if got := strings.Join(names, " "); got != "create=orders.v1.Orders/Create get=orders.v1.Orders/Get cancel=orders.v1.Orders/Cancel list=orders.v1.Orders/List" {
		t.Fatalf("rpcs: %s", got)
	}
	g, _ := p.Find("Get")
	if f := g.Fields["id"].From; f == nil || f.RPC != "orders.v1.Orders/Create" || f.Side != "response" || f.Path != "id" {
		t.Fatalf("Get.id from: %+v", f)
	}
	if len(g.Needs) != 1 {
		t.Fatalf("Get needs %v", g.Needs)
	}
	c, _ := p.Find("orders.v1.Orders/Create")
	if c.Retry.Attempts != 4 || !c.Retry.AtEnd || c.OutcomeID != "request_id" || c.Weight != 3 {
		t.Fatalf("Create: %+v", c)
	}
	// the plan is a pure function of its inputs
	a, _ := p.Marshal()
	p2, _ := Build(demo(t), h, nil)
	b, _ := p2.Marshal()
	if string(a) != string(b) {
		t.Fatal("two builds differ")
	}
}

func TestHintErrors(t *testing.T) {
	f := demo(t)
	for _, c := range []struct{ y, want string }{
		{"rpcs: {Nope: {}}", `no rpc "Nope"`},
		{"rpcs: {Create: {fields: {nope: {value: 1}}}}", `no field "nope"`},
		{"rpcs: {Create: {fields: {customer: {range: [1, 2]}}}}", "range needs a number field"},
		{"rpcs: {Create: {fields: {amount_cents: {range: [5, 1]}}}}", "lo > hi"},
		{"rpcs: {Create: {fields: {channel: {choice: [PURPLE]}}}}", "not a value of enum"},
		{"rpcs: {Create: {fields: {items: {value: x}}}}", "needs a scalar"},
		{"rpcs: {Create: {fields: {customer: {format: base64}}}}", "format"},
		{"rpcs: {Get: {fields: {id: {from: Create.amount_cents}}}}", ""}, // int64 -> string is allowed
		{"rpcs: {Get: {fields: {id: {from: Create.items}}}}", "is repeated message"},
		{"rpcs: {Get: {fields: {id: {from: Create.nope}}}}", `no field "nope"`},
		{"rpcs: {Create: {fields: {labels.key: {len: [2, 2]}, labels.bogus: {}}}}", "comes key or value"},
		{"rpcs: {Create: {oneofs: {payment: [cash]}}}", "not a member"},
		{"rpcs: {Create: {oneofs: {pay: [card]}}}", `no oneof "pay"`},
		{"rpcs: {Create: {classify: {TIMEOUT: fail}}}", "not a gRPC code name"},
		{"rpcs: {Create: {classify: {UNAVAILABLE: maybe}}}", "ok, fail, info"},
		{"rpcs: {Create: {outcome_id: items.sku}}", "single scalar"},
		{"services: [Payments]", "not in the schema"},
		{"scenarios: [{name: s, calls: [Create, Nope]}]", `no rpc "Nope"`},
	} {
		_, err := Build(f, hints(t, c.y), nil)
		switch {
		case c.want == "" && err != nil:
			t.Errorf("%s: unexpected error %v", c.y, err)
		case c.want != "" && (err == nil || !strings.Contains(err.Error(), c.want)):
			t.Errorf("%s: want error containing %q, got %v", c.y, c.want, err)
		}
	}
	// unknown keys are errors, not silently ignored
	p := filepath.Join(t.TempDir(), "h.yaml")
	os.WriteFile(p, []byte("rpcs: {Create: {wieght: 3}}\n"), 0644)
	if _, err := ReadHints(p); err == nil || !strings.Contains(err.Error(), "wieght") {
		t.Fatalf("typo accepted: %v", err)
	}
}

func TestParseFrom(t *testing.T) {
	for in, want := range map[string]string{
		"Create.id":                           "Create response id",
		"Create.request.request_id":           "Create request request_id",
		"orders.v1.Orders/Create.response.id": "orders.v1.Orders/Create response id",
		"orders.v1.Orders/List.orders.id":     "orders.v1.Orders/List response orders.id",
	} {
		f, err := parseFrom(in)
		if err != nil {
			t.Fatalf("%s: %v", in, err)
		}
		if got := f.RPC + " " + f.Side + " " + f.Path; got != want {
			t.Errorf("%s: %s, want %s", in, got, want)
		}
	}
	if _, err := parseFrom("Create"); err == nil {
		t.Fatal("no path accepted")
	}
	f, err := parseFrom(map[string]interface{}{"rpc": "Create", "path": "id", "pick": "latest", "fallback": "generate"})
	if err != nil || f.Pick != "latest" || f.Fallback != "generate" {
		t.Fatalf("map form: %+v %v", f, err)
	}
}

func TestSnake(t *testing.T) {
	for in, want := range map[string]string{"Create": "create", "GetOrder": "get_order", "ListHTTPRoutes": "list_http_routes", "V2Get": "v2_get"} {
		if got := snake(in); got != want {
			t.Errorf("snake(%s) = %s, want %s", in, got, want)
		}
	}
}

// The descriptor set is canonical: the same schema serializes to the same bytes.
func TestCanonicalSet(t *testing.T) {
	a, _, err := FromProtoDir("../../testdata/kitchen", nil)
	if err != nil {
		t.Fatal(err)
	}
	b, _, _ := FromProtoDir("../../testdata/kitchen", nil)
	x, _ := Marshal(a)
	y, _ := Marshal(b)
	if string(x) != string(y) {
		t.Fatal("two compilations differ")
	}
	var order []string
	for _, f := range a.File {
		order = append(order, f.GetName())
	}
	// dependencies first
	pos := map[string]int{}
	for i, n := range order {
		pos[n] = i
	}
	if pos["legacy.proto"] > pos["kitchen.proto"] || pos["google/protobuf/any.proto"] > pos["kitchen.proto"] {
		t.Fatalf("not dependencies-first: %v", order)
	}
}
