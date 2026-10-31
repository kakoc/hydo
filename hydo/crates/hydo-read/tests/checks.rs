//! The Go program's check_test.go, ported: the source syntax, the natural order, every check's
//! explanation line and exit status, the HYDO-ASSERT record and the hydo source.

use hydo_read::check::{check_main, parse_source, shell_words, sort_items};
use std::path::{Path, PathBuf};

fn b(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

#[test]
fn shell_words_like_a_shell() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("a b  c", vec!["a", "b", "c"]),
        ("mysql u:@tcp(h:1)/ \"SELECT 1\"", vec!["mysql", "u:@tcp(h:1)/", "SELECT 1"]),
        ("x 'it''s' \"a\\\"b\" c\\ d", vec!["x", "its", "a\"b", "c d"]),
        ("\"\" ''", vec!["", ""]),
        ("name=file /tmp/a --field line ", vec!["name=file", "/tmp/a", "--field", "line"]),
    ];
    for (input, want) in cases {
        let got = shell_words(input.as_bytes()).unwrap();
        let want: Vec<Vec<u8>> = want.into_iter().map(b).collect();
        assert_eq!(got, want, "{input}");
    }
    assert!(shell_words(b"\"open").is_err());
}

#[test]
fn sources() {
    let s = parse_source(b"acked=hydo acked --add-prefix c").unwrap();
    assert_eq!(s.name, b("acked"));
    assert_eq!(s.args, vec![b("hydo"), b("acked")]);
    assert_eq!(s.add, b("c"));
    let s = parse_source(b"redis 127.0.0.1:6379 scan \"order:*\" --strip-prefix order:").unwrap();
    assert_eq!(s.name, b("redis"));
    assert_eq!(s.strip, b("order:"));
    assert_eq!(s.args.len(), 4);
    // a DSN with '=' in it is not a name
    let s = parse_source(b"postgres \"host=x user=y\" \"SELECT 1\"").unwrap();
    assert_eq!(s.name, b("postgres"));
}

#[test]
fn natural_item_order() {
    let mut x: Vec<Vec<u8>> = "10 9 b a 100 -1 c10 c4 c04 order:2 c1x c1"
        .split(' ')
        .map(b)
        .collect();
    sort_items(&mut x);
    let got: Vec<String> = x.iter().map(|v| String::from_utf8_lossy(v).into_owned()).collect();
    assert_eq!(got.join(" "), "-1 9 10 100 a b c1 c1x c04 c4 c10 order:2");
}

/// A temp dir with the given files.
pub fn write_files(files: &[(&str, &str)]) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "hydo-read-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for (n, c) in files {
        std::fs::write(d.join(n), c).unwrap();
    }
    d
}

/// A check in-process: its exit status and its last output line.
pub fn run(args: &[String]) -> (i32, String) {
    let args: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let rc = check_main(&args, &mut out, &mut err);
    let s = String::from_utf8_lossy(&out).into_owned();
    let last = s.trim().lines().last().unwrap_or("").to_string();
    (rc, last)
}

fn p(d: &Path, n: &str) -> String {
    d.join(n).to_string_lossy().into_owned()
}

#[test]
fn checks() {
    let d = write_files(&[
        ("acked", "0\n1\n2\n3\n"),
        ("failed", "5\n7\n"),
        (
            "kafka.jsonl",
            r#"{"key":"0","offset":0,"partition":0,"value":"100000"}
{"key":"1","offset":1,"partition":0,"value":"100001"}
{"key":"2","offset":2,"partition":0,"value":"100002"}
{"key":"3","offset":3,"partition":0,"value":"100003"}
{"key":"5","offset":4,"partition":0,"value":"100005"}
{"key":"3","offset":5,"partition":0,"value":"100003"}
"#,
        ),
        (
            "mysql.jsonl",
            r#"{"amount":"100000","id":"0"}
{"amount":"100001","id":"1"}
{"amount":"100002","id":"2"}
{"amount":"100003","id":"3"}
{"amount":"999","id":"5"}
"#,
        ),
        (
            "redis.jsonl",
            r#"{"key":"order:1","type":"string","value":"100001"}
{"key":"order:2","type":"string","value":"7"}
{"key":"order:9","type":"string","value":"100009"}
"#,
        ),
    ]);
    let f = |n: &str| p(&d, n);
    let kafka = format!("kafka=jsonl {}", f("kafka.jsonl"));
    let acked = format!("acked=file {}", f("acked"));
    let nope = f("nope");
    let cases: Vec<(Vec<String>, i32, String)> = vec![
        (vec!["subset".into(), "--left".into(), acked.clone(), "--right".into(), kafka.clone()], 0, "subset holds: all 4 items of acked are in kafka".into()),
        (vec!["subset".into(), "--left".into(), "range 0 6".into(), "--right".into(), kafka.clone()], 1, "subset violated: 1 item of 6 in range not in kafka: 4".into()),
        (vec!["exactly-once".into(), "--left".into(), acked.clone(), "--right".into(), kafka.clone()], 1, "exactly-once violated: acked 3 is in kafka 2 times (of 4 in acked); duplicated 1: 3 (x2)".into()),
        (vec!["exactly-once".into(), "--left".into(), format!("acked=file {} --field line", f("acked")), "--right".into(), format!("jsonl {} --field id", f("mysql.jsonl"))], 0, "exactly-once holds: each of the 4 items of acked occurs once in jsonl (jsonl has 5 records)".into()),
        (vec!["disjoint".into(), "--left".into(), format!("told_failed=file {}", f("failed")), "--right".into(), kafka.clone()], 1, "disjoint violated: 1 item of told_failed is also in kafka: 5".into()),
        (vec!["no-duplicates".into(), "--in".into(), kafka.clone()], 1, "no-duplicates violated: 3 occurs 2 times in kafka; 1 item repeated: 3 (x2)".into()),
        (vec!["count".into(), "--in".into(), kafka.clone(), "--eq".into(), "6".into()], 0, "count holds: kafka has 6 records (want == 6)".into()),
        (vec!["count".into(), "--in".into(), kafka.clone(), "--max".into(), "2".into()], 1, r#"count violated: kafka has 6 records, want <= 2; first: {"key":"0","offset":0,"partition":0,"value":"100000"}"#.into()),
        (vec!["equal".into(), "--left".into(), acked.clone(), "--right".into(), "range 0 4".into()], 0, "equal holds: acked and range hold the same 4 items".into()),
        (vec!["equal".into(), "--left".into(), acked.clone(), "--right".into(), "range 0 5".into()], 1, "equal violated: 1 item in range not in acked: 4".into()),
        (vec!["never-wrong-value".into(), "--cache".into(), format!("redis=jsonl {} --strip-prefix order:", f("redis.jsonl")), "--truth".into(), format!("mysql=jsonl {} --key id --value amount", f("mysql.jsonl"))], 1,
            "never-wrong-value violated: 2 values wrong in redis: 2=7 but mysql has 100002, 9=100009 but mysql has no such key".into()),
        (vec!["never-wrong-value".into(), "--allow-orphans".into(), "--cache".into(), format!("kafka=jsonl {}", f("kafka.jsonl")), "--truth".into(), format!("mysql=jsonl {} --key id --value amount", f("mysql.jsonl"))], 1,
            "never-wrong-value violated: 1 value wrong in kafka: 5=100005 but mysql has 999".into()),
        (vec!["subset".into(), "--left".into(), format!("file {nope}"), "--right".into(), kafka.clone()], 2, format!("subset could not be evaluated: could not read file (file): open {nope}: no such file or directory")),
        (vec!["subset".into(), "--unreadable".into(), "violation".into(), "--left".into(), format!("file {nope}"), "--right".into(), kafka.clone()], 1, format!("subset violated: could not read file (file): open {nope}: no such file or directory")),
        (vec!["subset".into(), "--oracle".into(), "--left".into(), "range 0 6".into(), "--right".into(), kafka.clone()], 1, "HYDO-ORACLE: VIOLATION subset violated: 1 item of 6 in range not in kafka: 4".into()),
        (vec!["subset".into(), "--left".into(), "range 0 2".into()], 2, "".into()),
        (vec!["exactly-once".into(), "--left".into(), "range 0 12 --add-prefix c".into(), "--right".into(), "range 0 4 --add-prefix c".into()], 1, "exactly-once violated: range c4 is in range 0 times (of 12 in range); missing 8: c4, c5, c6, c7, c8 (+3 more)".into()),
        (vec!["subset".into(), "--left".into(), format!("acked=file {} --item 'k{{line}}={{line}}'", f("acked")), "--right".into(), format!("jsonl {} --item 'k{{id}}={{id}}'", f("mysql.jsonl"))], 0, "subset holds: all 4 items of acked are in jsonl (jsonl has 5 items)".into()),
        (vec!["subset".into(), "--left".into(), format!("acked=file {} --item '{{line}}=10000{{line}}'", f("acked")), "--right".into(), format!("jsonl {} --item '{{id}}={{amount}}'", f("mysql.jsonl"))], 0, "subset holds".into()),
        (vec!["subset".into(), "--left".into(), "range 0 6 --item '{n}=10000{n}'".into(), "--right".into(), format!("kafka=jsonl {} --item '{{key}}={{value}}'", f("kafka.jsonl"))], 1, "subset violated: 1 item of 6 in range not in kafka: 4=100004".into()),
        (vec!["subset".into(), "--left".into(), "range 0 1 --item '{nope}'".into(), "--right".into(), "range 0 1".into()], 2, "".into()),
        (vec!["frobnicate".into(), "--left".into(), acked.clone(), "--right".into(), kafka.clone()], 2, "frobnicate could not be evaluated: usage: unknown check \"frobnicate\"".into()),
    ];
    for (args, rc, want) in cases {
        let (got_rc, last) = run(&args);
        assert!(
            got_rc == rc && (want.is_empty() || last.starts_with(&want)),
            "{args:?}:\n got rc={got_rc} {last:?}\nwant rc={rc} {want:?}"
        );
    }
}

#[test]
fn assert_record() {
    let d = write_files(&[("a", "1\n2\n")]);
    let args: Vec<Vec<u8>> = vec![
        b("subset"),
        b("--id"),
        b("x.y"),
        b("--left"),
        format!("file {}", p(&d, "a")).into_bytes(),
        b("--right"),
        b("range 0 2"),
    ];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let rc = check_main(&args, &mut out, &mut err);
    let want = r#"HYDO-ASSERT-DECL {"id":"x.y","msg":"subset","t":"always"}
HYDO-ASSERT {"details":{"check":"subset","explanation":"subset violated: 1 item of 2 in file not in range: 2","first":"2"},"id":"x.y","msg":"subset","ok":false,"t":"always"}
subset violated: 1 item of 2 in file not in range: 2
"#;
    assert_eq!(rc, 1);
    assert_eq!(String::from_utf8_lossy(&out), want);
}

#[test]
fn hydo_source() {
    let d = write_files(&[
        ("acked", "1\n3\n"),
        ("failed", "2\n3\n"),
        ("outcomes", "1 ack\n2 failed\n3 failed\n3 ack\n4 unknown\n5 unknown\n5 ack\n"),
    ]);
    std::env::set_var("HYDO_ACKED", d.join("acked"));
    std::env::set_var("HYDO_FAILED", d.join("failed"));
    std::env::set_var("HYDO_OUTCOMES", d.join("outcomes"));
    for (what, want) in [("acked", "1 3"), ("failed", "2 3"), ("told-failed", "2"), ("unknown", "4")] {
        let (recs, _) = hydo_read::read_store(&[b("hydo"), b(what)]).unwrap();
        let ids: Vec<String> = recs
            .iter()
            .map(|r| match r.as_obj().and_then(|o| o.get(&b"id"[..])) {
                Some(hydo_read::json::Val::Str(s)) => String::from_utf8_lossy(s).into_owned(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(ids.join(" "), want, "hydo {what}");
    }
}
