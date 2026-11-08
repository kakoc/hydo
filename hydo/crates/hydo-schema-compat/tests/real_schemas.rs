//! The check on the shapes the real generators write (schemars for the project and results
//! formats, hydo-agent-proto's own generator for the agent protocol): every committed schema under
//! `hydo/schema/` is an addition to itself for every kind of reader the check knows and to its
//! released copy for the readers its format has (the project format: readers of the current
//! format), and every removed key, every key made required, every required key made
//! optional and every removed enum value of a definition the root reaches is caught where it is.
//! (Each format's own test checks it with that format's rules.)

use hydo_schema_compat::{check, Kind, Readers, Rules};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

const FORMATS: &[&str] = &["project", "results", "agent"];

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema")
}

fn read(p: PathBuf) -> Value {
    let t = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    serde_json::from_str(&t).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(r) = m.get("$ref").and_then(Value::as_str) {
                if let Some(n) = r.strip_prefix("#/$defs/") {
                    out.push(n.to_string());
                }
            }
            for (k, x) in m {
                if k != "$defs" {
                    refs(x, out);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
        _ => {}
    }
}

/// The definitions the root reaches.
fn reached(s: &Value) -> BTreeSet<String> {
    let mut todo = Vec::new();
    refs(s, &mut todo);
    let mut seen = BTreeSet::new();
    while let Some(n) = todo.pop() {
        if seen.insert(n.clone()) {
            refs(&s["$defs"][&n], &mut todo);
        }
    }
    seen
}

#[test]
fn every_committed_schema_is_an_addition_to_itself_and_to_its_release() {
    for f in FORMATS {
        let now = read(schema_dir().join(format!("{f}-v1.json")));
        let released = read(schema_dir().join(format!("released/{f}-v1.json")));
        for readers in [Readers::Current, Readers::Released] {
            let rules = Rules::new(readers);
            assert_eq!(check(&now, &now, &rules), vec![], "{f} {readers:?}");
            assert_eq!(
                check(&released, &released, &rules),
                vec![],
                "{f} {readers:?}"
            );
            // against the release, each format with the readers it has: the project format's
            // readers are hydo of the current format only, so a widening of it (a new enum value,
            // as `faults:` gained) passes there and not for released readers
            if *f != "project" || readers == Readers::Current {
                assert_eq!(check(&released, &now, &rules), vec![], "{f} {readers:?}");
            }
        }
    }
}

#[test]
fn every_removal_and_every_required_change_is_caught_where_it_is() {
    let mut n = 0;
    for f in FORMATS {
        let s = read(schema_dir().join(format!("{f}-v1.json")));
        let defs = reached(&s);
        assert!(!defs.is_empty(), "{f}");
        for d in &defs {
            let def = &s["$defs"][d];
            let at = format!("#/$defs/{d}");
            let props: Vec<String> = def["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default();
            let required: Vec<String> = def["required"]
                .as_array()
                .map(|r| {
                    r.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let expect = |new: &Value, place: &str, kind: Kind, readers: Readers| {
                let got = check(&s, new, &Rules::new(readers));
                assert!(
                    got.iter().any(|p| p.at == place && p.kind == kind),
                    "{f} {readers:?}: want {kind:?} at {place}, got {got:?}"
                );
            };
            for p in &props {
                let place = format!("{at}/properties/{p}");
                let mut new = s.clone();
                new["$defs"][d]["properties"]
                    .as_object_mut()
                    .unwrap()
                    .shift_remove(p);
                expect(&new, &place, Kind::Changes, Readers::Current);
                expect(&new, &place, Kind::Changes, Readers::Released);
                n += 1;
                if !required.contains(p) {
                    let mut new = s.clone();
                    let r = new["$defs"][d]
                        .as_object_mut()
                        .unwrap()
                        .entry("required")
                        .or_insert(Value::Array(vec![]));
                    r.as_array_mut().unwrap().push(Value::String(p.clone()));
                    expect(&new, &place, Kind::Narrows, Readers::Current);
                    n += 1;
                } else {
                    let mut new = s.clone();
                    new["$defs"][d]["required"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|x| x.as_str() != Some(p));
                    expect(&new, &place, Kind::Widens, Readers::Released);
                    assert_eq!(check(&s, &new, &Rules::new(Readers::Current)), vec![]);
                    n += 1;
                }
                let e = &def["properties"][p]["enum"];
                for (i, x) in e.as_array().into_iter().flatten().enumerate() {
                    let mut new = s.clone();
                    new["$defs"][d]["properties"][p]["enum"]
                        .as_array_mut()
                        .unwrap()
                        .remove(i);
                    let got = check(&s, &new, &Rules::new(Readers::Current));
                    assert!(
                        got.iter().any(|q| q.at == format!("{place}/enum")
                            && q.kind == Kind::Narrows
                            && q.what.contains(&x.to_string())),
                        "{f}: {place} {x}: {got:?}"
                    );
                    n += 1;
                }
            }
        }
    }
    assert!(n > 1000, "only {n} mutations");
}
