#!/usr/bin/env python3
"""Generate one patched copy of main.go per defect and a unified diff for each."""
import difflib, os, sys

here = os.path.dirname(os.path.abspath(__file__))
orig = open(os.path.join(here, "orig.go")).read()
out = sys.argv[1]


def sub(src, old, new, count=1):
    if src.count(old) != count:
        raise SystemExit("pattern found %d times, want %d:\n%s" % (src.count(old), count, old))
    return src.replace(old, new)


D = {}

# d1 kafka-first: publish to Kafka before the order is recorded in MySQL, and call a MySQL
# failure "not applied".
s = orig
s = sub(s, """	// 1. MySQL: upsert -- a retry of an order that is already stored is a no-op.
	if _, err := s.db.ExecContext(ctx, "INSERT INTO stack.orders (id, amount) VALUES (?, ?) ON DUPLICATE KEY UPDATE id = id", id, amt); err != nil {
		return nil, s.failBefore("mysql", id, fmt.Errorf("mysql: %w", err))
	}
""", """	// BENCH DEFECT d1 (kafka-first): publish first, then record the order in MySQL. A MySQL
	// failure is reported as "not applied" because the order is not in our database -- but
	// it is already in Kafka, and the client's retry publishes it a second time.
	res := s.kp.ProduceSync(ctx, &kgo.Record{
		Key:   []byte(strconv.FormatInt(id, 10)),
		Value: []byte(strconv.FormatInt(amt, 10)),
	})
	if err := res.FirstErr(); err != nil {
		return nil, outcomeUnknown("kafka", id, fmt.Errorf("kafka: %w", err))
	}
	if _, err := s.db.ExecContext(ctx, "INSERT INTO stack.orders (id, amount, published) VALUES (?, ?, 1) ON DUPLICATE KEY UPDATE published = 1", id, amt); err != nil {
		return nil, notApplied("mysql", id, fmt.Errorf("mysql: %w", err))
	}
	if false {
""")
s = sub(s, """	// 3. Redis: a cache, so best effort.""", """	}
	// 3. Redis: a cache, so best effort.""")
D["d1-kafka-first"] = s

# d2 batched-publish-flag: the published flag is written behind, five orders per UPDATE; the
# batch waiting in memory is lost on a power cut although its orders were ACKed.
s = sub(orig, """	uncertain map[int64]bool // ids whose produce ended in an error: outcome unknown""",
        """	uncertain map[int64]bool // ids whose produce ended in an error: outcome unknown
	pending   []int64        // BENCH DEFECT d2: published flags not yet written""")
s = sub(s, """		delete(s.uncertain, id)
		if _, err := s.db.ExecContext(ctx, "UPDATE stack.orders SET published = 1 WHERE id = ?", id); err != nil {
			// Kafka has it; the flag will be set by the retry, which finds the key first.
			s.uncertain[id] = true
			return nil, outcomeUnknown("mysql-publish", id, fmt.Errorf("mysql publish flag: %w", err))
		}""", """		delete(s.uncertain, id)
		// BENCH DEFECT d2 (batched-publish-flag): "it is only bookkeeping" -- the published
		// flags are written five at a time, and the order is ACKed before its flag is stored.
		s.pending = append(s.pending, id)
		if len(s.pending) >= 5 {
			q, args := "UPDATE stack.orders SET published = 1 WHERE id IN (?", []interface{}{s.pending[0]}
			for _, p := range s.pending[1:] {
				q += ", ?"
				args = append(args, p)
			}
			if _, err := s.db.ExecContext(ctx, q+")", args...); err == nil {
				s.pending = nil
			}
		}""")
D["d2-batched-publish-flag"] = s

# d3 blind-retry: a retry of an order whose earlier produce had an unknown outcome produces
# again without looking in Kafka first.
s = sub(orig, """		if s.uncertain[id] {
			var err error""", """		// BENCH DEFECT d3 (blind-retry): the lookup before re-producing is skipped.
		if false && s.uncertain[id] {
			var err error""")
D["d3-blind-retry"] = s

# d4 produce-timeout-definite: the front door gives the produce its own 5 s budget and reports
# a produce that ran out of it as "not applied".
s = sub(orig, """			res := s.kp.ProduceSync(ctx, &kgo.Record{
				Key:   []byte(strconv.FormatInt(id, 10)),
				Value: []byte(strconv.FormatInt(amt, 10)),
			})
			if err := res.FirstErr(); err != nil {
				return nil, outcomeUnknown("kafka", id, fmt.Errorf("kafka: %w", err))
			}""", """			// BENCH DEFECT d4 (produce-timeout-definite): "don't keep the client waiting" -- the
			// produce gets 5 s, and a produce that timed out is reported as not applied. But a
			// timed-out produce may still be written once the broker gets to it.
			done := make(chan error, 1)
			go func() {
				done <- s.kp.ProduceSync(ctx, &kgo.Record{
					Key:   []byte(strconv.FormatInt(id, 10)),
					Value: []byte(strconv.FormatInt(amt, 10)),
				}).FirstErr()
			}()
			var err error
			select {
			case err = <-done:
			case <-time.After(5 * time.Second):
				err = fmt.Errorf("produce timed out after 5s")
			}
			if err != nil {
				return nil, notApplied("kafka", id, fmt.Errorf("kafka: %w", err))
			}""")
D["d4-produce-timeout-definite"] = s

# d5 myisam: the orders table is created with a non-transactional, non-crash-safe engine.
s = sub(orig, """published TINYINT NOT NULL DEFAULT 0) ENGINE=InnoDB\"""", """published TINYINT NOT NULL DEFAULT 0) ENGINE=MyISAM\"""")
s = sub(s, """	if _, err := db.Exec("CREATE TABLE IF NOT EXISTS stack.orders""", """	// BENCH DEFECT d5 (myisam): MyISAM ("simpler, no transactions needed") is not crash
	// safe and never fsyncs its data file: committed rows can vanish on a power cut.
	if _, err := db.Exec("CREATE TABLE IF NOT EXISTS stack.orders""")
D["d5-myisam"] = s

# d6 client-deadline-definite: the client counts its own timeout as a definite failure, and
# the retry pass skips definite failures.
s = sub(orig, """	if status.Code(err) == codes.FailedPrecondition {
		pClientDefinite.Check(true, nil)""", """	// BENCH DEFECT d6 (client-deadline-definite): "it timed out, so it did not happen".
	if c := status.Code(err); c == codes.FailedPrecondition || c == codes.DeadlineExceeded {
		pClientDefinite.Check(true, nil)""")
s = sub(s, """		f.Close()
	}
	cc, err := grpc.NewClient(grpcAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		fail("HYDO-STACK: client error %v", err)
	}
	defer cc.Close()
	ackf, _ := os.OpenFile(file""", """		f.Close()
	}
	// BENCH DEFECT d6, second half: an order whose last outcome was a definite failure is not
	// retried ("the user was told it failed; they will place it again").
	if p := os.Getenv("HYDO_OUTCOMES"); p != "" {
		last := map[int64]string{}
		if f, err := os.Open(p); err == nil {
			sc := bufio.NewScanner(f)
			for sc.Scan() {
				var id int64
				var o string
				if _, err := fmt.Sscanf(sc.Text(), "%d %s", &id, &o); err == nil {
					last[id] = o
				}
			}
			f.Close()
		}
		for id, o := range last {
			if o == "failed" {
				acked[id] = true
			}
		}
	}
	cc, err := grpc.NewClient(grpcAddr, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		fail("HYDO-STACK: client error %v", err)
	}
	defer cc.Close()
	ackf, _ := os.OpenFile(file""")
D["d6-client-deadline-definite"] = s

# d7 skip-last-record: an off-by-one in the Kafka lookup -- the scan stops one record early, so
# an uncertain order that is the topic's LAST record is produced again.
s = sub(orig, """	want := strconv.FormatInt(id, 10)
	for off := first; off < last; {""", """	want := strconv.FormatInt(id, 10)
	// BENCH DEFECT d7 (skip-last-record): "last" read as the offset of the last record, but it
	// is the next offset to be written -- so the scan stops one record early.
	for off := first; off < last-1; {""")
D["d7-skip-last-record"] = s

os.makedirs(out, exist_ok=True)
for name, src in D.items():
    d = difflib.unified_diff(orig.splitlines(True), src.splitlines(True), "a/main.go", "b/main.go")
    open(os.path.join(out, name + ".patch"), "w").write("".join(d))
print(" ".join(D))
