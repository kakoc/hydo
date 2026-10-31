//! Two analyses that run no VM: d7prob.py (how often the generators draw a schedule in d7's
//! measured trigger set) and depth1map.py (the trigger map of a defect from two depth-1 sweeps).

use hydo_explore::gen::{Ex, NORDERS};
use hydo_explore::py::{dumps, py_str, repr};
use hydo_explore::report::verdict_kind;
use hydo_explore::sched::{Pause, S};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const END: i64 = NORDERS;

/// d7prob.py `trigger`: the kind of fault that puts the order whose send is uncertain LAST in the
/// topic when the client retries (the record d7's lookup skips), with an honest disk and no power
/// cut; None when the schedule is not in the measured trigger set (hydo/bench/README.md, "d7
/// trigger set": a kill reaching the end from order 27 on, a counted pause reaching the end, no
/// timed pause, reply loss of 90% or more on Kafka over the last order, and one MySQL loss case).
pub fn d7_trigger(s: &S) -> Option<&'static str> {
    if s.disk != "none" {
        return None;
    }
    if let Some((svc, at, n)) = &s.k {
        if svc == "kafka" && at + n >= END && *at >= END - 3 {
            return Some("K");
        }
    }
    if let Some(p) = &s.p {
        if p.svc() == "kafka" {
            if let Pause::Plus(_, at, n) = p {
                if at + n >= END && *at >= 0 {
                    return Some("P+");
                }
            }
            // timed pauses: none of 36 measured triggered
        }
    }
    if let Some((svc, a, b, d, kinds)) = &s.o {
        if svc == "kafka"
            && *b >= END
            && *a == END - 1
            && d == "resp"
            && kinds.len() == 1
            && kinds[0].0 == "loss"
            && kinds[0].1 >= 90
        {
            return Some("O");
        }
    }
    if let Some((svc, a, b, kinds)) = &s.n {
        if svc == "mysql" && *a == END - 3 && *b >= END && kinds == &[("loss".to_string(), 60)] {
            return Some("N");
        }
    }
    None
}

/// d7prob.py: the per-run probability for random and swarm, without and with --new-faults.
pub fn d7prob(seeds: i64) -> String {
    let mut out = String::new();
    for strat in ["random", "swarm"] {
        for nf in [false, true] {
            let ex = Ex {
                new_faults: nf,
                ..Ex::default()
            };
            let mut hits = 0i64;
            let mut kinds: Vec<(&str, i64)> = Vec::new();
            for sd in 1..=seeds {
                let u = sd as u64;
                let g = if strat == "swarm" {
                    ex.generate_swarm(u)
                } else {
                    ex.generate(u)
                };
                let s = ex.add_new_faults(g, u, strat == "swarm");
                if let Some(t) = d7_trigger(&s) {
                    hits += 1;
                    match kinds.iter_mut().find(|(k, _)| *k == t) {
                        Some(e) => e.1 += 1,
                        None => kinds.push((t, 1)),
                    }
                }
            }
            let p = hits as f64 / seeds as f64;
            let one_in = if p != 0.0 {
                format!("{:.0}", 1.0 / p)
            } else {
                "inf".into()
            };
            let kd = kinds
                .iter()
                .map(|(k, n)| format!("{}: {n}", repr(k)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "{:<6} {:<11} p={:.5} (1 in {})  by kind {{{}}}\n",
                strat,
                if nf { "+new-faults" } else { "" },
                p,
                one_in,
                kd
            ));
        }
    }
    out
}

/// A sweep folder: seed -> point label (sweep-points.jsonl), seed -> run record (runs.jsonl).
type Sweep = (BTreeMap<i64, String>, BTreeMap<i64, Value>);

fn load(d: &Path) -> Result<Sweep, String> {
    let read = |n: &str| {
        let p = d.join(n);
        std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))
    };
    let mut pts = BTreeMap::new();
    for l in read("sweep-points.jsonl")?.lines() {
        let v: Value = serde_json::from_str(l).map_err(|e| e.to_string())?;
        pts.insert(v["seed"].as_i64().unwrap_or(0), py_str(&v["point"]));
    }
    let mut runs = BTreeMap::new();
    for l in read("runs.jsonl")?.lines() {
        let v: Value = serde_json::from_str(l).map_err(|e| e.to_string())?;
        runs.insert(v["seed"].as_i64().unwrap_or(0), v);
    }
    Ok((pts, runs))
}

fn list_repr(v: &[String]) -> String {
    format!(
        "[{}]",
        v.iter().map(|s| repr(s)).collect::<Vec<_>>().join(", ")
    )
}

/// depth1map.py: a point TRIGGERS the defect when the defect image's verdict is a VIOLATION and
/// the control's verdict for the same point is OK. Returns (the printed lines, the --json map).
pub fn depth1map(defect: &Path, control: &Path, sig: &str) -> Result<(String, String), String> {
    let (pts, dr) = load(defect)?;
    let (_, cr) = load(control)?;
    let (mut trig, mut both, mut ctl_only, mut nonrepro) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (sd, p) in &pts {
        let dv = dr
            .get(sd)
            .map(|r| py_str(&r["verdict"]))
            .ok_or_else(|| format!("{}: no run for seed {sd}", defect.display()))?;
        let cv = cr
            .get(sd)
            .map(|r| py_str(&r["verdict"]))
            .ok_or_else(|| format!("{}: no run for seed {sd}", control.display()))?;
        let dbad = verdict_kind(&dv) == "violation";
        let cbad = verdict_kind(&cv) == "violation";
        if dr[sd].get("reproduced") == Some(&json!(false)) {
            nonrepro.push(p.clone());
        }
        if dbad && !cbad {
            trig.push((p.clone(), dv));
        } else if dbad && cbad {
            both.push((p.clone(), cv));
        } else if cbad {
            ctl_only.push((p.clone(), cv));
        }
    }
    let mut kinds: Vec<(String, usize)> = Vec::new();
    for (p, _) in &trig {
        let k = p.split('=').next().unwrap_or("").to_string();
        match kinds.iter_mut().find(|(x, _)| *x == k) {
            Some(e) => e.1 += 1,
            None => kinds.push((k, 1)),
        }
    }
    let kd = kinds
        .iter()
        .map(|(k, n)| format!("{}: {n}", repr(k)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "points {}  triggers {} {{{}}}  fail-on-both {}  control-only {}  not-reproduced {}\n",
        pts.len(),
        trig.len(),
        kd,
        both.len(),
        ctl_only.len(),
        list_repr(&nonrepro)
    );
    if !sig.is_empty() {
        let re = regex::Regex::new(sig).map_err(|e| e.to_string())?;
        let ok = trig.iter().filter(|(_, v)| re.is_match(v)).count();
        out.push_str(&format!("signature match {ok} of {}\n", trig.len()));
    }
    for (p, v) in &trig {
        let v: String = v.chars().take(90).collect();
        out.push_str(&format!("  T {p} | {v}\n"));
    }
    let pairs = |v: &[(String, String)]| -> Value {
        Value::Array(v.iter().map(|(a, b)| json!([a, b])).collect())
    };
    let mut m = serde_json::Map::new();
    m.insert("triggers".into(), pairs(&trig));
    m.insert("both".into(), pairs(&both));
    m.insert("control_only".into(), pairs(&ctl_only));
    m.insert("nonrepro".into(), json!(nonrepro));
    Ok((out, dumps(&Value::Object(m), 1)))
}
