//! Compare two exploration directories made with different engines (M132: `hvx_explore` and the
//! Rust engine): every `run` record of `events.jsonl` (schedule, role, verdict, fingerprints,
//! findings, extras; not the timing, the worker or the order runs finished in), the `start` and
//! `end` records, `exploration.json`, and the consoles byte for byte. Fields that measure time
//! (`secs`, and keys with `secs`, `wall`, `time`, `elapsed` or `duration` in their names) and the
//! engine's argv are left out. Prints the counts and every difference; exit status 1 on one.
//!
//! ```text
//! cargo run --release -p hydo-explore --example engine_equiv -- <exploration A> <exploration B>
//! ```

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

fn timing(k: &str) -> bool {
    k == "secs"
        || k == "worker"
        || k == "argv"
        || k == "out"
        || [
            "secs", "wall", "time", "elapsed", "duration", "boot_s", "per_hour", "mean_run",
        ]
        .iter()
        .any(|w| k.contains(w))
}

fn strip(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.retain(|k, _| !timing(k));
            for x in m.values_mut() {
                strip(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(strip),
        _ => {}
    }
}

fn read_events(dir: &Path) -> (Vec<String>, Vec<Value>) {
    let t = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    let mut runs = Vec::new();
    let mut other = Vec::new();
    for l in t.lines().filter(|l| !l.trim().is_empty()) {
        let mut v: Value = serde_json::from_str(l).expect("events.jsonl line");
        strip(&mut v);
        if v["type"] == "run" {
            runs.push(serde_json::to_string(&v).unwrap());
        } else {
            other.push(v);
        }
    }
    runs.sort();
    (runs, other)
}

fn diff(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                diff(
                    x.get(k).unwrap_or(&Value::Null),
                    y.get(k).unwrap_or(&Value::Null),
                    &format!("{path}.{k}"),
                    out,
                );
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (p, q)) in x.iter().zip(y).enumerate() {
                diff(p, q, &format!("{path}[{i}]"), out);
            }
        }
        _ if a != b => out.push(format!("{path}: {a} | {b}")),
        _ => {}
    }
}

fn consoles(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut m = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("consoles")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".log") && !n.contains(".agent.") {
                m.insert(n, std::fs::read(e.path()).unwrap_or_default());
            }
        }
    }
    m
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() != 2 {
        eprintln!("usage: engine_equiv <exploration A> <exploration B>");
        std::process::exit(2);
    }
    let (da, db) = (Path::new(&a[0]), Path::new(&a[1]));
    let mut bad = Vec::new();
    let (ra, oa) = read_events(da);
    let (rb, ob) = read_events(db);
    let only_a: Vec<&String> = ra.iter().filter(|r| !rb.contains(r)).collect();
    let only_b: Vec<&String> = rb.iter().filter(|r| !ra.contains(r)).collect();
    for r in &only_a {
        bad.push(format!("run only in A: {r}"));
    }
    for r in &only_b {
        bad.push(format!("run only in B: {r}"));
    }
    if oa.len() != ob.len() {
        bad.push(format!(
            "{} other records in A, {} in B",
            oa.len(),
            ob.len()
        ));
    }
    for (x, y) in oa.iter().zip(&ob) {
        diff(x, y, "events", &mut bad);
    }
    let ex = |d: &Path| -> Value {
        let mut v: Value = std::fs::read_to_string(d.join("exploration.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        strip(&mut v);
        v
    };
    diff(&ex(da), &ex(db), "exploration", &mut bad);
    // a console whose run is in neither events.jsonl belongs to a shrink candidate the explorer
    // started and then cancelled (its round had already kept a candidate): which ones get started
    // depends on how fast runs finish, so they are counted, not compared
    let tags: std::collections::BTreeSet<String> = ra
        .iter()
        .chain(&rb)
        .filter_map(|r| {
            let v: Value = serde_json::from_str(r).ok()?;
            v["tag"].as_str().map(str::to_string)
        })
        .collect();
    let recorded = |n: &str| {
        let t = n.trim_end_matches(".log").trim_end_matches(".recovery");
        tags.contains(t)
    };
    let (mut ca, mut cb) = (consoles(da), consoles(db));
    let cancelled = ca.keys().chain(cb.keys()).filter(|n| !recorded(n)).count();
    ca.retain(|n, _| recorded(n));
    cb.retain(|n, _| recorded(n));
    let mut same = 0;
    for (n, x) in &ca {
        match cb.get(n) {
            Some(y) if x == y => same += 1,
            Some(_) => bad.push(format!("console {n} differs")),
            None => bad.push(format!("console {n} only in A")),
        }
    }
    for n in cb.keys().filter(|n| !ca.contains_key(*n)) {
        bad.push(format!("console {n} only in B"));
    }
    if cancelled > 0 {
        println!("{cancelled} consoles of cancelled runs (in neither events.jsonl) set aside");
    }
    let roles = |rs: &[String]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for r in rs {
            let v: Value = serde_json::from_str(r).unwrap();
            *m.entry(v["role"].as_str().unwrap_or("?").to_string())
                .or_default() += 1;
        }
        m
    };
    println!(
        "runs: A {} B {} identical {} (roles {:?}); consoles: {} identical of {}",
        ra.len(),
        rb.len(),
        ra.iter().filter(|r| rb.contains(r)).count(),
        roles(&ra),
        same,
        ca.len().max(cb.len())
    );
    if bad.is_empty() {
        println!("engine_equiv: IDENTICAL");
    } else {
        for b in &bad {
            println!("DIFF {b}");
        }
        println!("engine_equiv: {} differences", bad.len());
        std::process::exit(1);
    }
}
