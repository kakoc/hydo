//! What the explorer reads back from the consoles (failed properties, the property table, the
//! liveness summary) and the report it writes (report.md), as explore.py wrote them.

use crate::pool::{console, read_text};
use crate::py::{float_repr, py_str};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

fn assert_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"HYDO-ASSERT (\{.*\})").expect("pattern"))
}

/// The HYDO-ASSERT records of a text, one per line that has one (lines that do not parse are skipped).
pub fn records(text: &str) -> Vec<Map<String, Value>> {
    text.split('\n')
        .filter_map(|l| assert_re().captures(l))
        .filter_map(|c| serde_json::from_str::<Value>(&c[1]).ok())
        .filter_map(|v| v.as_object().cloned())
        .collect()
}

/// explore.py `assert_failures`: failed property evaluations in one run's consoles, sorted ids.
pub fn assert_failures(outdir: &Path, tag: &str) -> Vec<Value> {
    let mut bad: Vec<Value> = Vec::new();
    for suffix in ["", ".recovery", ".checks"] {
        let Some(text) = read_text(&console(outdir, tag, suffix)) else {
            continue;
        };
        for rec in records(&text) {
            let t = rec.get("t").and_then(Value::as_str).unwrap_or("");
            let ok_false = rec.get("ok") == Some(&Value::Bool(false));
            if (matches!(t, "always" | "always_or_unreachable" | "eventually") && ok_false)
                || t == "unreachable"
            {
                let id = rec.get("id").cloned().unwrap_or(Value::String("?".into()));
                if !bad.contains(&id) {
                    bad.push(id);
                }
            }
        }
    }
    bad.sort_by_key(py_str);
    bad
}

/// explore.py `property_table`: hydo_assert_report (compiled on the fly) over the merged
/// per-schedule consoles of the given runs; None when no run emitted records.
pub fn property_table(outdir: &Path, hydo: &Path, tags: &[String]) -> Option<String> {
    let merged = outdir.join("consoles-merged");
    let _ = std::fs::create_dir_all(&merged);
    let mut files = Vec::new();
    for t in tags {
        let mut text = String::new();
        for suffix in ["", ".recovery", ".checks"] {
            if let Some(x) = read_text(&console(outdir, t, suffix)) {
                text.push_str(&x);
            }
        }
        if text.contains("HYDO-ASSERT") {
            let m = merged.join(format!("{t}.log"));
            let _ = std::fs::write(&m, &text);
            files.push(m);
        }
    }
    if files.is_empty() {
        return None;
    }
    let tool = outdir.join("hydo_assert_report");
    let st = std::process::Command::new("cc")
        .arg("-O2")
        .arg("-o")
        .arg(&tool)
        .arg(hydo.join("assert").join("hydo_assert_report.c"))
        .status();
    if !st.is_ok_and(|s| s.success()) {
        eprintln!("hydo-explore: cannot compile hydo_assert_report.c (cc)");
        return None;
    }
    let (mut rd, wr) = std::io::pipe().ok()?;
    let mut cmd = std::process::Command::new(&tool);
    cmd.args(&files).stdout(wr.try_clone().ok()?).stderr(wr);
    let mut child = cmd.spawn().ok()?;
    drop(cmd);
    let mut out = Vec::new();
    use std::io::Read;
    let _ = rd.read_to_end(&mut out);
    let _ = child.wait();
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// explore.py `liveness_summary`: per eventually property over the merged consoles.
pub fn liveness_summary(outdir: &Path) -> Vec<String> {
    struct P {
        held: i64,
        missed: i64,
        slowest: Option<(f64, String)>,
        first_miss: Option<(String, Value)>,
    }
    let d = outdir.join("consoles-merged");
    let mut names: Vec<String> = std::fs::read_dir(&d)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut props: BTreeMap<String, P> = BTreeMap::new();
    for f in names {
        let text = read_text(&d.join(&f)).unwrap_or_default();
        type Episodes = Vec<(bool, f64, Value)>;
        let mut seen: Vec<(String, Episodes)> = Vec::new();
        for rec in records(&text) {
            if rec.get("t").and_then(Value::as_str) != Some("eventually") {
                continue;
            }
            let det = rec
                .get("details")
                .filter(|v| hydo_manifest::py::truthy(v))
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
            let el = match det.get("elapsed_s") {
                Some(v) if !v.is_null() && v.as_str() != Some("?") => match v {
                    Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.0),
                    other => other.as_f64().unwrap_or(0.0),
                },
                _ => det.get("elapsed_ms").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0,
            };
            let id = rec.get("id").map(py_str).unwrap_or_else(|| "?".into());
            let ok = rec.get("ok") == Some(&Value::Bool(true));
            match seen.iter_mut().find(|(k, _)| *k == id) {
                Some((_, v)) => v.push((ok, el, det)),
                None => seen.push((id, vec![(ok, el, det)])),
            }
        }
        let stem = f
            .strip_suffix(".log")
            .unwrap_or(&f[..f.len().saturating_sub(4)])
            .to_string();
        for (pid, eps) in seen {
            let p = props.entry(pid).or_insert(P {
                held: 0,
                missed: 0,
                slowest: None,
                first_miss: None,
            });
            if eps.iter().all(|e| e.0) {
                p.held += 1;
                let worst = eps.iter().map(|e| e.1).fold(f64::NEG_INFINITY, f64::max);
                if p.slowest.as_ref().is_none_or(|s| worst > s.0) {
                    p.slowest = Some((worst, stem.clone()));
                }
            } else {
                p.missed += 1;
                if p.first_miss.is_none() {
                    let det = eps
                        .iter()
                        .find(|e| !e.0)
                        .map(|e| e.2.clone())
                        .unwrap_or(Value::Null);
                    p.first_miss = Some((stem.clone(), det));
                }
            }
        }
    }
    if props.is_empty() {
        return Vec::new();
    }
    let mut l = vec![
        "## Liveness (eventually properties, hvx/ASSERTIONS.md)".to_string(),
        String::new(),
        "| property | held in time | missed deadline | slowest in time | first miss |".into(),
        "|---|---|---|---|---|".into(),
    ];
    for (pid, p) in &props {
        let slow = p
            .slowest
            .as_ref()
            .map(|(w, s)| format!("{w:.2} s ({s})"))
            .unwrap_or_else(|| "-".into());
        let miss = p
            .first_miss
            .as_ref()
            .map(|(s, det)| {
                format!(
                    "{}: `{}`",
                    s,
                    det.get("out").map(py_str).unwrap_or_default()
                )
            })
            .unwrap_or_else(|| "-".into());
        l.push(format!(
            "| `{pid}` | {} | {} | {slow} | {miss} |",
            p.held, p.missed
        ));
    }
    l.push(String::new());
    l
}

/// explore.py `verdict_kind`.
pub fn verdict_kind(v: &str) -> &'static str {
    hydo_engine::protocol::verdict_kind(v)
}

/// explore.py `signature`: the verdict without its prefix, every number as '#'.
pub fn signature(v: &str) -> String {
    static D: OnceLock<Regex> = OnceLock::new();
    let v = v.replace("HYDO-ORACLE: ", "");
    D.get_or_init(|| Regex::new(r"\d+").expect("p"))
        .replace_all(&v, "#")
        .into_owned()
}

/// The grouping key of a failure: the signature up to " order=" for violations.
pub fn sig_key(v: &str) -> String {
    let s = signature(v);
    if verdict_kind(v) == "violation" {
        s.split(" order=").next().unwrap_or("").to_string()
    } else {
        s
    }
}

/// explore.py `same_failure`.
pub fn same_failure(v_ref: &str, v: &str) -> bool {
    verdict_kind(v) == "violation" && sig_key(v) == sig_key(v_ref)
}

/// A value as `"%s" % v` prints it in a report table.
pub fn cell(v: &Value) -> String {
    match v {
        Value::Number(n) if n.is_f64() => float_repr(n.as_f64().unwrap_or(0.0)),
        other => py_str(other),
    }
}
