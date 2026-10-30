//! The unit tests of hydo/history/test_hydohist.py, on the Rust checkers. (tests/equiv.py proves
//! the answers equal the Python ones byte for byte; these tests keep the behaviour pinned without
//! Python.)

use hydo_history::py::{json_loads, BigInt, PyVal};
use hydo_history::{parse_sources, pdec, penc, run_check, run_checks, CheckResult, Outcome};

fn hist_named(lines: &[&str], name: &str) -> String {
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        let mut it = l.splitn(3, ' ');
        let (p, ty, rest) = (
            it.next().unwrap(),
            it.next().unwrap(),
            it.next().unwrap_or(""),
        );
        out.push_str(&format!(
            "HYDO-HIST {name} {p} {ty} {} {rest}\n",
            (i + 1) * 1_000_000
        ));
    }
    out
}

fn hist(lines: &[&str]) -> String {
    hist_named(lines, "kv")
}

fn spec(kind: &str, extra: &str) -> PyVal {
    json_loads(&format!(
        "{{\"id\":\"t\",\"type\":\"{kind}\",\"history\":\"kv\"{extra}}}"
    ))
    .unwrap()
}

fn check(kind: &str, lines: &[&str], extra: &str, budget: i64) -> CheckResult {
    let (ev, _, _) = parse_sources(&[("w", hist(lines))]).unwrap();
    run_check(&ev, &spec(kind, extra), budget).unwrap()
}

fn lin(lines: &[&str], extra: &str) -> CheckResult {
    check("linearizable", lines, extra, hydo_history::DEFAULT_BUDGET)
}

fn la(lines: &[&str], extra: &str) -> CheckResult {
    check("list-append", lines, extra, hydo_history::DEFAULT_BUDGET)
}

fn det<'a>(r: &'a CheckResult, k: &str) -> Option<&'a PyVal> {
    r.details.iter().find(|(x, _)| x == k).map(|(_, v)| v)
}

fn det_int(r: &CheckResult, k: &str) -> i64 {
    match det(r, k) {
        Some(PyVal::Int(b)) => b.saturating_i64(),
        other => panic!("{k}: {other:?}"),
    }
}

fn det_str<'a>(r: &'a CheckResult, k: &str) -> &'a str {
    det(r, k).and_then(PyVal::as_str).unwrap_or("")
}

const OK: Outcome = Outcome::Ok;
const BAD: Outcome = Outcome::Violation;

#[test]
fn register() {
    let r = lin(
        &[
            "0 invoke write x 1",
            "0 ok write x 1",
            "1 invoke read x",
            "1 ok read x 1",
        ],
        "",
    );
    assert_eq!(r.status, OK);
    assert_eq!(det_int(&r, "ops"), 2);
    assert_eq!(
        lin(
            &[
                "0 invoke write x 1",
                "1 invoke read x",
                "0 ok write x 1",
                "1 ok read x nil"
            ],
            ""
        )
        .status,
        OK
    );
    let r = lin(
        &[
            "0 invoke write x 1",
            "0 ok write x 1",
            "1 invoke read x",
            "1 ok read x nil",
        ],
        "",
    );
    assert_eq!(r.status, BAD);
    assert!(r.summary.contains("acknowledged write is lost"));
    assert_eq!(det_int(&r, "witness_ops"), 2);
    assert_eq!(
        r.summary,
        "key x: an acknowledged write is lost or a read is stale: a read that began after the write of 1 completed returned nil: p0 write x=1 (ok, t=0.00-0.00s); p1 read x -> nil (ok, t=0.00-0.00s)"
    );
    let two = [
        "0 invoke write x 1",
        "0 ok write x 1",
        "0 invoke write x 2",
        "0 ok write x 2",
        "1 invoke read x",
        "1 ok read x 1",
    ];
    assert_eq!(lin(&two, "").status, BAD);
    let later = [
        "0 invoke write x 1",
        "1 invoke read x",
        "1 ok read x nil",
        "1 invoke read x",
        "1 ok read x 1",
    ];
    assert_eq!(lin(&later, "").status, OK);
    let never = [
        "0 invoke write x 1",
        "0 info write x 1",
        "1 invoke read x",
        "1 ok read x nil",
    ];
    assert_eq!(lin(&never, "").status, OK);
    let undo = [
        "0 invoke write x 1",
        "1 invoke read x",
        "1 ok read x 1",
        "1 invoke read x",
        "1 ok read x nil",
    ];
    assert_eq!(lin(&undo, "").status, BAD);
    let failed = [
        "0 invoke write x 1",
        "0 fail write x 1",
        "1 invoke read x",
        "1 ok read x 1",
    ];
    assert_eq!(lin(&failed, "").status, BAD);
}

#[test]
fn cas_and_versions() {
    let base = [
        "0 invoke write x 0",
        "0 ok write x 0",
        "0 invoke cas x 0 1",
        "0 ok cas x 0 1",
    ];
    let mut ok = base.to_vec();
    ok.extend(["1 invoke read x", "1 ok read x 1"]);
    assert_eq!(lin(&ok, ",\"model\":\"cas\"").status, OK);
    let mut bad = base.to_vec();
    bad.extend(["1 invoke cas x 0 2", "1 ok cas x 0 2"]);
    assert_eq!(lin(&bad, ",\"model\":\"cas\"").status, BAD);
    assert_eq!(
        lin(&["0 invoke cas x 0 1", "0 ok cas x 0 1"], "").status,
        Outcome::Error
    );
    let unknown = [
        "0 invoke write x 0",
        "0 ok write x 0",
        "1 invoke cas x 5 1",
        "2 invoke read x",
        "2 ok read x 0",
    ];
    assert_eq!(lin(&unknown, ",\"model\":\"cas\"").status, OK);
    let v = |r: &str| {
        lin(
            &[
                "0 invoke write x a",
                "0 ok write x a@5",
                "1 invoke read x",
                &format!("1 ok read x {r}"),
            ],
            "",
        )
    };
    assert_eq!(v("a@5").status, OK);
    let bad = v("a@4");
    assert_eq!(bad.status, BAD);
    assert!(bad.summary.contains("version 5"));
    let dec = [
        "0 invoke write x a",
        "0 ok write x a@5",
        "0 invoke write x b",
        "0 ok write x b@3",
    ];
    assert_eq!(lin(&dec, "").status, BAD);
    let learned = [
        "0 invoke write x a",
        "1 invoke read x",
        "1 ok read x a@7",
        "1 invoke read x",
        "1 ok read x a@8",
    ];
    assert_eq!(lin(&learned, "").status, BAD);
}

#[test]
fn keys_witness_initial() {
    let r = lin(
        &[
            "0 invoke write x 1",
            "0 ok write x 1",
            "0 invoke write y 2",
            "0 ok write y 2",
            "1 invoke read *",
            "1 ok read x 1 y nil",
        ],
        "",
    );
    assert_eq!(r.status, BAD);
    assert_eq!(det_str(&r, "key"), "y");
    assert_eq!(det_int(&r, "keys"), 2);
    let r = lin(
        &[
            "0 invoke write x 1",
            "0 ok write x 1",
            "1 invoke read x",
            "1 ok read x 1",
            "0 invoke write x 2",
            "0 ok write x 2",
            "2 invoke read x",
            "2 ok read x 2",
            "1 invoke read x",
            "1 ok read x 1",
        ],
        "",
    );
    assert_eq!(det_int(&r, "witness_ops"), 3, "{}", r.explanation);
    assert!(r.summary.contains("(an older value)"));
    let reads = [
        "1 invoke read x",
        "1 ok read x 9",
        "1 invoke read x",
        "1 ok read x 9",
    ];
    assert_eq!(lin(&reads, ",\"initial\":\"any\"").status, OK);
    assert_eq!(lin(&reads[..2], "").status, BAD);
    assert_eq!(
        lin(
            &["0 invoke write x 1", "0 invoke read x", "0 ok read x 1"],
            ""
        )
        .status,
        OK
    );
}

#[test]
fn undecided_and_search() {
    let mut lines: Vec<String> = (0..12).map(|p| format!("{p} invoke write x {p}")).collect();
    lines.extend((0..12).map(|p| format!("{p} ok write x {p}")));
    lines.extend([
        "20 invoke read x".to_string(),
        "20 ok read x 99".to_string(),
    ]);
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(lin(&refs, "").status, BAD);
    assert_eq!(
        check("linearizable", &refs, "", 5).status,
        Outcome::Undecided
    );
    let mut lines: Vec<String> = (0..8).map(|p| format!("{p} invoke write x {p}")).collect();
    lines.extend((0..8).map(|p| format!("{p} ok write x {p}")));
    lines.extend(
        [
            "9 invoke read x",
            "9 ok read x 5",
            "9 invoke read x",
            "9 ok read x 5",
        ]
        .map(String::from),
    );
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    assert_eq!(lin(&refs, "").status, OK);
}

#[test]
fn recovery_console() {
    let w = hist(&["0 invoke write x 1", "0 ok write x 1", "0 invoke write y 1"]);
    let rec =
        "HYDO-HIST kv 20 invoke 5000000 read x\nHYDO-HIST kv 20 ok 6000000 read x nil y nil\n";
    let (ev, _, _) = parse_sources(&[("w", w.as_str()), ("w.recovery", rec)]).unwrap();
    let r = run_check(&ev, &spec("linearizable", ""), hydo_history::DEFAULT_BUDGET).unwrap();
    assert_eq!(r.status, BAD);
    assert_eq!(det_str(&r, "key"), "x");
    assert!(r.explanation.contains("recovery boot"));
}

#[test]
fn list_append() {
    let ser = ",\"consistency\":\"serializable\"";
    let r = la(
        &[
            "0 invoke append x 1",
            "0 ok append x 1",
            "1 invoke append x 2",
            "1 ok append x 2",
            "2 invoke read x",
            "2 ok read x [1,2]",
        ],
        "",
    );
    assert_eq!(r.status, OK);
    let r = la(
        &[
            "0 invoke append x 1",
            "0 ok append x 1",
            "1 invoke append x 2",
            "1 ok append x 2",
            "2 invoke read x",
            "2 ok read x [2]",
        ],
        "",
    );
    assert_eq!(det_str(&r, "anomaly"), "lost-append");
    let r = la(
        &[
            "0 invoke append x 1",
            "0 ok append x 1",
            "2 invoke read x",
            "2 ok read x []",
        ],
        ser,
    );
    assert_eq!(r.status, OK);
    let r = la(
        &[
            "0 invoke append x 1",
            "0 ok append x 1",
            "2 invoke read x",
            "2 ok read x [1,1]",
        ],
        "",
    );
    assert!(det_str(&r, "anomalies").contains("duplicate-elements"));
    let r = la(
        &[
            "0 invoke append x 1",
            "1 invoke append x 2",
            "0 ok append x 1",
            "1 ok append x 2",
            "2 invoke read x",
            "2 ok read x [1,2]",
            "3 invoke read x",
            "3 ok read x [2]",
        ],
        ser,
    );
    assert_eq!(det_str(&r, "anomaly"), "incompatible-order");
    let r = la(
        &[
            "0 invoke append x 3",
            "0 fail append x 3",
            "2 invoke read x",
            "2 ok read x [3,9]",
        ],
        "",
    );
    assert!(det_str(&r, "anomalies").contains("aborted-read"));
    assert!(det_str(&r, "anomalies").contains("garbage-read"));
}

#[test]
fn cycles() {
    let ser = ",\"consistency\":\"serializable\"";
    let r = la(
        &[
            "0 invoke txn a:x:1 a:y:1",
            "1 invoke txn a:x:2 a:y:2",
            "0 ok txn a:x:1 a:y:1",
            "1 ok txn a:x:2 a:y:2",
            "2 invoke txn r:x r:y",
            "2 ok txn r:x:[1,2] r:y:[2,1]",
        ],
        ser,
    );
    assert_eq!(det_str(&r, "anomaly"), "G0", "{}", r.explanation);
    let r = la(
        &[
            "0 invoke txn a:x:1 r:y",
            "1 invoke txn a:y:1 r:x",
            "0 ok txn a:x:1 r:y:[1]",
            "1 ok txn a:y:1 r:x:[1]",
        ],
        ser,
    );
    assert_eq!(det_str(&r, "anomaly"), "G1c", "{}", r.explanation);
    let skew = [
        "0 invoke txn r:x r:y",
        "1 invoke txn a:x:1 a:y:1",
        "1 ok txn a:x:1 a:y:1",
        "0 ok txn r:x:[] r:y:[1]",
        "2 invoke txn r:x r:y",
        "2 ok txn r:x:[1] r:y:[1]",
    ];
    for lv in ["serializable", "snapshot-isolation"] {
        let r = la(&skew, &format!(",\"consistency\":\"{lv}\""));
        assert_eq!(det_str(&r, "anomaly"), "G-single", "{lv}");
    }
    let ws = [
        "0 invoke txn r:x a:y:1",
        "1 invoke txn r:y a:x:1",
        "0 ok txn r:x:[] a:y:1",
        "1 ok txn r:y:[] a:x:1",
        "2 invoke txn r:x r:y",
        "2 ok txn r:x:[1] r:y:[1]",
    ];
    let r = la(&ws, ser);
    assert_eq!(det_str(&r, "anomaly"), "G2", "{}", r.explanation);
    assert_eq!(
        la(&ws, ",\"consistency\":\"snapshot-isolation\"").status,
        OK
    );
}

#[test]
fn sessions_and_more() {
    let mono = [
        "5 invoke append x 1",
        "5 ok append x 1",
        "6 invoke append x 2",
        "6 ok append x 2",
        "0 invoke read x",
        "7 invoke read x",
        "7 ok read x [1,2]",
        "0 ok read x [1,2]",
        "0 invoke read x",
        "0 ok read x [1]",
    ];
    let r = la(&mono, ",\"consistency\":\"strong-session-serializable\"");
    assert!(det_str(&r, "anomalies").contains("monotonic-read"));
    assert_eq!(la(&mono, ",\"consistency\":\"serializable\"").status, OK);
    let r = la(
        &[
            "0 invoke append x 1",
            "0 ok append x 1",
            "0 invoke read x",
            "0 ok read x []",
        ],
        ",\"consistency\":\"strong-session-snapshot-isolation\"",
    );
    assert!(det_str(&r, "anomalies").contains("read-your-writes"));
    assert_eq!(
        la(
            &["0 invoke append x 1", "2 invoke read x", "2 ok read x [1]"],
            ""
        )
        .status,
        OK
    );
    assert_eq!(
        la(
            &[
                "0 invoke append x 1",
                "0 info append x 1",
                "2 invoke read x",
                "2 ok read x []"
            ],
            ""
        )
        .status,
        OK
    );
    let ser = ",\"consistency\":\"serializable\"";
    let r = la(
        &[
            "0 invoke txn a:x:5 r:x",
            "0 ok txn a:x:5 r:x:[1]",
            "1 invoke append x 1",
            "1 ok append x 1",
        ],
        ser,
    );
    assert!(det_str(&r, "anomalies").contains("internal"));
    let r = la(
        &[
            "0 invoke txn a:x:1 a:x:2",
            "1 invoke read x",
            "0 ok txn a:x:1 a:x:2",
            "1 ok read x [1]",
        ],
        ser,
    );
    assert!(det_str(&r, "anomalies").contains("intermediate-read"));
    let rt = [
        "0 invoke append x 1",
        "0 ok append x 1",
        "1 invoke append x 2",
        "1 ok append x 2",
        "2 invoke read x",
        "2 ok read x [1]",
        "3 invoke read x",
        "3 ok read x [1,2]",
    ];
    assert_eq!(la(&rt, "").status, BAD);
    assert_eq!(la(&rt, ser).status, OK);
    assert_eq!(
        la(
            &[
                "0 invoke append x 1",
                "0 ok append x 1",
                "1 invoke append x 1",
                "1 ok append x 1"
            ],
            ""
        )
        .status,
        Outcome::Error
    );
}

#[test]
fn driver() {
    let decl = "HYDO-CHECK {\"id\":\"kv.lin\",\"type\":\"linearizable\",\"model\":\"register\",\"history\":\"kv\"}\n";
    let good = format!(
        "{decl}{}",
        hist(&[
            "0 invoke write x 1",
            "0 ok write x 1",
            "1 invoke read x",
            "1 ok read x 1"
        ])
    );
    let gv = Some("HYDO-ORACLE: OK invariants=1 held (a: b)");
    let r = run_checks(&[("w", good)], gv, hydo_history::DEFAULT_BUDGET, None).unwrap();
    let v = r.verdict.unwrap();
    assert!(v.starts_with("HYDO-ORACLE: OK invariants=1"));
    assert!(v.contains("checks=1 held"));
    assert_eq!(r.records.len(), 2);
    assert_eq!(
        r.records[0],
        "HYDO-ASSERT-DECL {\"id\":\"kv.lin\",\"msg\":\"linearizable history kv is register\",\"t\":\"always\"}"
    );
    let bad = format!(
        "{decl}{}",
        hist(&[
            "0 invoke write x 1",
            "0 ok write x 1",
            "1 invoke read x",
            "1 ok read x nil"
        ])
    );
    let r = run_checks(
        &[("w", bad.as_str())],
        gv,
        hydo_history::DEFAULT_BUDGET,
        None,
    )
    .unwrap();
    assert!(r
        .verdict
        .as_deref()
        .unwrap()
        .starts_with("HYDO-ORACLE: VIOLATION [kv.lin] key x"));
    assert!(r.records[1].contains("\"ok\":false"));
    let g = Some("HYDO-ORACLE: VIOLATION [g] guest first");
    let r = run_checks(
        &[("w", bad.as_str())],
        g,
        hydo_history::DEFAULT_BUDGET,
        None,
    )
    .unwrap();
    assert_eq!(r.verdict.as_deref(), g);
    let r = run_checks(&[("w", "nothing\n")], Some("HYDO-ORACLE: OK x"), 1, None).unwrap();
    assert!(r.verdict.is_none());
}

#[test]
fn final_status() {
    let decl = "HYDO-CHECK {\"id\":\"kv.lin\",\"type\":\"linearizable\",\"history\":\"kv\"}\n";
    let good = format!("{decl}{}", hist(&["0 invoke write x 1", "0 ok write x 1"]));
    let gv = Some("HYDO-ORACLE: OK invariants=0 held ()");
    let ok = format!("{good}HYDO-CHECK-FINAL {{\"id\":\"kv.lin\",\"rc\":0}}\n");
    let r = run_checks(&[("w", ok)], gv, hydo_history::DEFAULT_BUDGET, None).unwrap();
    assert!(r
        .verdict
        .unwrap()
        .starts_with("HYDO-ORACLE: OK checks=1 held (kv.lin: "));
    let bad = format!("{good}HYDO-CHECK-FINAL {{\"id\":\"kv.lin\",\"rc\":2}}\n");
    let r = run_checks(&[("w", bad)], gv, hydo_history::DEFAULT_BUDGET, None).unwrap();
    assert!(r
        .verdict
        .unwrap()
        .starts_with("HYDO-ORACLE: ERROR check kv.lin could not be evaluated"));
}

#[test]
fn encoding() {
    let s = "a b%c@d,e";
    assert_eq!(pdec(&penc(s, ",")), s);
    let r = lin(
        &[
            &format!("0 invoke write x {}", penc("v 1", "")),
            &format!("0 ok write x {}", penc("v 1", "")),
            "1 invoke read x",
            "1 ok read x v%201",
        ],
        "",
    );
    assert_eq!(r.status, OK);
    assert_eq!(pdec("%"), "");
    assert_eq!(penc("", ""), "%");
    assert_eq!(penc("nil", ""), "%6Eil");
    let r = lin(
        &[
            "0 invoke write x %",
            "0 ok write x %",
            "1 invoke read x",
            "1 ok read x %",
        ],
        "",
    );
    assert_eq!(r.status, OK);
    let txt = "HYDO-HIST kv 3 invoke 1000 write k1 a%20b\nHYDO-HIST kv 3 ok 2000 write k1 a%20b@83\nHYDO-HIST kv 4 invoke 3000 read k1\nHYDO-HIST kv 4 ok 4000 read k1 a%20b@83\n";
    let (ev, _, _) = parse_sources(&[("w", txt)]).unwrap();
    assert_eq!(
        run_check(&ev, &spec("linearizable", ""), hydo_history::DEFAULT_BUDGET)
            .unwrap()
            .status,
        OK
    );
    let junk =
        "[  12.3] junk HYDO-HIST kv 0 invoke 1 write x 1\nHYDO-HIST kv 0 bogus 2 write x 1\n";
    let (ev, _, warn) = parse_sources(&[("w", junk)]).unwrap();
    assert_eq!(ev.events.len(), 1);
    assert_eq!(warn.len(), 1);
    let _ = BigInt::from_i64(0);
}

/// An open operation of the generator: f, key, value, took effect, what it read.
type Pending = (String, String, String, bool, Option<Vec<String>>);

/// A random history that is linearizable by construction (test_hydohist.gen_linearizable, with
/// its own seeded generator): each operation takes effect at an instant inside its interval.
fn gen_linearizable(seed: u64, nproc: u64, nops: usize, keys: &[&str], lists: bool) -> Vec<String> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut rnd = move |n: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % n
    };
    let mut store: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut lines = Vec::new();
    let mut open: std::collections::BTreeMap<u64, Pending> = Default::default();
    let (mut n, mut elem) = (0, 0);
    while n < nops || !open.is_empty() {
        let p = rnd(nproc);
        if !open.contains_key(&p) && n < nops {
            let k = keys[rnd(keys.len() as u64) as usize].to_string();
            if lists {
                if rnd(2) == 0 {
                    elem += 1;
                    open.insert(
                        p,
                        ("append".into(), k.clone(), elem.to_string(), false, None),
                    );
                    lines.push(format!("{p} invoke append {k} {elem}"));
                } else {
                    open.insert(p, ("read".into(), k.clone(), String::new(), false, None));
                    lines.push(format!("{p} invoke read {k}"));
                }
            } else {
                let f = if rnd(3) == 0 { "write" } else { "read" };
                let v = rnd(4).to_string();
                open.insert(p, (f.into(), k.clone(), v.clone(), false, None));
                lines.push(if f == "write" {
                    format!("{p} invoke write {k} {v}")
                } else {
                    format!("{p} invoke read {k}")
                });
            }
            n += 1;
            continue;
        }
        let Some((f, k, v, done, res)) = open.get(&p).cloned() else {
            continue;
        };
        if !done {
            let res = match f.as_str() {
                "write" => {
                    store.insert(k.clone(), vec![v.clone()]);
                    None
                }
                "append" => {
                    store.entry(k.clone()).or_default().push(v.clone());
                    None
                }
                _ => Some(store.get(&k).cloned().unwrap_or_default()),
            };
            open.insert(p, (f, k, v, true, res));
            continue;
        }
        open.remove(&p);
        let r = res.unwrap_or_default();
        lines.push(match f.as_str() {
            "write" => format!("{p} ok write {k} {v}"),
            "append" => format!("{p} ok append {k} {v}"),
            _ if lists => format!("{p} ok read {k} [{}]", r.join(",")),
            _ => format!(
                "{p} ok read {k} {}",
                r.last().cloned().unwrap_or_else(|| "nil".into())
            ),
        });
    }
    lines
}

#[test]
fn randomized() {
    for seed in 0..20 {
        let l = gen_linearizable(seed, 5, 300, &["x", "y"], false);
        let refs: Vec<&str> = l.iter().map(String::as_str).collect();
        let r = lin(&refs, "");
        assert_eq!(r.status, OK, "{seed}: {}", r.summary);
    }
    let mut found = 0;
    for seed in 0..20 {
        let mut l = gen_linearizable(seed, 5, 60, &["x"], false);
        let reads: Vec<usize> = (0..l.len())
            .filter(|&i| l[i].contains(" ok read "))
            .collect();
        let i = reads[reads.len() / 2];
        l[i] = format!("{} 7", l[i].rsplit_once(' ').unwrap().0);
        let refs: Vec<&str> = l.iter().map(String::as_str).collect();
        found += (lin(&refs, "").status == BAD) as usize;
    }
    assert_eq!(found, 20);
    let mut l = gen_linearizable(7, 10, 400, &["x"], false);
    let reads: Vec<usize> = (0..l.len())
        .filter(|&i| l[i].contains(" ok read "))
        .collect();
    let i = reads[150];
    l[i] = format!("{} 9", l[i].rsplit_once(' ').unwrap().0);
    let refs: Vec<&str> = l.iter().map(String::as_str).collect();
    let r = lin(&refs, "");
    assert_eq!(det_int(&r, "witness_ops"), 1, "{}", r.explanation);
    for seed in 0..20 {
        let l = gen_linearizable(seed, 5, 300, &["x", "y", "z"], true);
        let refs: Vec<&str> = l.iter().map(String::as_str).collect();
        let r = la(&refs, "");
        assert_eq!(r.status, OK, "{seed}: {}", r.summary);
    }
}
