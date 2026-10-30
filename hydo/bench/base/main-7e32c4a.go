// hydostack -- the gRPC front door of the four-service stack (Milestones 35-37), plus its
// client and its cross-service oracle, in one static binary.
//
//	hydostack server            gRPC OrderService on 127.0.0.1:50051. Place(id) is SAFE TO
//	                            RETRY with the same id (Milestone 37):
//	                              1. MySQL: the order row is upserted (a retry is a no-op);
//	                              2. Kafka: published exactly once -- an idempotent producer
//	                                 (franz-go: producer id + sequence numbers, so the broker
//	                                 drops its own internal retries), the row's `published`
//	                                 flag records success, and if an earlier attempt for this
//	                                 id ended in an error (its outcome unknown) the topic is
//	                                 checked for the key before producing again;
//	                              3. Redis: SET (idempotent).
//	                            It ACKs only after all three are confirmed.
//	hydostack place <n>         client: places orders 0..n-1 through gRPC, one at a time.
//	hydostack place <from> <to> client: one attempt per order; a failed order is reported
//	                            and the client moves on. ACKed ids go to $HYDO_ACKED.
//	hydostack retry <n> <file>  client: re-places, with the SAME id and several attempts,
//	                            every order in 0..n-1 not yet listed in <file> (the ACKed
//	                            ids); newly ACKed ids are appended to <file>.
//	hydostack oracle <n>        after a crash + recovery: every order 0..n-1 is in MySQL and
//	                            exactly once in Kafka, and Redis never holds a wrong value.
//	hydostack oracle <n> <file> the same for the ACKed ids in <file>, which must also be
//	                            marked published; every order NOT acked must not have
//	                            reached Kafka (no "applied but reported as failed").
//
// Invariants are described with hydo's assertion SDK (package hydoassert, hydo/ASSERTIONS.md):
// the catalog below is declared up front and written to the console by `server` and
// `oracle`; every evaluation is a HYDO-ASSERT line. The HYDO-ORACLE verdict line is derived
// from the same checks (the first failing one), so existing harnesses keep working.
//
// Deterministic by construction: fixed ids, the amount is a pure function of the id, no
// clocks or randomness in the data. gRPC uses the standard protobuf codec with the
// well-known wrapper types, so no generated code is needed.
package main

import (
	"bufio"
	"context"
	"database/sql"
	"fmt"
	"net"
	"os"
	"strconv"
	"time"

	_ "github.com/go-sql-driver/mysql"
	"github.com/segmentio/kafka-go"
	"github.com/twmb/franz-go/pkg/kgo"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/protobuf/types/known/wrapperspb"

	"hydostack/hydoassert"
)

// ---- the property catalog (declaration order = catalog order) ----
var (
	// after a crash + recovery (the oracle)
	pStoresReadable = hydoassert.Always("oracle.stores_readable", "after recovery the oracle can read MySQL, Kafka and Redis")
	pInMySQL        = hydoassert.Always("order.in_mysql", "every checked order is in MySQL with its amount")
	pInKafka        = hydoassert.Always("order.in_kafka", "every checked order is in Kafka with its amount")
	pKafkaOnce      = hydoassert.Always("order.kafka_exactly_once", "every checked order is in Kafka exactly once")
	pPublished      = hydoassert.AlwaysOrUnreachable("order.acked_marked_published", "an ACKed order is marked published in MySQL")
	pRedisNotWrong  = hydoassert.Always("order.redis_never_wrong", "Redis never holds a wrong amount (a missing key is allowed)")
	pAppliedNoAck   = hydoassert.Unreachable("order.applied_but_not_acked", "an order the client was never ACKed for reached Kafka")
	pRedisMiss      = hydoassert.Sometimes("oracle.redis_cache_miss", "the oracle saw a Redis cache miss")
	pPending        = hydoassert.Sometimes("oracle.pending_order", "an un-ACKed order was left pending (unpublished) in MySQL")
	// the front door
	pPlaceAcked   = hydoassert.Sometimes("server.place_acked", "a Place call was ACKed")
	pStoreError   = hydoassert.Sometimes("server.store_error", "a Place call failed on a store error")
	pKafkaRecheck = hydoassert.Sometimes("server.kafka_recheck", "an order whose earlier produce was uncertain was looked up in Kafka first")
	pDedupHit     = hydoassert.Sometimes("server.kafka_dedup_hit", "the Kafka lookup found the order already there (a duplicate was avoided)")
	pRedisRedial  = hydoassert.Reachable("server.redis_redialed", "the front door re-dialed Redis after its connection broke")
	// the client
	pClientFail     = hydoassert.Sometimes("client.place_failed", "a client Place attempt failed")
	pRetryRecovered = hydoassert.Sometimes("client.retry_recovered", "an order that had failed was ACKed on a later attempt")
	pRetryExhausted = hydoassert.Sometimes("client.retry_exhausted", "an order was still un-ACKed after all retry attempts")
)

const (
	grpcAddr  = "127.0.0.1:50051"
	kafkaAddr = "127.0.0.1:9092"
	redisAddr = "127.0.0.1:6379"
	mysqlDSN  = "hydo:@tcp(127.0.0.1:3306)/"
	topic     = "orders"
)

func amount(id int64) int64 { return 100000 + id }

// ---- a minimal RESP client (SET / GET over one connection) ----

type redisConn struct {
	c net.Conn
	r *bufio.Reader
}

func redisDial() (*redisConn, error) {
	c, err := net.Dial("tcp", redisAddr)
	if err != nil {
		return nil, err
	}
	return &redisConn{c: c, r: bufio.NewReader(c)}, nil
}

func (rc *redisConn) cmd(args ...string) (string, bool, error) {
	msg := fmt.Sprintf("*%d\r\n", len(args))
	for _, a := range args {
		msg += fmt.Sprintf("$%d\r\n%s\r\n", len(a), a)
	}
	if _, err := rc.c.Write([]byte(msg)); err != nil {
		return "", false, err
	}
	line, err := rc.r.ReadString('\n')
	if err != nil {
		return "", false, err
	}
	switch line[0] {
	case '+':
		return line[1 : len(line)-2], true, nil
	case '-':
		return "", false, fmt.Errorf("redis: %s", line[1:len(line)-2])
	case '$':
		n, _ := strconv.Atoi(line[1 : len(line)-2])
		if n < 0 {
			return "", false, nil // nil bulk string: key missing
		}
		buf := make([]byte, n+2)
		if _, err := ioReadFull(rc.r, buf); err != nil {
			return "", false, err
		}
		return string(buf[:n]), true, nil
	}
	return "", false, fmt.Errorf("redis: unexpected reply %q", line)
}

func ioReadFull(r *bufio.Reader, b []byte) (int, error) {
	n := 0
	for n < len(b) {
		m, err := r.Read(b[n:])
		n += m
		if err != nil {
			return n, err
		}
	}
	return n, nil
}

// ---- the gRPC service (hand-written ServiceDesc over well-known types) ----

type orderServer struct {
	db        *sql.DB
	kp        *kgo.Client // idempotent producer
	rc        *redisConn
	uncertain map[int64]bool // ids whose produce ended in an error: outcome unknown
}

// kafkaHasKey scans the (small) topic for a record with this key. Used only before
// re-producing an order whose previous produce ended in an error, so a produce that had
// in fact succeeded is not written a second time.
func kafkaHasKey(id int64) (bool, error) {
	kc, err := kafka.DialLeader(context.Background(), "tcp", kafkaAddr, topic, 0)
	if err != nil {
		return false, err
	}
	defer kc.Close()
	first, last, err := kc.ReadOffsets()
	if err != nil {
		return false, err
	}
	if _, err := kc.Seek(first, kafka.SeekAbsolute); err != nil {
		return false, err
	}
	want := strconv.FormatInt(id, 10)
	for off := first; off < last; {
		kc.SetReadDeadline(time.Now().Add(15 * time.Second))
		m, err := kc.ReadMessage(1 << 20)
		if err != nil {
			return false, err
		}
		if string(m.Key) == want {
			return true, nil
		}
		off = m.Offset + 1
	}
	return false, nil
}

// redisSet sets one key, re-dialing once if the connection broke (Redis restarted).
func (s *orderServer) redisSet(k, v string) error {
	if s.rc != nil {
		if _, _, err := s.rc.cmd("SET", k, v); err == nil {
			return nil
		}
		s.rc.c.Close()
		s.rc = nil
		pRedisRedial.Hit(nil)
	}
	rc, err := redisDial()
	if err != nil {
		return err
	}
	s.rc = rc
	_, _, err = s.rc.cmd("SET", k, v)
	return err
}

func (s *orderServer) place(ctx context.Context, in *wrapperspb.Int64Value) (*wrapperspb.StringValue, error) {
	id := in.GetValue()
	amt := amount(id)
	// 1. MySQL: upsert -- a retry of an order that is already stored is a no-op.
	if _, err := s.db.ExecContext(ctx, "INSERT INTO stack.orders (id, amount) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id", id, amt); err != nil {
		return nil, storeErr("mysql", id, fmt.Errorf("mysql: %w", err))
	}
	// 2. Kafka, exactly once: skip if already published; otherwise, if an earlier attempt's
	// outcome is unknown, look before producing again.
	var published int
	if err := s.db.QueryRowContext(ctx, "SELECT published FROM stack.orders WHERE id = ?", id).Scan(&published); err != nil {
		return nil, storeErr("mysql", id, fmt.Errorf("mysql: %w", err))
	}
	if published == 0 {
		have := false
		if s.uncertain[id] {
			var err error
			if have, err = kafkaHasKey(id); err != nil {
				return nil, storeErr("kafka-check", id, fmt.Errorf("kafka check: %w", err))
			}
			pKafkaRecheck.Check(true, hydoassert.D{"order": id})
			pDedupHit.Check(have, hydoassert.D{"order": id})
		}
		if !have {
			s.uncertain[id] = true
			res := s.kp.ProduceSync(ctx, &kgo.Record{
				Key:   []byte(strconv.FormatInt(id, 10)),
				Value: []byte(strconv.FormatInt(amt, 10)),
			})
			if err := res.FirstErr(); err != nil {
				return nil, storeErr("kafka", id, fmt.Errorf("kafka: %w", err))
			}
		}
		delete(s.uncertain, id)
		if _, err := s.db.ExecContext(ctx, "UPDATE stack.orders SET published = 1 WHERE id = ?", id); err != nil {
			// Kafka has it; the flag will be set by the retry, which finds the key first.
			s.uncertain[id] = true
			return nil, storeErr("mysql-publish", id, fmt.Errorf("mysql publish flag: %w", err))
		}
	}
	// 3. Redis (idempotent).
	if err := s.redisSet("order:"+strconv.FormatInt(id, 10), strconv.FormatInt(amt, 10)); err != nil {
		return nil, storeErr("redis", id, fmt.Errorf("redis: %w", err))
	}
	pPlaceAcked.Check(true, nil)
	return wrapperspb.String("ok"), nil
}

// storeErr records that a Place failed on one store (coverage: the error paths were
// exercised) and passes the error through.
func storeErr(store string, id int64, err error) error {
	pStoreError.Check(true, hydoassert.D{"store": store, "order": id})
	return err
}

type orderService interface {
	place(context.Context, *wrapperspb.Int64Value) (*wrapperspb.StringValue, error)
}

var serviceDesc = grpc.ServiceDesc{
	ServiceName: "hydo.OrderService",
	HandlerType: (*orderService)(nil),
	Methods: []grpc.MethodDesc{{
		MethodName: "Place",
		Handler: func(srv interface{}, ctx context.Context, dec func(interface{}) error, _ grpc.UnaryServerInterceptor) (interface{}, error) {
			in := new(wrapperspb.Int64Value)
			if err := dec(in); err != nil {
				return nil, err
			}
			return srv.(orderService).place(ctx, in)
		},
	}},
	Streams:  []grpc.StreamDesc{},
	Metadata: "hydo/order.proto",
}

func fail(format string, a ...interface{}) {
	fmt.Printf(format+"\n", a...)
	os.Exit(1)
}

func server() {
	hydoassert.Catalog()
	db, err := sql.Open("mysql", mysqlDSN)
	if err != nil {
		fail("hydostack: mysql open: %v", err)
	}
	db.SetMaxOpenConns(1)
	if _, err := db.Exec("CREATE DATABASE IF NOT EXISTS stack"); err != nil {
		fail("hydostack: mysql create db: %v", err)
	}
	if _, err := db.Exec("CREATE TABLE IF NOT EXISTS stack.orders (id BIGINT PRIMARY KEY, amount BIGINT NOT NULL, published TINYINT NOT NULL DEFAULT 0) ENGINE=InnoDB"); err != nil {
		fail("hydostack: mysql create table: %v", err)
	}
	// The topic is created explicitly (one partition) so the first produce never races
	// auto-creation.
	conn, err := kafka.Dial("tcp", kafkaAddr)
	if err != nil {
		fail("hydostack: kafka dial: %v", err)
	}
	ctl, err := conn.Controller()
	if err != nil {
		fail("hydostack: kafka controller: %v", err)
	}
	cc, err := kafka.Dial("tcp", net.JoinHostPort(ctl.Host, strconv.Itoa(ctl.Port)))
	if err != nil {
		fail("hydostack: kafka controller dial: %v", err)
	}
	_ = cc.CreateTopics(kafka.TopicConfig{Topic: topic, NumPartitions: 1, ReplicationFactor: 1})
	cc.Close()
	conn.Close()
	rc, err := redisDial()
	if err != nil {
		fail("hydostack: redis dial: %v", err)
	}
	// franz-go's producer is idempotent by default (producer id + per-partition sequence
	// numbers; acks=all): the broker discards a batch the client re-sends after a lost
	// response, so internal retries can no longer duplicate a record.
	kp, err := kgo.NewClient(
		kgo.SeedBrokers(kafkaAddr),
		kgo.DefaultProduceTopic(topic),
		kgo.RequiredAcks(kgo.AllISRAcks()),
		kgo.ProducerLinger(0),
	)
	if err != nil {
		fail("hydostack: kafka producer: %v", err)
	}
	lis, err := net.Listen("tcp", grpcAddr)
	if err != nil {
		fail("hydostack: listen: %v", err)
	}
	gs := grpc.NewServer()
	gs.RegisterService(&serviceDesc, &orderServer{db: db, kp: kp, rc: rc, uncertain: map[int64]bool{}})
	fmt.Println("hydostack: server listening on " + grpcAddr)
	if err := gs.Serve(lis); err != nil {
		fail("hydostack: serve: %v", err)
	}
}

// place sends orders from..to-1. In the plain form (keepGoing false: `place <n>`) the first
// failure ends the run, as before. In the range form every order is attempted once with a
// per-order deadline, failures are reported, and ACKed ids go to $HYDO_ACKED.
func place(from, to int64, keepGoing bool) {
	cc, err := grpc.NewClient(grpcAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		fail("HYDO-STACK: client error %v", err)
	}
	defer cc.Close()
	var ackf *os.File
	if p := os.Getenv("HYDO_ACKED"); p != "" {
		ackf, _ = os.OpenFile(p, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	}
	deadline := 120 * time.Second
	if keepGoing {
		deadline = 20 * time.Second
	}
	var acked, failed int64
	for id := from; id < to; id++ {
		out := new(wrapperspb.StringValue)
		ctx, cancel := context.WithTimeout(context.Background(), deadline)
		err := cc.Invoke(ctx, "/hydo.OrderService/Place", wrapperspb.Int64(id), out)
		cancel()
		if err != nil {
			fmt.Printf("hydostack: place %d failed: %v\n", id, err)
			pClientFail.Check(true, hydoassert.D{"order": id})
			failed++
			if !keepGoing {
				break
			}
			continue
		}
		acked++
		if ackf != nil {
			fmt.Fprintf(ackf, "%d\n", id)
		}
	}
	if ackf != nil {
		ackf.Close()
	}
	if keepGoing {
		fmt.Printf("HYDO-STACK: placed acked=%d/%d via gRPC (orders %d..%d, failed=%d)\n", acked, to-from, from, to-1, failed)
	} else {
		fmt.Printf("HYDO-STACK: placed acked=%d/%d via gRPC\n", acked, to-from)
	}
}

// retry re-places every order in 0..n-1 that is not yet ACKed (not listed in file), with
// the SAME id, up to 8 attempts each, one second apart; newly ACKed ids are appended.
func retry(n int64, file string) {
	acked := map[int64]bool{}
	if f, err := os.Open(file); err == nil {
		sc := bufio.NewScanner(f)
		for sc.Scan() {
			if id, err := strconv.ParseInt(sc.Text(), 10, 64); err == nil {
				acked[id] = true
			}
		}
		f.Close()
	}
	cc, err := grpc.NewClient(grpcAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		fail("HYDO-STACK: client error %v", err)
	}
	defer cc.Close()
	ackf, _ := os.OpenFile(file, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0644)
	var tried, ok, attempts int64
	for id := int64(0); id < n; id++ {
		if acked[id] {
			continue
		}
		tried++
		recovered := false
		for a := 0; a < 8; a++ {
			attempts++
			out := new(wrapperspb.StringValue)
			ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
			err := cc.Invoke(ctx, "/hydo.OrderService/Place", wrapperspb.Int64(id), out)
			cancel()
			if err == nil {
				ok++
				recovered = true
				pRetryRecovered.Check(true, hydoassert.D{"order": id, "attempt": a + 1})
				if ackf != nil {
					fmt.Fprintf(ackf, "%d\n", id)
				}
				break
			}
			pClientFail.Check(true, hydoassert.D{"order": id})
			fmt.Printf("hydostack: retry %d attempt %d failed: %v\n", id, a+1, err)
			time.Sleep(time.Second)
		}
		pRetryExhausted.Check(!recovered, hydoassert.D{"order": id})
	}
	if ackf != nil {
		ackf.Close()
	}
	fmt.Printf("HYDO-STACK: retried %d un-acked orders, %d now acked (%d attempts)\n", tried, ok, attempts)
}

// oracle: every acked order (0..n-1) must be in MySQL with its amount and in the Kafka
// topic; Redis is a cache, so a missing key is allowed but a WRONG value is not. The first
// problem found (in id order: mysql, kafka, redis) is reported.
func oracle(n int64, ackedFile string) {
	hydoassert.Catalog()
	// A store that cannot be read is itself a violation (and ends the check).
	unreadable := func(what string, err error) {
		pStoresReadable.Check(false, hydoassert.D{"what": what, "error": err.Error()})
		fmt.Printf("HYDO-ORACLE: VIOLATION %s (%v)\n", what, err)
	}
	db, err := sql.Open("mysql", mysqlDSN)
	if err != nil {
		unreadable("mysql unreachable", err)
		return
	}
	mysqlHas := map[int64]int64{}
	mysqlPub := map[int64]bool{}
	rows, err := db.Query("SELECT id, amount, published FROM stack.orders ORDER BY id")
	if err != nil {
		unreadable("mysql table unreadable", err)
		return
	}
	for rows.Next() {
		var id, amt, pub int64
		if err := rows.Scan(&id, &amt, &pub); err == nil {
			mysqlHas[id] = amt
			mysqlPub[id] = pub != 0
		}
	}
	rows.Close()

	kafkaHas := map[int64]int64{}
	kafkaCount := map[int64]int{}
	kc, err := kafka.DialLeader(context.Background(), "tcp", kafkaAddr, topic, 0)
	if err != nil {
		unreadable("kafka topic unreadable", err)
		return
	}
	first, last, err := kc.ReadOffsets()
	if err != nil {
		unreadable("kafka offsets unreadable", err)
		return
	}
	if _, err := kc.Seek(first, kafka.SeekAbsolute); err != nil {
		unreadable("kafka seek", err)
		return
	}
	for off := first; off < last; {
		kc.SetReadDeadline(time.Now().Add(30 * time.Second))
		m, err := kc.ReadMessage(1 << 20)
		if err != nil {
			unreadable(fmt.Sprintf("kafka read at offset %d", off), err)
			return
		}
		id, _ := strconv.ParseInt(string(m.Key), 10, 64)
		v, _ := strconv.ParseInt(string(m.Value), 10, 64)
		kafkaHas[id] = v
		kafkaCount[id]++
		off = m.Offset + 1
	}
	kc.Close()

	rc, err := redisDial()
	if err != nil {
		unreadable("redis unreachable", err)
		return
	}
	pStoresReadable.Check(true, nil)

	// which ids must be consistent: all of 0..n-1, or exactly the ACKed ones
	ids := []int64{}
	ackedSet := map[int64]bool{}
	if ackedFile != "" {
		f, err := os.Open(ackedFile)
		if err == nil {
			sc := bufio.NewScanner(f)
			for sc.Scan() {
				if id, err := strconv.ParseInt(sc.Text(), 10, 64); err == nil {
					ids = append(ids, id)
					ackedSet[id] = true
				}
			}
			f.Close()
		}
	} else {
		for id := int64(0); id < n; id++ {
			ids = append(ids, id)
		}
	}
	// Every property is evaluated for every order (so the property table is complete);
	// the HYDO-ORACLE verdict is the first violation in id order, as before.
	verdict := ""
	violate := func(format string, a ...interface{}) {
		if verdict == "" {
			verdict = fmt.Sprintf("HYDO-ORACLE: VIOLATION "+format, a...)
		}
	}
	cached := 0
	for _, id := range ids {
		want := amount(id)
		m, inM := mysqlHas[id]
		k, inK := kafkaHas[id]
		if !pInMySQL.Check(inM && m == want, hydoassert.D{"order": id, "want": want, "got": show(m, inM), "in_kafka": inK}) {
			violate("mysql order=%d expected=%d got=%s (kafka has it: %v)", id, want, show(m, inM), inK)
		}
		if !pInKafka.Check(inK && k == want, hydoassert.D{"order": id, "want": want, "got": show(k, inK), "in_mysql": inM}) {
			violate("kafka order=%d expected=%d got=%s (mysql has it: %v)", id, want, show(k, inK), inM)
		}
		if inK && !pKafkaOnce.Check(kafkaCount[id] == 1, hydoassert.D{"order": id, "count": kafkaCount[id]}) {
			violate("kafka order=%d present %d times (must be exactly once)", id, kafkaCount[id])
		}
		if ackedFile != "" && inM && !pPublished.Check(mysqlPub[id], hydoassert.D{"order": id}) {
			violate("mysql order=%d acked but not marked published", id)
		}
		v, ok, err := rc.cmd("GET", "order:"+strconv.FormatInt(id, 10))
		if err != nil {
			pStoresReadable.Check(false, hydoassert.D{"what": "redis read", "order": id, "error": err.Error()})
			violate("redis order=%d read failed (%v)", id, err)
			continue
		}
		if !ok {
			pRedisMiss.Check(true, hydoassert.D{"order": id})
			continue
		}
		if !pRedisNotWrong.Check(v == strconv.FormatInt(want, 10), hydoassert.D{"order": id, "want": want, "got": v}) {
			violate("redis order=%d expected=%d got=%s (stale cache)", id, want, v)
			continue
		}
		cached++
	}
	if ackedFile == "" {
		if verdict != "" {
			fmt.Println(verdict)
			return
		}
		fmt.Printf("HYDO-ORACLE: OK orders=%d consistent (mysql=%d kafka=%d redis-cached=%d)\n", n, len(mysqlHas), len(kafkaHas), cached)
		return
	}
	// Orders the client was never ACKed for must not have taken effect: none may be in
	// Kafka (that would be an order applied but reported as failed). A MySQL row with
	// published=0 is a pending order that was never confirmed -- allowed and counted.
	pending := 0
	for id := int64(0); id < n; id++ {
		if ackedSet[id] {
			continue
		}
		if kafkaCount[id] > 0 {
			pAppliedNoAck.Hit(hydoassert.D{"order": id, "kafka_count": kafkaCount[id]})
			violate("order=%d is in kafka (%d times) but the client was never ACKed", id, kafkaCount[id])
		}
		if _, inM := mysqlHas[id]; inM {
			pPending.Check(true, hydoassert.D{"order": id})
			pending++
		}
	}
	if verdict != "" {
		fmt.Println(verdict)
		return
	}
	fmt.Printf("HYDO-ORACLE: OK acked=%d/%d exactly-once (not-acked: %d, pending-in-mysql=%d; redis-cached=%d)\n",
		len(ids), n, n-int64(len(ids)), pending, cached)
}

func show(v int64, ok bool) string {
	if !ok {
		return "MISSING"
	}
	return strconv.FormatInt(v, 10)
}

func main() {
	if len(os.Args) < 2 {
		fail("usage: hydostack server | place <n> | place <from> <to> | retry <n> <ackedfile> | oracle <n> [ackedfile]")
	}
	n := int64(0)
	if len(os.Args) > 2 {
		n, _ = strconv.ParseInt(os.Args[2], 10, 64)
	}
	switch os.Args[1] {
	case "server":
		server()
	case "place":
		if len(os.Args) > 3 {
			to, _ := strconv.ParseInt(os.Args[3], 10, 64)
			place(n, to, true)
		} else {
			place(0, n, false)
		}
	case "retry":
		if len(os.Args) < 4 {
			fail("usage: hydostack retry <n> <ackedfile>")
		}
		retry(n, os.Args[3])
	case "oracle":
		f := ""
		if len(os.Args) > 3 {
			f = os.Args[3]
		}
		oracle(n, f)
	default:
		fail("hydostack: unknown mode %s", os.Args[1])
	}
}
