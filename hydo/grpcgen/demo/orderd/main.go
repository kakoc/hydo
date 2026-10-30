// orderd is the demo order service of hydo/grpcgen: Create / Get / Cancel / List over gRPC,
// with the orders in Redis (a pool of RESP connections; every store command has its own 60 s
// timeout and does NOT follow the gRPC request's deadline, as in many real services).
//
// It carries ONE planted bug, reachable only through an overlap of two requests: with
// --mode buggy, Create looks its idempotency key (request_id) up in Redis and, on a miss,
// writes the new order with separate commands -- check, then act. Two Creates with the same
// request_id that run at the same time both see the miss and create two orders. A client gets
// there by retrying a Create whose outcome was unknown (DEADLINE_EXCEEDED) while the first
// attempt is still waiting on the store, for example while Redis is paused: when it resumes,
// both lookups miss. --mode fixed does the lookup and the writes in one Lua script, which
// Redis runs atomically.
//
//	orderd serve [--addr 127.0.0.1:7070] [--redis 127.0.0.1:6379] [--mode buggy|fixed]
//	orderd check [--addr ...]   the invariants: exit 0 holds, 1 violated, 2 cannot evaluate
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/reflection"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/timestamppb"

	pb "hydogrpc/demo/ordersv1"
)

// ---- a minimal Redis client: RESP over a pool of connections ----

const storeTimeout = 60 * time.Second

type rconn struct {
	c net.Conn
	r *bufio.Reader
}

type redis struct {
	addr string
	mu   sync.Mutex
	idle []*rconn
}

var errNil = errors.New("redis: nil")

func (rd *redis) get() (*rconn, error) {
	rd.mu.Lock()
	if n := len(rd.idle); n > 0 {
		c := rd.idle[n-1]
		rd.idle = rd.idle[:n-1]
		rd.mu.Unlock()
		return c, nil
	}
	rd.mu.Unlock()
	c, err := net.DialTimeout("tcp", rd.addr, 5*time.Second)
	if err != nil {
		return nil, err
	}
	return &rconn{c: c, r: bufio.NewReader(c)}, nil
}

func (rd *redis) put(c *rconn) {
	rd.mu.Lock()
	rd.idle = append(rd.idle, c)
	rd.mu.Unlock()
}

// do runs one command. The reply is a string, an int64, nil (errNil) or a []interface{}.
func (rd *redis) do(args ...string) (interface{}, error) {
	c, err := rd.get()
	if err != nil {
		return nil, err
	}
	var b strings.Builder
	fmt.Fprintf(&b, "*%d\r\n", len(args))
	for _, a := range args {
		fmt.Fprintf(&b, "$%d\r\n%s\r\n", len(a), a)
	}
	c.c.SetDeadline(time.Now().Add(storeTimeout))
	if _, err := c.c.Write([]byte(b.String())); err != nil {
		c.c.Close()
		return nil, err
	}
	v, err := readReply(c.r)
	if err != nil && err != errNil && !strings.HasPrefix(err.Error(), "redis: ERR") {
		c.c.Close() // a broken connection is not reused
		return nil, err
	}
	rd.put(c)
	return v, err
}

func readReply(r *bufio.Reader) (interface{}, error) {
	line, err := r.ReadString('\n')
	if err != nil {
		return nil, err
	}
	line = strings.TrimRight(line, "\r\n")
	if line == "" {
		return nil, errors.New("redis: empty reply")
	}
	switch line[0] {
	case '+':
		return line[1:], nil
	case '-':
		return nil, errors.New("redis: " + line[1:])
	case ':':
		return strconv.ParseInt(line[1:], 10, 64)
	case '$':
		n, _ := strconv.Atoi(line[1:])
		if n < 0 {
			return nil, errNil
		}
		buf := make([]byte, n+2)
		if _, err := ioReadFull(r, buf); err != nil {
			return nil, err
		}
		return string(buf[:n]), nil
	case '*':
		n, _ := strconv.Atoi(line[1:])
		if n < 0 {
			return nil, errNil
		}
		out := make([]interface{}, n)
		for i := range out {
			v, err := readReply(r)
			if err != nil && err != errNil {
				return nil, err
			}
			out[i] = v
		}
		return out, nil
	}
	return nil, fmt.Errorf("redis: bad reply %q", line)
}

func ioReadFull(r *bufio.Reader, buf []byte) (int, error) {
	n := 0
	for n < len(buf) {
		k, err := r.Read(buf[n:])
		n += k
		if err != nil {
			return n, err
		}
	}
	return n, nil
}

// ---- the service ----

type orderJSON struct {
	ID        string     `json:"id,omitempty"`
	RequestID string     `json:"rid"`
	Customer  string     `json:"cust"`
	Amount    int64      `json:"amt"`
	Status    string     `json:"st"`
	Items     []itemJSON `json:"items,omitempty"`
	Created   int64      `json:"ts"`
}

type itemJSON struct {
	Sku string `json:"sku"`
	Qty int32  `json:"qty"`
}

func (o *orderJSON) proto() *pb.Order {
	p := &pb.Order{Id: o.ID, RequestId: o.RequestID, Customer: o.Customer, AmountCents: o.Amount,
		Status: pb.Status(pb.Status_value[o.Status]), Created: timestamppb.New(time.Unix(o.Created, 0))}
	for _, it := range o.Items {
		p.Items = append(p.Items, &pb.Item{Sku: it.Sku, Qty: it.Qty})
	}
	return p
}

type server struct {
	rd    *redis
	fixed bool
}

func storeErr(err error) error {
	// the request may or may not have taken effect: never a definite failure
	return status.Error(codes.Unavailable, "store: "+err.Error())
}

func (s *server) load(id string) (*orderJSON, error) {
	v, err := s.rd.do("GET", "order:"+id)
	if err == errNil {
		return nil, status.Error(codes.NotFound, "no order "+id)
	}
	if err != nil {
		return nil, storeErr(err)
	}
	var o orderJSON
	if err := json.Unmarshal([]byte(v.(string)), &o); err != nil {
		return nil, status.Error(codes.DataLoss, "order "+id+": "+err.Error())
	}
	return &o, nil
}

// createScript is the fixed Create: lookup and writes in one atomic step.
const createScript = `
local id = redis.call('GET', KEYS[1])
if id then return id end
id = 'o' .. redis.call('INCR', 'next')
redis.call('SET', 'order:' .. id, '{"id":"' .. id .. '",' .. string.sub(ARGV[1], 2))
redis.call('SET', KEYS[1], id)
redis.call('RPUSH', 'orders', id)
return id`

func (s *server) Create(ctx context.Context, in *pb.CreateRequest) (*pb.Order, error) {
	if in.RequestId == "" || in.Customer == "" || in.AmountCents <= 0 {
		return nil, status.Error(codes.InvalidArgument, "request_id, customer and a positive amount_cents are required")
	}
	o := orderJSON{RequestID: in.RequestId, Customer: in.Customer, Amount: in.AmountCents, Status: "OPEN",
		Created: time.Now().Unix()}
	for _, it := range in.Items {
		o.Items = append(o.Items, itemJSON{it.Sku, it.Qty})
	}
	if s.fixed {
		body, _ := json.Marshal(o)
		v, err := s.rd.do("EVAL", createScript, "1", "req:"+in.RequestId, string(body))
		if err != nil {
			return nil, storeErr(err)
		}
		got, err := s.load(v.(string))
		if err != nil {
			return nil, err
		}
		return got.proto(), nil
	}
	// BUG (mode buggy): check, then act, in separate commands. Two Creates with the same
	// request_id that both pass the check before either writes create two orders.
	if v, err := s.rd.do("GET", "req:"+in.RequestId); err == nil {
		got, err := s.load(v.(string))
		if err != nil {
			return nil, err
		}
		return got.proto(), nil // a retry of a Create that was applied
	} else if err != errNil {
		return nil, storeErr(err)
	}
	n, err := s.rd.do("INCR", "next")
	if err != nil {
		return nil, storeErr(err)
	}
	o.ID = "o" + strconv.FormatInt(n.(int64), 10)
	body, _ := json.Marshal(o)
	for _, cmd := range [][]string{{"SET", "order:" + o.ID, string(body)}, {"SET", "req:" + in.RequestId, o.ID}, {"RPUSH", "orders", o.ID}} {
		if _, err := s.rd.do(cmd...); err != nil {
			return nil, storeErr(err)
		}
	}
	return o.proto(), nil
}

func (s *server) Get(ctx context.Context, in *pb.GetRequest) (*pb.Order, error) {
	o, err := s.load(in.Id)
	if err != nil {
		return nil, err
	}
	return o.proto(), nil
}

func (s *server) Cancel(ctx context.Context, in *pb.CancelRequest) (*pb.Order, error) {
	o, err := s.load(in.Id)
	if err != nil {
		return nil, err
	}
	if o.Status != "CANCELLED" {
		o.Status = "CANCELLED"
		body, _ := json.Marshal(o)
		if _, err := s.rd.do("SET", "order:"+o.ID, string(body)); err != nil {
			return nil, storeErr(err)
		}
	}
	return o.proto(), nil
}

func (s *server) List(ctx context.Context, in *pb.ListRequest) (*pb.ListResponse, error) {
	size := int(in.PageSize)
	if size <= 0 || size > 1000 {
		size = 1000
	}
	off := 0
	if in.PageToken != "" {
		n, err := strconv.Atoi(in.PageToken)
		if err != nil || n < 0 {
			return nil, status.Error(codes.InvalidArgument, "bad page_token")
		}
		off = n
	}
	v, err := s.rd.do("LRANGE", "orders", "0", "-1")
	if err != nil {
		return nil, storeErr(err)
	}
	var all []*pb.Order
	for _, x := range v.([]interface{}) {
		id, _ := x.(string)
		o, err := s.load(id)
		if status.Code(err) == codes.NotFound {
			continue
		}
		if err != nil {
			return nil, err
		}
		if in.Customer == "" || o.Customer == in.Customer {
			all = append(all, o.proto())
		}
	}
	out := &pb.ListResponse{}
	for i := off; i < len(all) && len(out.Orders) < size; i++ {
		out.Orders = append(out.Orders, all[i])
	}
	if off+len(out.Orders) < len(all) {
		out.NextPageToken = strconv.Itoa(off + len(out.Orders))
	}
	return out, nil
}

// The service descriptor, written by hand (no protoc-gen-go-grpc), like hydo/stack/main.go.
type ordersServer interface {
	Create(context.Context, *pb.CreateRequest) (*pb.Order, error)
	Get(context.Context, *pb.GetRequest) (*pb.Order, error)
	Cancel(context.Context, *pb.CancelRequest) (*pb.Order, error)
	List(context.Context, *pb.ListRequest) (*pb.ListResponse, error)
}

func unary[Req any, PReq interface {
	*Req
	proto.Message
}, Resp proto.Message](name string, f func(ordersServer, context.Context, PReq) (Resp, error)) grpc.MethodDesc {
	return grpc.MethodDesc{MethodName: name, Handler: func(srv interface{}, ctx context.Context, dec func(interface{}) error, _ grpc.UnaryServerInterceptor) (interface{}, error) {
		in := PReq(new(Req))
		if err := dec(in); err != nil {
			return nil, err
		}
		return f(srv.(ordersServer), ctx, in)
	}}
}

var serviceDesc = grpc.ServiceDesc{
	ServiceName: "orders.v1.Orders",
	HandlerType: (*ordersServer)(nil),
	Methods: []grpc.MethodDesc{
		unary("Create", ordersServer.Create),
		unary("Get", ordersServer.Get),
		unary("Cancel", ordersServer.Cancel),
		unary("List", ordersServer.List),
	},
	Metadata: "orders/v1/orders.proto",
}

func serve(addr, raddr, mode string) {
	if mode != "buggy" && mode != "fixed" {
		fail("--mode must be buggy or fixed")
	}
	lis, err := net.Listen("tcp", addr)
	if err != nil {
		fail("listen: %v", err)
	}
	gs := grpc.NewServer()
	gs.RegisterService(&serviceDesc, &server{rd: &redis{addr: raddr}, fixed: mode == "fixed"})
	reflection.Register(gs)
	fmt.Printf("orderd: serving on %s (mode %s, store %s)\n", addr, mode, raddr)
	if err := gs.Serve(lis); err != nil {
		fail("serve: %v", err)
	}
}

// ---- the invariants (orderd check) ----

// check reads every order through List and checks, against $HYDO_ACKED (the request_ids of
// acknowledged Creates, "r..."; the ids of acknowledged Cancels, "o..."):
//   - no two orders share a request_id (Create is idempotent);
//   - every acknowledged Create's order exists;
//   - every acknowledged Cancel's order is CANCELLED.
func check(addr string) int {
	cc, err := grpc.NewClient("passthrough:///"+addr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		fmt.Println("cannot connect:", err)
		return 2
	}
	defer cc.Close()
	var all []*pb.Order
	tok := ""
	for {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		out := new(pb.ListResponse)
		err := cc.Invoke(ctx, "/orders.v1.Orders/List", &pb.ListRequest{PageSize: 1000, PageToken: tok}, out)
		cancel()
		if err != nil {
			fmt.Println("List failed:", err)
			return 2
		}
		all = append(all, out.Orders...)
		if tok = out.NextPageToken; tok == "" {
			break
		}
	}
	byReq := map[string][]string{}
	byID := map[string]*pb.Order{}
	for _, o := range all {
		byReq[o.RequestId] = append(byReq[o.RequestId], o.Id)
		byID[o.Id] = o
	}
	var rids []string
	for r := range byReq {
		rids = append(rids, r)
	}
	sort.Strings(rids)
	for _, r := range rids {
		if ids := byReq[r]; len(ids) > 1 {
			fmt.Printf("VIOLATION request_id %s created %d orders: %s\n", r, len(ids), strings.Join(ids, " "))
			return 1
		}
	}
	acked := 0
	if p := os.Getenv("HYDO_ACKED"); p != "" {
		if f, err := os.Open(p); err == nil {
			sc := bufio.NewScanner(f)
			for sc.Scan() {
				id := strings.TrimSpace(sc.Text())
				switch {
				case strings.HasPrefix(id, "r"):
					acked++
					if len(byReq[id]) == 0 {
						fmt.Printf("VIOLATION acknowledged Create %s has no order\n", id)
						return 1
					}
				case strings.HasPrefix(id, "o"):
					acked++
					if o := byID[id]; o == nil || o.Status != pb.Status_CANCELLED {
						fmt.Printf("VIOLATION acknowledged Cancel of %s did not stick\n", id)
						return 1
					}
				}
			}
			f.Close()
		}
	}
	fmt.Printf("orders=%d acked=%d: request ids unique, acknowledged operations present\n", len(all), acked)
	return 0
}

func fail(f string, a ...interface{}) {
	fmt.Fprintf(os.Stderr, "orderd: "+f+"\n", a...)
	os.Exit(1)
}

func main() {
	if len(os.Args) < 2 {
		fail("usage: orderd serve|check [flags]")
	}
	fs := flag.NewFlagSet(os.Args[1], flag.ExitOnError)
	addr := fs.String("addr", "127.0.0.1:7070", "listen / connect address")
	raddr := fs.String("redis", "127.0.0.1:6379", "Redis address")
	mode := fs.String("mode", "buggy", "buggy | fixed")
	fs.Parse(os.Args[2:])
	switch os.Args[1] {
	case "serve":
		serve(*addr, *raddr, *mode)
	case "check":
		os.Exit(check(*addr))
	default:
		fail("unknown command %q", os.Args[1])
	}
}
