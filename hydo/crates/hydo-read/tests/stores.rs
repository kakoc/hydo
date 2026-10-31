//! The Go program's stores_test.go, ported: tests against real stores. Each test is skipped
//! unless its address variable is set; native-test.sh starts MySQL, PostgreSQL, Kafka, Redis and
//! etcd, sets the variables and runs these tests.
//!
//!   HYDO_TEST_MYSQL     user:pw@tcp(host:port)/        (a fresh server; the test creates db t)
//!   HYDO_TEST_POSTGRES  postgres://user@host:port/db?sslmode=disable
//!   HYDO_TEST_KAFKA     host:port
//!   HYDO_TEST_REDIS     host:port
//!   HYDO_TEST_ETCD      http://host:port
//!   HYDO_TEST_DEADPORT  a host:port nothing listens on

mod common;

use common::{run, write_files};
use hydo_read::json::Val;
use hydo_read::{encode_all, read_store, Error};
use std::time::Duration;

fn env(k: &str) -> Option<String> {
    let v = std::env::var(k).unwrap_or_default();
    if v.is_empty() {
        eprintln!("{k} not set (run native-test.sh): skipped");
        return None;
    }
    Some(v)
}

fn bs(args: &[&str]) -> Vec<Vec<u8>> {
    args.iter().map(|a| a.as_bytes().to_vec()).collect()
}

fn lines(args: &[&str]) -> String {
    let (recs, ordered) = read_store(&bs(args)).unwrap_or_else(|e| panic!("hydo-read {args:?}: {e}"));
    let l: Vec<String> = encode_all(&recs, ordered)
        .into_iter()
        .map(|x| String::from_utf8(x).unwrap())
        .collect();
    l.join("\n")
}

fn expect_cannot(args: &[&str]) {
    match read_store(&bs(args)) {
        Err(Error::Cannot(_)) => {}
        other => panic!("hydo-read {args:?}: want could-not-read, got {other:?}"),
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn store_mysql() {
    let Some(dsn) = env("HYDO_TEST_MYSQL") else { return };
    hydo_read::mysql::exec_statements(
        &dsn,
        &[
            "DROP DATABASE IF EXISTS t",
            "CREATE DATABASE t",
            "CREATE TABLE t.orders (id INT PRIMARY KEY, amount INT, published TINYINT, note VARCHAR(20))",
            "INSERT INTO t.orders VALUES (2, 100002, 1, NULL), (10, 100010, 1, 'x'), (1, 100001, 0, 'quote\"me'), (3, 999, 2, '')",
        ],
    )
    .unwrap();
    assert_eq!(
        lines(&["mysql", &dsn, "SELECT * FROM t.orders"]),
        r#"{"amount":"100001","id":"1","note":"quote\"me","published":"0"}
{"amount":"100002","id":"2","note":null,"published":"1"}
{"amount":"100010","id":"10","note":"x","published":"1"}
{"amount":"999","id":"3","note":"","published":"2"}"#
    );
    // the URL form of the DSN gives the same rows
    let u = format!(
        "mysql://{}/t",
        dsn.trim_end_matches('/').replacen("@tcp(", "@", 1).replacen(')', "", 1)
    );
    assert_eq!(
        lines(&["mysql", &u, "SELECT id FROM orders WHERE published = 1"]),
        "{\"id\":\"10\"}\n{\"id\":\"2\"}"
    );
    // checks: published ids exactly once in a list; wrong amounts counted
    let d = write_files(&[("acked", "2\n10\n")]);
    let src = format!("mysql=mysql {dsn} \"SELECT id FROM t.orders WHERE published = 1\"");
    let (rc, last) = run(&s(&["exactly-once", "--left", &format!("file {}/acked", d.display()), "--right", &src]));
    assert_eq!(rc, 0, "{last}");
    let (rc, last) = run(&s(&[
        "count",
        "--in",
        &format!("mysql {dsn} \"SELECT id FROM t.orders WHERE amount <> 100000 + id\""),
        "--eq",
        "0",
    ]));
    assert_eq!(last, r#"count violated: mysql has 1 record, want == 0; first: {"id":"3"}"#);
    assert_eq!(rc, 1);
    // errors: a bad query and a dead server are "could not read" (exit 2)
    expect_cannot(&["mysql", &dsn, "SELECT * FROM t.nope"]);
    if let Some(dead) = env("HYDO_TEST_DEADPORT") {
        expect_cannot(&["--timeout", "2", "mysql", &format!("root@tcp({dead})/"), "SELECT 1"]);
        let (rc, _) = run(&s(&[
            "subset",
            "--left",
            "range 0 1",
            "--right",
            &format!("mysql root@tcp({dead})/ \"SELECT 1\" --timeout 2"),
        ]));
        assert_eq!(rc, 2, "dead mysql");
    }
}

#[test]
fn store_postgres() {
    let Some(dsn) = env("HYDO_TEST_POSTGRES") else { return };
    hydo_read::pg::exec_statements(
        &dsn,
        &[
            "DROP TABLE IF EXISTS categories",
            "CREATE TABLE categories (id SERIAL PRIMARY KEY, title TEXT UNIQUE, hidden BOOLEAN, created TIMESTAMPTZ)",
            "INSERT INTO categories (title, hidden, created) VALUES ('All', false, '2026-01-01 00:00:00+00'), ('c0', true, NULL), ('c4', false, NULL)",
        ],
    )
    .unwrap();
    assert_eq!(
        lines(&["postgres", &dsn, "SELECT title, hidden, created FROM categories"]),
        r#"{"created":"2026-01-01T00:00:00Z","hidden":"false","title":"All"}
{"created":null,"hidden":"false","title":"c4"}
{"created":null,"hidden":"true","title":"c0"}"#
    );
    let d = write_files(&[("acked", "0\n4\n7\n"), ("failed", "9\n")]);
    let cats = format!("categories=postgres {dsn} \"SELECT title FROM categories\"");
    let (rc, last) = run(&s(&[
        "exactly-once",
        "--left",
        &format!("acked=file {}/acked --add-prefix c", d.display()),
        "--right",
        &cats,
    ]));
    assert_eq!(last, "exactly-once violated: acked c7 is in categories 0 times (of 3 in acked); missing 1: c7");
    assert_eq!(rc, 1);
    let (rc, last) = run(&s(&[
        "disjoint",
        "--left",
        &format!("failed=file {}/failed --add-prefix c", d.display()),
        "--right",
        &cats,
    ]));
    assert_eq!(rc, 0, "{last}");
    expect_cannot(&["postgres", &dsn, "SELECT * FROM nope"]);
    if let Some(dead) = env("HYDO_TEST_DEADPORT") {
        expect_cannot(&["--timeout", "2", "postgres", &format!("postgres://x@{dead}/x?sslmode=disable"), "SELECT 1"]);
    }
}

#[test]
fn store_kafka() {
    let Some(boot) = env("HYDO_TEST_KAFKA") else { return };
    use hydo_read::kafka::{create_topic, delete_topic, list_topics, produce, ProduceRec};
    let _ = delete_topic(&boot, "orders");
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(create_topic(&boot, "orders", 2).unwrap(), 0);
    std::thread::sleep(Duration::from_secs(2));
    let ts = 1767225600000i64;
    let recs = [
        (Some("0"), "100000", 0),
        (Some("1"), "100001", 1),
        (Some("2"), "100002", 0),
        (Some("2"), "100002", 0),
        (None, "nokey", 1),
    ];
    let pr: Vec<ProduceRec> = recs
        .iter()
        .enumerate()
        .map(|(i, (k, v, p))| ProduceRec {
            partition: *p,
            key: k.map(str::as_bytes),
            value: Some(v.as_bytes()),
            ts: ts + i as i64,
        })
        .collect();
    produce(&boot, "orders", &pr).unwrap();
    assert_eq!(
        lines(&["kafka", &boot, "orders"]),
        r#"{"key":"0","offset":0,"partition":0,"timestamp_ms":1767225600000,"value":"100000"}
{"key":"2","offset":1,"partition":0,"timestamp_ms":1767225600002,"value":"100002"}
{"key":"2","offset":2,"partition":0,"timestamp_ms":1767225600003,"value":"100002"}
{"key":"1","offset":0,"partition":1,"timestamp_ms":1767225600001,"value":"100001"}
{"key":null,"offset":1,"partition":1,"timestamp_ms":1767225600004,"value":"nokey"}"#
    );
    assert_eq!(
        lines(&["kafka", &boot, "orders", "--base64"]).lines().next().unwrap(),
        r#"{"key":"MA==","offset":0,"partition":0,"timestamp_ms":1767225600000,"value":"MTAwMDAw"}"#
    );
    let d = write_files(&[("acked", "0\n1\n2\n")]);
    let (rc, last) = run(&s(&[
        "exactly-once",
        "--left",
        &format!("acked=file {}/acked", d.display()),
        "--right",
        &format!("kafka {boot} orders"),
    ]));
    assert_eq!(last, "exactly-once violated: acked 2 is in kafka 2 times (of 3 in acked); duplicated 1: 2 (x2)");
    assert_eq!(rc, 1);
    // a missing topic is "could not read", and reading it does not create it
    expect_cannot(&["kafka", &boot, "no-such-topic"]);
    std::thread::sleep(Duration::from_secs(1));
    assert!(!list_topics(&boot).unwrap().iter().any(|t| t == "no-such-topic"), "reading a missing topic created it");
    // an empty topic: zero records, not an error
    if create_topic(&boot, "empty", 1).unwrap() == 0 {
        std::thread::sleep(Duration::from_secs(2));
    }
    assert_eq!(lines(&["kafka", &boot, "empty"]), "");
    if let Some(dead) = env("HYDO_TEST_DEADPORT") {
        expect_cannot(&["--timeout", "3", "kafka", &dead, "orders"]);
    }
}

#[test]
fn store_redis() {
    let Some(addr) = env("HYDO_TEST_REDIS") else { return };
    let cmds: Vec<Vec<Vec<u8>>> = [
        vec!["FLUSHALL"],
        vec!["SET", "order:1", "100001"],
        vec!["SET", "order:2", "7"],
        vec!["SET", "other", "x"],
        vec!["HSET", "order:h", "b", "2", "a", "1"],
        vec!["SADD", "order:s", "z", "y"],
        vec!["RPUSH", "order:l", "q", "p"],
        vec!["ZADD", "order:z", "2", "m2", "1", "m1"],
    ]
    .iter()
    .map(|c| bs(c))
    .collect();
    hydo_read::redis::run_commands(addr.as_bytes(), &cmds).unwrap();
    assert_eq!(
        lines(&["redis", &addr, "scan", "order:*"]),
        r#"{"key":"order:1","type":"string","value":"100001"}
{"key":"order:2","type":"string","value":"7"}
{"key":"order:h","type":"hash","value":{"a":"1","b":"2"}}
{"key":"order:l","type":"list","value":["q","p"]}
{"key":"order:s","type":"set","value":["y","z"]}
{"key":"order:z","type":"zset","value":[["m1","1"],["m2","2"]]}"#
    );
    assert_eq!(
        lines(&["redis", &addr, "get", "order:1", "order:9"]),
        "{\"key\":\"order:1\",\"value\":\"100001\"}\n{\"key\":\"order:9\",\"value\":null}"
    );
    assert_eq!(
        lines(&["redis", &addr, "hgetall", "order:h"]),
        "{\"field\":\"a\",\"key\":\"order:h\",\"value\":\"1\"}\n{\"field\":\"b\",\"key\":\"order:h\",\"value\":\"2\"}"
    );
    assert_eq!(
        lines(&["redis", &addr, "lrange", "order:l"]),
        "{\"index\":0,\"key\":\"order:l\",\"value\":\"q\"}\n{\"index\":1,\"key\":\"order:l\",\"value\":\"p\"}"
    );
    assert_eq!(
        lines(&["redis", &addr, "zrange", "order:z"]),
        "{\"key\":\"order:z\",\"member\":\"m1\",\"score\":\"1\"}\n{\"key\":\"order:z\",\"member\":\"m2\",\"score\":\"2\"}"
    );
    assert_eq!(lines(&["redis", &addr, "cmd", "DBSIZE"]), "{\"reply\":7}");
    let d = write_files(&[("truth.jsonl", "{\"id\":\"1\",\"amount\":\"100001\"}\n{\"id\":\"2\",\"amount\":\"100002\"}\n")]);
    let (code, last) = run(&s(&[
        "never-wrong-value",
        "--cache",
        &format!("redis {addr} get order:1 order:2 order:3 --strip-prefix order:"),
        "--truth",
        &format!("mysql=jsonl {}/truth.jsonl --key id --value amount", d.display()),
    ]));
    assert_eq!(last, "never-wrong-value violated: 1 value wrong in redis: 2=7 but mysql has 100002");
    assert_eq!(code, 1);
    if let Some(dead) = env("HYDO_TEST_DEADPORT") {
        expect_cannot(&["redis", &dead, "get", "x"]);
    }
}

#[test]
fn store_etcd() {
    let Some(ep) = env("HYDO_TEST_ETCD") else { return };
    for (k, v) in [("k1", "v1"), ("k2", "v2"), ("k1", "v1b"), ("l1", "x")] {
        hydo_read::etcd::put(&ep, k.as_bytes(), v.as_bytes()).unwrap();
    }
    let got = lines(&["etcd", &ep, "get", "k", "--prefix"]);
    // revisions depend on the fresh server's history: check the shape and the values
    let (recs, _) = read_store(&bs(&["etcd", &ep, "get", "k", "--prefix"])).unwrap();
    assert_eq!(recs.len(), 2, "{got}");
    let f = |r: &Val, k: &str| r.as_obj().unwrap().get(k.as_bytes()).cloned().unwrap();
    assert_eq!(f(&recs[0], "key"), Val::str("k1"), "{got}");
    assert_eq!(f(&recs[0], "value"), Val::str("v1b"), "{got}");
    assert_eq!(f(&recs[0], "version"), Val::Int(2), "{got}");
    assert_eq!(f(&recs[1], "key"), Val::str("k2"), "{got}");
    let (Val::Int(m1), Val::Int(m2)) = (f(&recs[0], "mod_revision"), f(&recs[1], "mod_revision")) else {
        panic!("{got}")
    };
    assert!(m1 > m2, "{got}");
    assert_eq!(
        lines(&["etcd", &ep, "get", "l1", "--serializable"]),
        lines(&["etcd", &ep, "get", "l1"])
    );
    let st = lines(&["etcd", &ep, "status"]);
    assert!(st.contains("\"revision\":") && !st.contains("\"revision\":0"), "status {st}");
    if let Some(dead) = env("HYDO_TEST_DEADPORT") {
        expect_cannot(&["--timeout", "2", "etcd", &format!("http://{dead}"), "get", "k"]);
    }
}
