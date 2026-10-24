package main

// Tests against real stores. Each test is skipped unless its address variable is set;
// native-test.sh starts MySQL, PostgreSQL, Kafka, Redis and etcd from the pinned store
// paths in a temp dir, sets the variables and runs `go test -run Store -v`.
//
//	HYDO_TEST_MYSQL     user:pw@tcp(host:port)/        (a fresh server; the test creates db t)
//	HYDO_TEST_POSTGRES  postgres://user@host:port/db?sslmode=disable
//	HYDO_TEST_KAFKA     host:port
//	HYDO_TEST_REDIS     host:port
//	HYDO_TEST_ETCD      http://host:port
//	HYDO_TEST_DEADPORT  a host:port nothing listens on

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/twmb/franz-go/pkg/kadm"
	"github.com/twmb/franz-go/pkg/kgo"
)

func env(t *testing.T, k string) string {
	v := os.Getenv(k)
	if v == "" {
		t.Skipf("%s not set (run native-test.sh)", k)
	}
	return v
}

func lines(t *testing.T, args ...string) string {
	t.Helper()
	recs, ordered, err := readStore(args)
	if err != nil {
		t.Fatalf("hydo-read %q: %v", args, err)
	}
	return strings.Join(encodeAll(recs, ordered), "\n")
}

func expectCannot(t *testing.T, args ...string) {
	t.Helper()
	_, _, err := readStore(args)
	if _, ok := err.(cannotRead); !ok {
		t.Fatalf("hydo-read %q: want could-not-read, got %v", args, err)
	}
}

func same(t *testing.T, what, got, want string) {
	t.Helper()
	if got != want {
		t.Fatalf("%s:\n got:\n%s\nwant:\n%s", what, got, want)
	}
}

func TestStoreMySQL(t *testing.T) {
	dsn := env(t, "HYDO_TEST_MYSQL")
	db, err := sql.Open("mysql", dsn+"?multiStatements=true")
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	if _, err := db.Exec(`DROP DATABASE IF EXISTS t; CREATE DATABASE t;
		CREATE TABLE t.orders (id INT PRIMARY KEY, amount INT, published TINYINT, note VARCHAR(20));
		INSERT INTO t.orders VALUES (2, 100002, 1, NULL), (10, 100010, 1, 'x'), (1, 100001, 0, 'quote"me'), (3, 999, 2, '');`); err != nil {
		t.Fatal(err)
	}
	same(t, "rows", lines(t, "mysql", dsn, "SELECT * FROM t.orders"),
		`{"amount":"100001","id":"1","note":"quote\"me","published":"0"}
{"amount":"100002","id":"2","note":null,"published":"1"}
{"amount":"100010","id":"10","note":"x","published":"1"}
{"amount":"999","id":"3","note":"","published":"2"}`)
	// the URL form of the DSN gives the same rows
	u := "mysql://" + strings.Replace(strings.Replace(strings.TrimSuffix(dsn, "/"), "@tcp(", "@", 1), ")", "", 1) + "/t"
	same(t, "url dsn", lines(t, "mysql", u, "SELECT id FROM orders WHERE published = 1"), `{"id":"10"}`+"\n"+`{"id":"2"}`)
	// checks: published ids exactly once in a list; wrong amounts counted
	d := writeFiles(t, map[string]string{"acked": "2\n10\n"})
	src := "mysql=mysql " + dsn + ` "SELECT id FROM t.orders WHERE published = 1"`
	if rc, last := run(t, "exactly-once", "--left", "file "+d+"/acked", "--right", src); rc != 0 {
		t.Fatal(rc, last)
	}
	rc, last := run(t, "count", "--in", "mysql "+dsn+` "SELECT id FROM t.orders WHERE amount <> 100000 + id"`, "--eq", "0")
	same(t, "count", last, `count violated: mysql has 1 record, want == 0; first: {"id":"3"}`)
	if rc != 1 {
		t.Fatal(rc)
	}
	// errors: a bad query and a dead server are "could not read" (exit 2)
	expectCannot(t, "mysql", dsn, "SELECT * FROM t.nope")
	if dead := os.Getenv("HYDO_TEST_DEADPORT"); dead != "" {
		expectCannot(t, "--timeout", "2", "mysql", "root@tcp("+dead+")/", "SELECT 1")
		if rc, _ := run(t, "subset", "--left", "range 0 1", "--right", "mysql root@tcp("+dead+")/ \"SELECT 1\" --timeout 2"); rc != 2 {
			t.Fatalf("dead mysql: rc %d", rc)
		}
	}
}

func TestStorePostgres(t *testing.T) {
	dsn := env(t, "HYDO_TEST_POSTGRES")
	db, err := sql.Open("postgres", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	for _, q := range []string{`DROP TABLE IF EXISTS categories`,
		`CREATE TABLE categories (id SERIAL PRIMARY KEY, title TEXT UNIQUE, hidden BOOLEAN, created TIMESTAMPTZ)`,
		`INSERT INTO categories (title, hidden, created) VALUES ('All', false, '2026-01-01 00:00:00+00'), ('c0', true, NULL), ('c4', false, NULL)`} {
		if _, err := db.Exec(q); err != nil {
			t.Fatal(q, err)
		}
	}
	same(t, "rows", lines(t, "postgres", dsn, "SELECT title, hidden, created FROM categories"),
		`{"created":"2026-01-01T00:00:00Z","hidden":"false","title":"All"}
{"created":null,"hidden":"false","title":"c4"}
{"created":null,"hidden":"true","title":"c0"}`)
	d := writeFiles(t, map[string]string{"acked": "0\n4\n7\n", "failed": "9\n"})
	cats := `categories=postgres ` + dsn + ` "SELECT title FROM categories"`
	rc, last := run(t, "exactly-once", "--left", "acked=file "+d+"/acked --add-prefix c", "--right", cats)
	same(t, "exactly-once", last, "exactly-once violated: acked c7 is in categories 0 times (of 3 in acked); missing 1: c7")
	if rc != 1 {
		t.Fatal(rc)
	}
	if rc, last := run(t, "disjoint", "--left", "failed=file "+d+"/failed --add-prefix c", "--right", cats); rc != 0 {
		t.Fatal(rc, last)
	}
	expectCannot(t, "postgres", dsn, "SELECT * FROM nope")
	if dead := os.Getenv("HYDO_TEST_DEADPORT"); dead != "" {
		expectCannot(t, "--timeout", "2", "postgres", "postgres://x@"+dead+"/x?sslmode=disable", "SELECT 1")
	}
}

func TestStoreKafka(t *testing.T) {
	boot := env(t, "HYDO_TEST_KAFKA")
	cl, err := kgo.NewClient(kgo.SeedBrokers(boot), kgo.DefaultProduceTopic("orders"),
		kgo.RecordPartitioner(kgo.ManualPartitioner()))
	if err != nil {
		t.Fatal(err)
	}
	defer cl.Close()
	adm := kadm.NewClient(cl)
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	adm.DeleteTopics(ctx, "orders")
	time.Sleep(2 * time.Second)
	if r, err := adm.CreateTopic(ctx, 2, 1, nil, "orders"); err != nil || r.Err != nil {
		t.Fatal(err, r.Err)
	}
	time.Sleep(2 * time.Second)
	ts := time.UnixMilli(1767225600000)
	var recs []*kgo.Record
	for i, kv := range [][3]string{{"0", "100000", "0"}, {"1", "100001", "1"}, {"2", "100002", "0"}, {"2", "100002", "0"}, {"", "nokey", "1"}} {
		r := &kgo.Record{Value: []byte(kv[1]), Timestamp: ts.Add(time.Duration(i) * time.Millisecond)}
		if kv[0] != "" {
			r.Key = []byte(kv[0])
		}
		if kv[2] == "1" {
			r.Partition = 1
		}
		recs = append(recs, r)
	}
	if err := cl.ProduceSync(ctx, recs...).FirstErr(); err != nil {
		t.Fatal(err)
	}
	same(t, "records", lines(t, "kafka", boot, "orders"),
		`{"key":"0","offset":0,"partition":0,"timestamp_ms":1767225600000,"value":"100000"}
{"key":"2","offset":1,"partition":0,"timestamp_ms":1767225600002,"value":"100002"}
{"key":"2","offset":2,"partition":0,"timestamp_ms":1767225600003,"value":"100002"}
{"key":"1","offset":0,"partition":1,"timestamp_ms":1767225600001,"value":"100001"}
{"key":null,"offset":1,"partition":1,"timestamp_ms":1767225600004,"value":"nokey"}`)
	same(t, "base64", strings.Split(lines(t, "kafka", boot, "orders", "--base64"), "\n")[0],
		`{"key":"MA==","offset":0,"partition":0,"timestamp_ms":1767225600000,"value":"MTAwMDAw"}`)
	d := writeFiles(t, map[string]string{"acked": "0\n1\n2\n"})
	rc, last := run(t, "exactly-once", "--left", "acked=file "+d+"/acked", "--right", "kafka "+boot+" orders")
	same(t, "exactly-once", last, "exactly-once violated: acked 2 is in kafka 2 times (of 3 in acked); duplicated 1: 2 (x2)")
	if rc != 1 {
		t.Fatal(rc)
	}
	// a missing topic is "could not read", and reading it does not create it
	expectCannot(t, "kafka", boot, "no-such-topic")
	time.Sleep(time.Second)
	td, err := adm.ListTopics(ctx)
	if err != nil {
		t.Fatal(err)
	}
	if td.Has("no-such-topic") {
		t.Fatal("reading a missing topic created it")
	}
	// an empty topic: zero records, not an error
	if r, err := adm.CreateTopic(ctx, 1, 1, nil, "empty"); err == nil && r.Err == nil {
		time.Sleep(2 * time.Second)
	}
	same(t, "empty", lines(t, "kafka", boot, "empty"), "")
	if dead := os.Getenv("HYDO_TEST_DEADPORT"); dead != "" {
		expectCannot(t, "--timeout", "3", "kafka", dead, "orders")
	}
}

func TestStoreRedis(t *testing.T) {
	addr := env(t, "HYDO_TEST_REDIS")
	rc, err := dialRedis(addr, 5*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	for _, c := range [][]string{{"FLUSHALL"}, {"SET", "order:1", "100001"}, {"SET", "order:2", "7"}, {"SET", "other", "x"},
		{"HSET", "order:h", "b", "2", "a", "1"}, {"SADD", "order:s", "z", "y"}, {"RPUSH", "order:l", "q", "p"},
		{"ZADD", "order:z", "2", "m2", "1", "m1"}} {
		if _, err := rc.do(c...); err != nil {
			t.Fatal(c, err)
		}
	}
	rc.c.Close()
	same(t, "scan", lines(t, "redis", addr, "scan", "order:*"),
		`{"key":"order:1","type":"string","value":"100001"}
{"key":"order:2","type":"string","value":"7"}
{"key":"order:h","type":"hash","value":{"a":"1","b":"2"}}
{"key":"order:l","type":"list","value":["q","p"]}
{"key":"order:s","type":"set","value":["y","z"]}
{"key":"order:z","type":"zset","value":[["m1","1"],["m2","2"]]}`)
	same(t, "get", lines(t, "redis", addr, "get", "order:1", "order:9"),
		`{"key":"order:1","value":"100001"}`+"\n"+`{"key":"order:9","value":null}`)
	same(t, "hgetall", lines(t, "redis", addr, "hgetall", "order:h"),
		`{"field":"a","key":"order:h","value":"1"}`+"\n"+`{"field":"b","key":"order:h","value":"2"}`)
	same(t, "lrange", lines(t, "redis", addr, "lrange", "order:l"),
		`{"index":0,"key":"order:l","value":"q"}`+"\n"+`{"index":1,"key":"order:l","value":"p"}`)
	same(t, "zrange", lines(t, "redis", addr, "zrange", "order:z"),
		`{"key":"order:z","member":"m1","score":"1"}`+"\n"+`{"key":"order:z","member":"m2","score":"2"}`)
	same(t, "cmd", lines(t, "redis", addr, "cmd", "DBSIZE"), `{"reply":7}`)
	d := writeFiles(t, map[string]string{"truth.jsonl": `{"id":"1","amount":"100001"}` + "\n" + `{"id":"2","amount":"100002"}` + "\n"})
	code, last := run(t, "never-wrong-value", "--cache", `redis `+addr+` get order:1 order:2 order:3 --strip-prefix order:`,
		"--truth", "mysql=jsonl "+d+"/truth.jsonl --key id --value amount")
	same(t, "never-wrong-value", last, "never-wrong-value violated: 1 value wrong in redis: 2=7 but mysql has 100002")
	if code != 1 {
		t.Fatal(code)
	}
	if dead := os.Getenv("HYDO_TEST_DEADPORT"); dead != "" {
		expectCannot(t, "redis", dead, "get", "x")
	}
}

func TestStoreEtcd(t *testing.T) {
	ep := env(t, "HYDO_TEST_ETCD")
	put := func(k, v string) {
		b, _ := json.Marshal(map[string]string{"key": base64.StdEncoding.EncodeToString([]byte(k)), "value": base64.StdEncoding.EncodeToString([]byte(v))})
		resp, err := http.Post(ep+"/v3/kv/put", "application/json", bytes.NewReader(b))
		if err != nil || resp.StatusCode != 200 {
			t.Fatal(err, resp)
		}
		resp.Body.Close()
	}
	put("k1", "v1")
	put("k2", "v2")
	put("k1", "v1b")
	put("l1", "x")
	got := lines(t, "etcd", ep, "get", "k", "--prefix")
	// revisions depend on the fresh server's history: check the shape and the values
	var r1, r2 Record
	ls := strings.Split(got, "\n")
	if len(ls) != 2 || json.Unmarshal([]byte(ls[0]), &r1) != nil || json.Unmarshal([]byte(ls[1]), &r2) != nil {
		t.Fatalf("got %q", got)
	}
	if r1["key"] != "k1" || r1["value"] != "v1b" || r1["version"].(float64) != 2 || r2["key"] != "k2" ||
		r1["mod_revision"].(float64) <= r2["mod_revision"].(float64) {
		t.Fatalf("got %q", got)
	}
	same(t, "serializable single key", lines(t, "etcd", ep, "get", "l1", "--serializable"),
		strings.Replace(lines(t, "etcd", ep, "get", "l1"), "", "", 1))
	st := lines(t, "etcd", ep, "status")
	if !strings.Contains(st, `"revision":`) || strings.Contains(st, `"revision":0`) {
		t.Fatalf("status %q", st)
	}
	if dead := os.Getenv("HYDO_TEST_DEADPORT"); dead != "" {
		expectCannot(t, "--timeout", "2", "etcd", "http://"+dead, "get", "k")
	}
}
