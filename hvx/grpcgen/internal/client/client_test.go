package client

import (
	"context"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/reflection"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"

	pb "hydogrpc/demo/ordersv1"
	"hydogrpc/internal/load"
	"hydogrpc/internal/rng"
)

// fake is an in-memory orders server. slow[rid] = how many Create attempts of that request
// sleep past the client's deadline before one is served.
type fake struct {
	mu     sync.Mutex
	byReq  map[string]*pb.Order
	byID   map[string]*pb.Order
	slow   map[string]int
	down   bool
	n      int
	seenRq []string
}

func (f *fake) create(ctx context.Context, in *pb.CreateRequest) (*pb.Order, error) {
	f.mu.Lock()
	f.seenRq = append(f.seenRq, in.RequestId)
	if f.down {
		f.mu.Unlock()
		return nil, status.Error(codes.Unavailable, "down")
	}
	if f.slow[in.RequestId] > 0 {
		f.slow[in.RequestId]--
		f.mu.Unlock()
		<-ctx.Done()
		return nil, ctx.Err()
	}
	defer f.mu.Unlock()
	if o := f.byReq[in.RequestId]; o != nil {
		return o, nil
	}
	f.n++
	o := &pb.Order{Id: "o" + strconv.Itoa(f.n), RequestId: in.RequestId, Customer: in.Customer, AmountCents: in.AmountCents, Status: pb.Status_OPEN}
	f.byReq[in.RequestId], f.byID[o.Id] = o, o
	return o, nil
}

func (f *fake) get(ctx context.Context, in *pb.GetRequest) (*pb.Order, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if o := f.byID[in.Id]; o != nil {
		return o, nil
	}
	return nil, status.Error(codes.NotFound, "no order "+in.Id)
}

func (f *fake) cancel(ctx context.Context, in *pb.CancelRequest) (*pb.Order, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	o := f.byID[in.Id]
	if o == nil {
		return nil, status.Error(codes.NotFound, "no order "+in.Id)
	}
	o.Status = pb.Status_CANCELLED
	return o, nil
}

func (f *fake) list(ctx context.Context, in *pb.ListRequest) (*pb.ListResponse, error) {
	return &pb.ListResponse{}, nil
}

func method[Req any, PReq interface {
	*Req
	proto.Message
}, Resp proto.Message](name string, h func(context.Context, PReq) (Resp, error)) grpc.MethodDesc {
	return grpc.MethodDesc{MethodName: name, Handler: func(_ interface{}, ctx context.Context, dec func(interface{}) error, _ grpc.UnaryServerInterceptor) (interface{}, error) {
		in := PReq(new(Req))
		if err := dec(in); err != nil {
			return nil, err
		}
		return h(ctx, in)
	}}
}

func serve(t *testing.T, f *fake) string {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	gs := grpc.NewServer()
	gs.RegisterService(&grpc.ServiceDesc{ServiceName: "orders.v1.Orders", HandlerType: (*interface{})(nil),
		Methods:  []grpc.MethodDesc{method("Create", f.create), method("Get", f.get), method("Cancel", f.cancel), method("List", f.list)},
		Metadata: "orders/v1/orders.proto"}, struct{}{})
	reflection.Register(gs)
	go gs.Serve(lis)
	t.Cleanup(gs.Stop)
	return lis.Addr().String()
}

func newFake() *fake {
	return &fake{byReq: map[string]*pb.Order{}, byID: map[string]*pb.Order{}, slow: map[string]int{}}
}

// setup writes desc.pb + plan.json for the demo schema and hints, pointed at addr.
func setup(t *testing.T, addr, extra string) (dir string) {
	dir = t.TempDir()
	set, _, err := load.FromProtoDir("../../demo/proto", nil)
	if err != nil {
		t.Fatal(err)
	}
	files, _ := protodesc.NewFiles(set)
	hb, _ := os.ReadFile("../../demo/hints.yaml")
	hp := filepath.Join(dir, "hints.yaml")
	os.WriteFile(hp, append(hb, []byte(extra)...), 0644)
	h, err := load.ReadHints(hp)
	if err != nil {
		t.Fatal(err)
	}
	h.Target, h.DeadlineMs = addr, 300
	for k, r := range h.RPCs {
		r.DeadlineMs = 300
		if r.Retry != nil {
			r.Retry.BackoffMs = 10
		}
		h.RPCs[k] = r
	}
	p, err := load.Build(files, h, nil)
	if err != nil {
		t.Fatal(err)
	}
	b, _ := load.Marshal(set)
	os.WriteFile(filepath.Join(dir, "desc.pb"), b, 0644)
	pj, _ := p.Marshal()
	os.WriteFile(filepath.Join(dir, "plan.json"), pj, 0644)
	return dir
}

func newClient(t *testing.T, dir string, seed uint64) (*Client, string) {
	st := t.TempDir()
	t.Setenv("HYDO_GRPC_CONSOLE", "off")
	t.Setenv("HYDO_OUTCOMES", filepath.Join(st, "outcomes"))
	t.Setenv("HYDO_ACKED", filepath.Join(st, "acked"))
	c, err := New(Options{Desc: filepath.Join(dir, "desc.pb"), Plan: filepath.Join(dir, "plan.json"), StateDir: st, Seed: &seed})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(c.Close)
	return c, st
}

var timeRe = regexp.MustCompile(`^(HYDO-CALL \S+ \S+ \S+) \S+ `)

func calls(t *testing.T, st string) []string {
	b, _ := os.ReadFile(filepath.Join(st, "calls"))
	var out []string
	for _, l := range strings.Split(strings.TrimSpace(string(b)), "\n") {
		out = append(out, timeRe.ReplaceAllString(l, "$1 T "))
	}
	return out
}

// Two runs with the same seed against equal servers send the same requests in the same
// order and record the same outcomes; another seed sends other requests.
func TestStepsDeterministic(t *testing.T) {
	var runs [][]string
	for _, seed := range []uint64{11, 11, 12} {
		addr := serve(t, newFake())
		c, st := newClient(t, setup(t, addr, ""), seed)
		c.Steps(0, 12)
		runs = append(runs, calls(t, st))
	}
	if strings.Join(runs[0], "\n") != strings.Join(runs[1], "\n") {
		t.Fatalf("same seed, different calls:\n%s\n---\n%s", strings.Join(runs[0], "\n"), strings.Join(runs[1], "\n"))
	}
	if strings.Join(runs[0], "\n") == strings.Join(runs[2], "\n") {
		t.Fatal("different seeds, same calls")
	}
	all := strings.Join(runs[0], "\n")
	for _, want := range []string{"invoke T orders.v1.Orders/Create", "ok T orders.v1.Orders/Create OK", "invoke T orders.v1.Orders/Cancel"} {
		if !strings.Contains(all, want) {
			t.Errorf("no %q in\n%s", want, all)
		}
	}
	// Cancel and Get only ever use ids an earlier Create returned
	for _, l := range runs[0] {
		if strings.Contains(l, "invoke T orders.v1.Orders/Cancel") || strings.Contains(l, "invoke T orders.v1.Orders/Get") {
			if !regexp.MustCompile(`%22id%22:%22o[0-9]+%22`).MatchString(strings.ReplaceAll(l, `"`, "%22")) {
				t.Errorf("not an id from Create: %s", l)
			}
		}
	}
}

// An unknown outcome is retried with the same request under the same op; definite
// failures are not retried; the outcome files get one line per attempt.
func TestRetryAndOutcomes(t *testing.T) {
	f := newFake()
	f.slow["r1"] = 2 // two attempts time out, the third succeeds
	addr := serve(t, f)
	c, st := newClient(t, setup(t, addr, ""), 1)
	r, _ := c.Plan.Find("Create")
	reqs, err := c.Generate(r, rng.New(1), &env{c: c, local: map[string][]string{}})
	if err != nil {
		t.Fatal(err)
	}
	res := c.Op(0, 1, r, reqs, &env{c: c, local: map[string][]string{}})
	if res.Kind != "ok" {
		t.Fatalf("Create ended %s", res.Kind)
	}
	cl := calls(t, st)
	if len(cl) != 6 || !strings.HasPrefix(cl[1], "HYDO-CALL 0 1 info T orders.v1.Orders/Create DEADLINE_EXCEEDED") ||
		!strings.HasPrefix(cl[5], "HYDO-CALL 0 1 ok T") || cl[0] != cl[2] || cl[2] != cl[4] {
		t.Fatalf("attempts:\n%s", strings.Join(cl, "\n"))
	}
	out, _ := os.ReadFile(filepath.Join(st, "outcomes"))
	if string(out) != "r1 unknown\nr1 unknown\nr1 ack\n" {
		t.Fatalf("outcomes %q", out)
	}
	acked, _ := os.ReadFile(filepath.Join(st, "acked"))
	if string(acked) != "r1\n" {
		t.Fatalf("acked %q", acked)
	}
	// a definite failure: one attempt only
	g, _ := c.Plan.Find("Cancel")
	reqs, _ = c.ParseJSON(g, `{"id":"o99"}`)
	if res := c.Op(0, 2, g, reqs, nil); res.Kind != "fail" {
		t.Fatalf("Cancel of a missing order: %s", res.Kind)
	}
	if n := len(calls(t, st)); n != 8 {
		t.Fatalf("definite failure retried (%d records)", n)
	}
}

// `hydo-grpc retry` re-sends operations whose last outcome was unknown, with the same
// request, after the faults heal.
func TestRetryAtEnd(t *testing.T) {
	f := newFake()
	f.down = true
	addr := serve(t, f)
	c, st := newClient(t, setup(t, addr, ""), 3)
	c.Steps(0, 4)
	before := len(f.seenRq)
	if before == 0 {
		t.Fatal("no Create was attempted")
	}
	f.mu.Lock()
	f.down = false
	f.mu.Unlock()
	if err := c.Retry(filepath.Join(st, "calls")); err != nil {
		t.Fatal(err)
	}
	if len(f.byReq) == 0 {
		t.Fatal("retry created nothing")
	}
	// every retried request_id was one the steps had sent
	sent := map[string]bool{}
	for _, r := range f.seenRq[:before] {
		sent[r] = true
	}
	for r := range f.byReq {
		if !sent[r] {
			t.Fatalf("retry sent a new request_id %s", r)
		}
	}
	out, _ := os.ReadFile(filepath.Join(st, "outcomes"))
	if !strings.Contains(string(out), " ack\n") {
		t.Fatalf("no ack after the retry: %s", out)
	}
	// nothing left unknown -> a second retry sends nothing
	n := len(f.seenRq)
	c.Retry(filepath.Join(st, "calls"))
	if len(f.seenRq) != n {
		t.Fatal("second retry re-sent known outcomes")
	}
}

// Reflection gives the same schema as the .proto files.
func TestReflection(t *testing.T) {
	addr := serve(t, newFake())
	got, err := load.FromReflection(addr, 5*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	want, _, _ := load.FromProtoDir("../../demo/proto", nil)
	var gn, wn []string
	for _, f := range got.File {
		gn = append(gn, f.GetName())
	}
	for _, f := range want.File {
		wn = append(wn, f.GetName())
	}
	if strings.Join(gn, ",") != strings.Join(wn, ",") {
		t.Fatalf("reflection files %v, proto files %v", gn, wn)
	}
	files, err := protodesc.NewFiles(got)
	if err != nil {
		t.Fatal(err)
	}
	p, err := load.Build(files, &load.Hints{}, nil)
	if err != nil || len(p.RPCs) != 4 {
		t.Fatalf("plan from reflection: %v", err)
	}
}

func TestExtract(t *testing.T) {
	m := &pb.ListResponse{Orders: []*pb.Order{{Id: "o1", Items: []*pb.Item{{Sku: "a"}, {Sku: "b"}}}, {Id: "o2"}}}
	if got := strings.Join(Extract(m.ProtoReflect(), "orders.id"), ","); got != "o1,o2" {
		t.Fatal(got)
	}
	if got := strings.Join(Extract(m.ProtoReflect(), "orders.items.sku"), ","); got != "a,b" {
		t.Fatal(got)
	}
	cr := &pb.CreateRequest{Labels: map[string]string{"z": "1", "a": "2"}}
	if got := strings.Join(Extract(cr.ProtoReflect(), "labels.key"), ","); got != "a,z" {
		t.Fatal(got)
	}
	if got := Extract(cr.ProtoReflect(), "note"); len(got) != 0 {
		t.Fatalf("unset optional extracted: %v", got)
	}
}
