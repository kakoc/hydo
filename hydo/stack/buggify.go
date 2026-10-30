package main

// BUGGIFY fault points of the order service (Milestone 56; hydoassert.Buggify).
//
// Every store call of Place goes through one of the wrappers below, which can make the call
// misbehave the way a real dependency does -- slow, failing before the request reaches the
// store, or doing the work and then losing the reply -- and otherwise make the call unchanged.
// The injected failure always takes the code's own error path for that call, so nothing here
// creates a state the real dependency could not. Off unless the run carries a BUGGIFY seed
// (see hydoassert), so without one the service behaves exactly as before.
//
// The wrappers are written against only what every version of the service has (s.db, s.kp,
// s.redisSet, kafkaHasKey), so hvx/bench/addbuggify.py can wire them into the benchmark's
// patched copies of older versions as well.

import (
	"context"
	"database/sql"
	"errors"
	"strings"
	"time"

	"github.com/twmb/franz-go/pkg/kgo"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/wrapperspb"

	"hydostack/hydoassert"
)

var (
	bgMySQLSlow  = hydoassert.Buggify("mysql.slow", "a MySQL statement takes 2 s")
	bgInsertErr  = hydoassert.Buggify("mysql.insert.error", "the order INSERT fails before it reaches MySQL")
	bgInsertLost = hydoassert.Buggify("mysql.insert.lost_reply", "the order INSERT commits but its reply is lost")
	bgMarkErr    = hydoassert.Buggify("mysql.mark_sending.error", "marking the send as started fails before it reaches MySQL")
	bgMarkLost   = hydoassert.Buggify("mysql.mark_sending.lost_reply", "marking the send as started commits but its reply is lost")
	bgPubErr     = hydoassert.Buggify("mysql.publish_flag.error", "setting the published flag fails before it reaches MySQL")
	bgPubLost    = hydoassert.Buggify("mysql.publish_flag.lost_reply", "setting the published flag commits but its reply is lost")
	bgKafkaSlow  = hydoassert.Buggify("kafka.slow", "the Kafka produce waits 6 s before it is sent")
	bgKafkaErr   = hydoassert.Buggify("kafka.produce.error", "the produce fails before the record is sent")
	bgKafkaLost  = hydoassert.Buggify("kafka.produce.lost_ack", "the record is written but the produce reports a timeout")
	bgCheckErr   = hydoassert.Buggify("kafka.check.error", "the Kafka lookup for an earlier send fails")
	bgRedisErr   = hydoassert.Buggify("redis.set.error", "the cache write fails")
	bgAckSlow    = hydoassert.Buggify("grpc.ack.slow", "the ACK leaves 3 s late")
	bgAckLost    = hydoassert.Buggify("grpc.ack.lost", "the order is applied but the reply to the client is lost")
)

// execBG is s.db.ExecContext with the MySQL fault points of the statement's kind.
func (s *orderServer) execBG(ctx context.Context, q string, args ...interface{}) (sql.Result, error) {
	kind, errP, lostP := "other", (*hydoassert.Point)(nil), (*hydoassert.Point)(nil)
	switch {
	case strings.HasPrefix(q, "INSERT"):
		kind, errP, lostP = "insert", bgInsertErr, bgInsertLost
	case strings.Contains(q, "published = 2"):
		kind, errP, lostP = "mark_sending", bgMarkErr, bgMarkLost
	case strings.Contains(q, "published = 1"):
		kind, errP, lostP = "publish_flag", bgPubErr, bgPubLost
	}
	d := hydoassert.D{"stmt": kind}
	if bgMySQLSlow.Fire(d) {
		time.Sleep(2 * time.Second)
	}
	if errP != nil && errP.Fire(d) {
		return nil, errors.New("buggify: connection reset before the statement was sent")
	}
	res, err := s.db.ExecContext(ctx, q, args...)
	if err == nil && lostP != nil && lostP.Fire(d) {
		return nil, errors.New("buggify: connection reset after the statement ran (reply lost)")
	}
	return res, err
}

// produceBG is s.kp.ProduceSync with the Kafka fault points.
func (s *orderServer) produceBG(ctx context.Context, rs ...*kgo.Record) kgo.ProduceResults {
	if bgKafkaSlow.Fire(nil) {
		time.Sleep(6 * time.Second)
	}
	fail := func(msg string) kgo.ProduceResults {
		out := make(kgo.ProduceResults, len(rs))
		for i, r := range rs {
			out[i] = kgo.ProduceResult{Record: r, Err: errors.New(msg)}
		}
		return out
	}
	if bgKafkaErr.Fire(nil) {
		return fail("buggify: NOT_LEADER_FOR_PARTITION, the record was not sent")
	}
	res := s.kp.ProduceSync(ctx, rs...)
	if res.FirstErr() == nil && bgKafkaLost.Fire(nil) {
		return fail("buggify: request timed out (the record was written)")
	}
	return res
}

// kafkaHasKeyBG is kafkaHasKey with a failing lookup.
func (s *orderServer) kafkaHasKeyBG(ctx context.Context, id int64) (bool, error) {
	if bgCheckErr.Fire(hydoassert.D{"order": id}) {
		return false, errors.New("buggify: kafka lookup failed (broker connection reset)")
	}
	return kafkaHasKey(ctx, id)
}

// redisSetBG is s.redisSet with a failing cache write.
func (s *orderServer) redisSetBG(k, v string) error {
	if bgRedisErr.Fire(nil) {
		return errors.New("buggify: redis connection reset")
	}
	return s.redisSet(k, v)
}

// ackBG is Place's successful return, with a late or lost reply.
func (s *orderServer) ackBG() (*wrapperspb.StringValue, error) {
	if bgAckSlow.Fire(nil) {
		time.Sleep(3 * time.Second)
	}
	if bgAckLost.Fire(nil) {
		return nil, status.Error(codes.Unavailable, "buggify: the reply was lost (the order was applied)")
	}
	return wrapperspb.String("ok"), nil
}
