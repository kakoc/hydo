//! A broken value property (`// hydo: keep`, Milestone 291) is a finding of kind `values` and
//! a VIOLATION whose words name the property, not the key, so that every run that breaks it has
//! one signature; the advice says what it is.

use hydo_contracts::plan::*;
use hydo_explore::advice::{self, What};
use serde_json::{json, Map, Value};

fn rd(i: u32, t: Ty) -> Expr {
    Expr::In(i, t)
}

fn plan() -> Plan {
    let v = |name: &str, key: bool, show: Show| hydo_contracts::plan::Value {
        name: name.into(),
        text: name.into(),
        line: 1,
        key,
        entry: Some(show),
        at_return: None,
    };
    Plan {
        v: PLAN_V,
        kind: PLAN_TYPE.into(),
        programs: vec![Program {
            program: "/bin/ledger".into(),
            arch: "arm64".into(),
            functions: vec![Function {
                id: 0,
                name: "github.com/x/ledger.copy".into(),
                file: "queue.go".into(),
                line: 10,
                lang: Lang::Go,
                entry: 0x100,
                code: "00".into(),
                frame: Frame::G,
                at_entry: vec![],
                at_return: vec![],
                clauses: vec![],
                inputs: vec![],
                values: vec![
                    v("t.Ref", true, Show::Val(rd(0, Ty::U(8)))),
                    v("limit", false, Show::Val(rd(1, Ty::F(8)))),
                ],
                ret_addr: None,
            }],
            keeps: vec![Keep {
                text: "keep limit".into(),
                file: "queue.go".into(),
                line: 8,
                name: "limit".into(),
                ..Keep::default()
            }],
            ..Program::default()
        }],
    }
}

/// One run's records: keys 7 and 9 keep their limit at the first stop and lose it later, with
/// other values for each run (`limits`).
fn judged(limits: [f64; 2]) -> Map<String, Value> {
    let mut lines = String::new();
    for (at, key, limit) in [
        (10, 7, 50.0),
        (20, 9, 50.0),
        (30, 7, limits[0]),
        (40, 9, limits[1]),
    ] {
        let r = Record {
            f: 0,
            at,
            at_entry: vec![Some(key), Some(f64::to_bits(limit))],
            ..Record::default()
        };
        lines.push_str(&serde_json::to_string(&r).unwrap());
        lines.push('\n');
    }
    let p = std::env::temp_dir().join(format!(
        "hydo-value-properties-{}-{}.contracts.jsonl",
        std::process::id(),
        limits[0]
    ));
    std::fs::write(&p, lines).unwrap();
    let mut res = Map::new();
    res.insert("verdict".into(), json!("HYDO-ORACLE: OK"));
    let j = hydo_explore::contracts::fold(&plan(), &p, &mut res);
    let _ = std::fs::remove_file(&p);
    let j = j.unwrap();
    assert_eq!(j.keeps.len(), 1);
    res
}

#[test]
fn a_broken_value_property_is_a_violation_with_one_signature() {
    let a = judged([0.0, 50.0]);
    let b = judged([50.0, 1.5]);
    let va = a["verdict"].as_str().unwrap();
    assert_eq!(va, "VIOLATION [values keep limit] the value changed");
    assert_eq!(a["guest_verdict"], json!("HYDO-ORACLE: OK"));
    // another key and another value: the same verdict, so one signature for the shrinker
    assert_eq!(b["verdict"], a["verdict"]);
    assert_eq!(
        hydo_explore::results::signature(va),
        hydo_explore::results::signature(b["verdict"].as_str().unwrap())
    );
    let run = hydo_explore::results::run_of("one", "disk=none | run", &a, &[]);
    let f = run
        .findings
        .iter()
        .find(|f| f.kind == hydo_results::kind::VALUES)
        .expect("a values finding");
    assert_eq!(f.id, "values keep limit");
    assert_eq!(f.status, "violation");
    assert_eq!(f.details["first"]["key"], json!("7"));
    assert_eq!(f.details["broken_keys"], json!(1));
    let out: Vec<&str> = f.details["output"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        out.last()
            .unwrap()
            .ends_with("<- BROKEN, first changed here"),
        "{out:?}"
    );
    // the advice knows the kind
    let what = advice::what_of(va, &run.findings);
    assert!(advice::is_values(&what));
    let p = advice::problem("durable-defaults", &what, "Ledger", None, &[]);
    assert_eq!(p.title, "Ledger broke a value property you declared");
    assert!(p.why.contains("`// hydo: keep`"), "{}", p.why);
    assert!(matches!(
        what,
        What::Check {
            description: Some(_),
            ..
        }
    ));
}

#[test]
fn a_kept_value_property_leaves_the_verdict() {
    let r = judged([50.0, 50.0]);
    assert_eq!(r["verdict"], json!("HYDO-ORACLE: OK"));
    assert!(!r.contains_key(hydo_explore::results::VALUES));
}
