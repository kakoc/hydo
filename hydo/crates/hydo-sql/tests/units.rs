//! The configuration, the SQL each dialect gets, and the outcome rules against scripted servers
//! (sqld's HTTP API and PostgreSQL's wire protocol, answering canned messages), so the rules
//! that decide ok / failed / unknown are tested without a database.

use hydo_sql::conf::{Conf, Dialect, Role};
use hydo_sql::db::{self, Fail};
use hydo_sql::work;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

const CONF: &str = "# a primary and a replica\n\
dialect libsql\n\
endpoint primary 127.0.0.1:1 primary\n\
endpoint replica 127.0.0.1:2 replica\n\
workloads bank list-append register set counter\n\
route register primary\n\
accounts 8\nbalance 100\nkeys 6\ntimeout_ms 2000\nretries 2\n";

#[test]
fn conf_round_trip() {
    let c = Conf::parse(CONF).unwrap();
    assert_eq!(c.dialect, Dialect::Libsql);
    assert_eq!(c.endpoints.len(), 2);
    assert_eq!(c.endpoints[1].role, Role::Replica);
    assert!(c.reads_replica("list-append"));
    assert!(!c.reads_replica("register"));
    assert_eq!(c.route("bank").len(), 2);
    assert_eq!(c.primary().name, "primary");
    assert_eq!(Conf::parse(&c.render()).unwrap(), c);
}

#[test]
fn conf_refusals() {
    for (text, want) in [
        ("endpoint a x\n", "no dialect line"),
        ("dialect oracle\n", "dialect postgres|mysql|libsql"),
        (
            "dialect mysql\nendpoint a x replica\n",
            "no primary endpoint",
        ),
        (
            "dialect mysql\nendpoint a x\nworkloads bank queue\n",
            "unknown workload queue",
        ),
        (
            "dialect mysql\nendpoint a x\nroute bank b\n",
            "no endpoint named b",
        ),
        (
            "dialect mysql\nendpoint a x\nendpoint a y\n",
            "endpoint named twice",
        ),
        ("dialect mysql\nendpoint a x\nspeed 3\n", "unknown setting"),
    ] {
        let e = Conf::parse(text).unwrap_err();
        assert!(e.contains(want), "{text:?}: {e}");
    }
}

fn conf_of(d: &str, addr: &str) -> Conf {
    Conf::parse(&format!("dialect {d}\nendpoint p {addr}\nworkloads bank list-append register set counter\ntimeout_ms 2000\nretries 1\n")).unwrap()
}

#[test]
fn sql_per_dialect() {
    let pg = conf_of("postgres", "postgres://u@h/db");
    let my = conf_of("mysql", "u@tcp(h:3306)/db");
    let li = conf_of("libsql", "h:8080");
    assert_eq!(db::begin(&pg), vec!["BEGIN ISOLATION LEVEL SERIALIZABLE"]);
    assert_eq!(
        db::begin(&my),
        vec![
            "SET SESSION TRANSACTION ISOLATION LEVEL SERIALIZABLE",
            "START TRANSACTION"
        ]
    );
    assert_eq!(db::begin(&li), vec!["BEGIN"]);
    assert!(work::transfer_sql(&pg, 1, 2, 5)[0].starts_with(
        "WITH d AS (UPDATE hydo_accounts SET bal = bal - 5 WHERE id = 1 AND bal >= 5 RETURNING id)"
    ));
    assert!(work::transfer_sql(&my, 1, 2, 5)[0]
        .contains("JOIN hydo_accounts b ON a.id = 1 AND b.id = 2"));
    assert_eq!(work::transfer_sql(&li, 1, 2, 5).len(), 2);
    assert!(work::append_sql(&my, 3, 41)
        .contains("ON DUPLICATE KEY UPDATE v = CONCAT(v, ',', VALUES(v))"));
    assert!(work::append_sql(&pg, 3, 41)
        .contains("ON CONFLICT (k) DO UPDATE SET v = hydo_lists.v || ',' || excluded.v"));
    let s = work::schema(&my);
    assert!(s
        .iter()
        .any(|x| x == "INSERT IGNORE INTO hydo_accounts (id, bal) VALUES (7, 100)"));
    assert!(work::schema(&pg)
        .iter()
        .any(|x| x == "INSERT INTO hydo_counter (k, n) VALUES (0, 0) ON CONFLICT DO NOTHING"));
    assert_eq!(db::text(&my, "a'b\\c"), "'a''b\\\\c'");
    assert_eq!(
        work::sql_of_file("-- hydo: store=p\nSELECT 1;\n\n"),
        "-- hydo: store=p\nSELECT 1"
    );
}

#[test]
fn outcome_rules() {
    assert!(db::pg_retry("40001") && db::pg_retry("40P01") && !db::pg_retry("23505"));
    assert!(
        db::pg_unknown_at_commit("08006")
            && db::pg_unknown_at_commit("57P01")
            && !db::pg_unknown_at_commit("40001")
    );
    assert!(db::mysql_retry(1213) && db::mysql_retry(1205) && !db::mysql_retry(1062));
    assert!(db::sqlite_retry("SQLite error: database is locked"));
    assert_eq!(
        Fail::Unknown("x".into()).before_commit(),
        Fail::Rejected {
            msg: "x".into(),
            retry: false
        }
    );
}

/// A server that answers each connection with the next canned reply (after reading the request).
fn http_server(replies: Vec<&'static str>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap().to_string();
    thread::spawn(move || {
        for r in replies {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let _ = s.read(&mut buf);
            if r == "CLOSE" {
                continue;
            }
            let _ = write!(s, "HTTP/1.1 {}", r);
        }
    });
    a
}

fn ok_json(body: &str) -> &'static str {
    Box::leak(format!("200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str())
}

#[test]
fn hrana_batch_outcomes() {
    // committed: BEGIN, one statement, COMMIT, ROLLBACK skipped
    let committed = ok_json(
        r#"{"baton":null,"results":[{"type":"ok","response":{"type":"batch","result":{"step_results":[{"cols":[],"rows":[],"affected_row_count":0},{"cols":[],"rows":[],"affected_row_count":1},{"cols":[],"rows":[],"affected_row_count":0},null],"step_errors":[null,null,null,null]}}},{"type":"ok","response":{"type":"close"}}]}"#,
    );
    // the statement failed with SQLITE_BUSY: COMMIT skipped, ROLLBACK ran
    let busy = ok_json(
        r#"{"baton":null,"results":[{"type":"ok","response":{"type":"batch","result":{"step_results":[{"cols":[],"rows":[],"affected_row_count":0},null,null,{"cols":[],"rows":[],"affected_row_count":0}],"step_errors":[null,{"message":"SQLite error: database is locked","code":"SQLITE_BUSY"},null,null]}}},{"type":"ok","response":{"type":"close"}}]}"#,
    );
    let a = http_server(vec![
        committed,
        busy,
        "502 Bad Gateway\r\nContent-Length: 4\r\n\r\ndown",
        "CLOSE",
    ]);
    let c = conf_of("libsql", &a);
    let ep = c.primary().clone();
    let st = vec!["UPDATE t SET x = 1".to_string()];
    let r = db::batch(&c, &ep, &st).unwrap();
    assert_eq!(r[0].affected, 1);
    assert!(matches!(
        db::batch(&c, &ep, &st),
        Err(Fail::Rejected { retry: true, .. })
    ));
    assert!(
        matches!(db::batch(&c, &ep, &st), Err(Fail::Unknown(_))),
        "a 5xx leaves the outcome open"
    );
    assert!(
        matches!(db::batch(&c, &ep, &st), Err(Fail::Unknown(_))),
        "no answer leaves the outcome open"
    );
    // nothing listens: never sent
    let c2 = conf_of("libsql", "127.0.0.1:1");
    assert!(matches!(
        db::batch(&c2, c2.primary(), &st),
        Err(Fail::NotSent(_))
    ));
}

// ------------------------------------------------------------------ a scripted PostgreSQL

fn msg(t: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![t];
    m.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
    m.extend_from_slice(body);
    m
}
fn cstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    v
}
fn complete(tag: &str) -> Vec<u8> {
    msg(b'C', &cstr(tag))
}
fn error(code: &str, m: &str) -> Vec<u8> {
    let mut b = vec![b'S'];
    b.extend(cstr("ERROR"));
    b.push(b'C');
    b.extend(cstr(code));
    b.push(b'M');
    b.extend(cstr(m));
    b.push(0);
    msg(b'E', &b)
}
fn ready() -> Vec<u8> {
    msg(b'Z', b"I")
}

/// Each connection: the startup (trust), then for each query the next canned answer (None:
/// close the connection without answering).
fn pg_server(per_conn: Vec<Vec<Option<Vec<u8>>>>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let a = l.local_addr().unwrap();
    thread::spawn(move || {
        for answers in per_conn {
            let (mut s, _) = l.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let mut rest = vec![0u8; u32::from_be_bytes(len) as usize - 4];
            s.read_exact(&mut rest).unwrap();
            let mut hello = msg(b'R', &0u32.to_be_bytes());
            hello.extend(ready());
            s.write_all(&hello).unwrap();
            for ans in answers {
                let mut t = [0u8; 1];
                if s.read_exact(&mut t).is_err() {
                    break;
                }
                s.read_exact(&mut len).unwrap();
                let mut q = vec![0u8; u32::from_be_bytes(len) as usize - 4];
                s.read_exact(&mut q).unwrap();
                match ans {
                    Some(b) => s.write_all(&b).unwrap(),
                    None => break,
                }
            }
        }
    });
    format!("postgres://u@{}:{}/db?sslmode=disable", a.ip(), a.port())
}

#[test]
fn pg_batch_outcomes() {
    let st = vec!["UPDATE t SET x = 1".to_string()];
    let ok = [
        complete("BEGIN"),
        complete("UPDATE 1"),
        complete("COMMIT"),
        ready(),
    ]
    .concat();
    let serial = [
        complete("BEGIN"),
        error("40001", "could not serialize access"),
        ready(),
    ]
    .concat();
    let at_commit = [
        complete("BEGIN"),
        complete("UPDATE 1"),
        error("57P01", "terminating connection"),
        ready(),
    ]
    .concat();
    let dsn = pg_server(vec![
        vec![Some(ok)],
        vec![Some(serial)],
        vec![Some(at_commit)],
        vec![None],
    ]);
    let c = conf_of("postgres", &dsn);
    let ep = c.primary().clone();
    assert_eq!(db::batch(&c, &ep, &st).unwrap()[0].affected, 1);
    assert!(matches!(
        db::batch(&c, &ep, &st),
        Err(Fail::Rejected { retry: true, .. })
    ));
    assert!(
        matches!(db::batch(&c, &ep, &st), Err(Fail::Unknown(_))),
        "an operator error at COMMIT"
    );
    assert!(
        matches!(db::batch(&c, &ep, &st), Err(Fail::Unknown(_))),
        "the connection closed after the request"
    );
}

#[test]
fn pg_interactive_outcomes() {
    let row = {
        let mut t = 1u16.to_be_bytes().to_vec();
        t.extend(cstr("bal"));
        t.extend([0u8; 4]); // table oid
        t.extend([0u8; 2]); // column
        t.extend(20u32.to_be_bytes()); // int8
        t.extend(8i16.to_be_bytes());
        t.extend((-1i32).to_be_bytes());
        t.extend(0i16.to_be_bytes());
        let mut d = 1u16.to_be_bytes().to_vec();
        d.extend(3i32.to_be_bytes());
        d.extend(b"100");
        [msg(b'T', &t), msg(b'D', &d), complete("SELECT 1"), ready()].concat()
    };
    let begin = [complete("BEGIN"), ready()].concat();
    let commit = [complete("COMMIT"), ready()].concat();
    let rollback_tag = [complete("ROLLBACK"), ready()].concat();
    let rb = [complete("ROLLBACK"), ready()].concat();
    let dsn = pg_server(vec![
        vec![Some(begin.clone()), Some(row.clone()), Some(commit)],
        vec![Some(begin.clone()), Some(row.clone()), Some(rollback_tag)],
        vec![Some(begin.clone()), None],
        vec![Some(begin), Some(row), None],
    ]);
    let c = conf_of("postgres", &dsn);
    let ep = c.primary().clone();
    let read = |s: &mut db::Session| {
        s.exec("SELECT bal FROM hydo_accounts WHERE id = 0")
            .map(|r| r.int())
    };
    assert_eq!(db::interactive(&c, &ep, read).unwrap(), Some(100));
    assert!(
        matches!(db::interactive(&c, &ep, read), Err(Fail::Rejected { .. })),
        "COMMIT answered ROLLBACK"
    );
    let _ = rb;
    assert!(
        matches!(db::interactive(&c, &ep, read), Err(Fail::Rejected { .. })),
        "lost before COMMIT: definite"
    );
    assert!(
        matches!(db::interactive(&c, &ep, read), Err(Fail::Unknown(_))),
        "no answer to COMMIT: unknown"
    );
}

#[test]
fn world_and_check_sources() {
    let dir = std::env::temp_dir().join(format!("hydo-sql-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("world.log");
    std::fs::write(&log, "seq=1 t=1 kind=req ep=stripe route=refund ref=pi_1 fx=1 out=ok\nseq=2 t=2 kind=req ep=stripe route=refund ref=pi_2 fx=0 out=replay\n").unwrap();
    let books = dir.join("books.txt");
    std::fs::write(&books, "pi_1\npi_2\n").unwrap();
    let args = |v: &[&str]| v.iter().map(|s| s.as_bytes().to_vec()).collect::<Vec<_>>();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let rc = hydo_sql::main_with(
        &args(&[
            "read",
            "world",
            "stripe.refund",
            "--log",
            log.to_str().unwrap(),
        ]),
        &mut out,
        &mut err,
    );
    assert_eq!(rc, 0, "{}", String::from_utf8_lossy(&err));
    assert!(String::from_utf8_lossy(&out).contains("\"ref\":\"pi_1\""));
    // hydo-check over a world source: the books list a refund the provider never made
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let left = format!("books=file {}", books.display());
    let right = format!(
        "stripe=world stripe.refund --log {} --field ref",
        log.display()
    );
    let rc = hydo_sql::main_with(
        &args(&["check", "subset", "--left", &left, "--right", &right]),
        &mut out,
        &mut err,
    );
    assert_eq!(rc, 1, "{}", String::from_utf8_lossy(&out));
    let o = String::from_utf8_lossy(&out);
    assert!(o.contains("pi_2"), "{o}");
    std::fs::remove_dir_all(&dir).unwrap();
}
