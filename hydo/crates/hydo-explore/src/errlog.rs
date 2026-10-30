//! Error logs as findings (M94; hydo/onboard/hydo_errors.py's explorer side): a run whose console
//! carries `HYDO-ERRLOG` records (an image whose manifest has `errors:`) has its error kinds compared
//! with the reviewed list of known kinds. The per-run analysis (`load_known`, `analyse`) is still
//! hydo_errors.py behind the bridge, called only for runs whose console has a summary record; what
//! the explorer does with the analyses (`apply`, `collect`, the report section) is here.

use crate::bridge;
use crate::py::{head, py_str};
use serde_json::{json, Map, Value};
use std::path::Path;

/// The known list as hydo_errors.load_known returns it (without compiled regexes).
#[derive(Clone, Debug)]
pub struct Known {
    pub v: Value,
    /// `--known-errors` as given (the bridge loads it again for each analysis).
    pub path: Option<String>,
}

impl Known {
    pub fn load(onboard: &Path, path: Option<&str>) -> Result<Known, String> {
        let v = bridge::call(onboard, "errors_load_known", json!({"path": path}))?;
        if let Some(e) = v.get("error").and_then(Value::as_str) {
            return Err(e.to_string());
        }
        Ok(Known {
            v,
            path: path.map(str::to_string),
        })
    }

    pub fn unknown(&self) -> String {
        py_str(self.v.get("unknown").unwrap_or(&Value::Null))
    }

    pub fn set_unknown(&mut self, u: &str) {
        self.v["unknown"] = json!(u);
    }

    /// One run's analysis (None: the image has no `errors:`).
    pub fn analyse(
        &self,
        onboard: &Path,
        prefix: &Path,
        schedule: &str,
        consoles: &[String],
    ) -> Result<Option<Value>, String> {
        if !consoles.iter().any(|t| t.contains("HYDO-ERRLOG-SUM")) {
            return Ok(None);
        }
        let an = bridge::call(
            onboard,
            "errors_analyse",
            json!({"path": self.path, "unknown": self.unknown(), "prefix": prefix.display().to_string(), "schedule": schedule}),
        )?;
        Ok((!an.is_null()).then_some(an))
    }
}

fn arr(v: &Value) -> Vec<Value> {
    v.as_array().cloned().unwrap_or_default()
}

/// hydo_errors.apply: fold one run's analysis into its result.
pub fn apply(res: &mut Map<String, Value>, an: &Value, known: &Known) {
    let new = arr(&an["new"]);
    res.insert(
        "errors_new".into(),
        Value::Array(new.iter().map(|k| k["sig"].clone()).collect()),
    );
    res.insert("errors_known".into(), json!(arr(&an["known"]).len()));
    let verdict = res.get("verdict").map(py_str).unwrap_or_default();
    if known.unknown() == "fail" && !new.is_empty() && verdict.contains("HYDO-ORACLE: OK") {
        res.insert("pre_errlog_verdict".into(), json!(verdict));
        let k = &new[0];
        res.insert(
            "verdict".into(),
            json!(format!(
                "VIOLATION error-log [{}] {} ({})",
                py_str(&k["svc"]),
                head(&py_str(&k["tpl"]), 160),
                py_str(&k["sig"])
            )),
        );
    }
}

/// hydo_errors.collect: runs [(seed, schedule, analysis)] in seed order -> {sig: aggregate}.
pub fn collect(runs: &[(i64, String, Option<Value>)]) -> Map<String, Value> {
    let mut agg: Map<String, Value> = Map::new();
    for (seed, sched, an) in runs {
        let Some(an) = an else { continue };
        for (status, ks) in [("new", arr(&an["new"])), ("known", arr(&an["known"]))] {
            for k in ks {
                let sig = py_str(&k["sig"]);
                let a = agg.entry(sig.clone()).or_insert_with(|| {
                    json!({"sig": k["sig"], "svc": k["svc"], "src": k["src"], "tpl": k["tpl"], "ex": k["ex"],
                           "runs": 0, "new_runs": 0, "seeds": [], "first_seed": seed,
                           "first_schedule": sched, "first_at": k["at"], "lines": 0})
                });
                let o = a.as_object_mut().expect("an object");
                let inc = |o: &mut Map<String, Value>, f: &str, by: i64| {
                    let v = o.get(f).and_then(Value::as_i64).unwrap_or(0) + by;
                    o.insert(f.into(), json!(v));
                };
                inc(o, "runs", 1);
                inc(o, "lines", k["n"].as_i64().unwrap_or(0));
                if status == "new" {
                    inc(o, "new_runs", 1);
                    if !o.contains_key("first_new_seed") {
                        o.insert("first_new_seed".into(), json!(seed));
                        o.insert("first_new_schedule".into(), json!(sched));
                        o.insert("first_new_at".into(), k["at"].clone());
                        o.insert("first_new_ex".into(), k["ex"].clone());
                    }
                } else if !o.contains_key("reason") {
                    o.insert(
                        "reason".into(),
                        k.get("reason").cloned().unwrap_or(Value::Null),
                    );
                }
                if let Some(Value::Array(s)) = o.get_mut("seeds") {
                    if s.len() < 12 {
                        s.push(json!(seed));
                    }
                }
            }
        }
    }
    agg
}

fn label_text(labels: &Value) -> String {
    arr(labels)
        .iter()
        .map(|l| match py_str(l).as_str() {
            "s" => "setup".to_string(),
            "r" => "recovery".to_string(),
            x => x.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn first_new_key(a: &Value) -> (i64, String) {
    (a["first_new_seed"].as_i64().unwrap_or(0), py_str(&a["sig"]))
}

/// The new kinds the explorer shrinks: not written before the workload, by first seed.
pub fn to_shrink(agg: &Map<String, Value>) -> Vec<Value> {
    let mut new: Vec<Value> = agg
        .values()
        .filter(|x| x["new_runs"].as_i64().unwrap_or(0) > 0)
        .filter(|x| !arr(&x["first_new_at"]).iter().any(|l| l == "s"))
        .cloned()
        .collect();
    new.sort_by_key(first_new_key);
    new
}

/// hydo_errors.report_md: the report.md section.
pub fn report_md(
    agg: &Map<String, Value>,
    known: &Known,
    shrunk: &Map<String, Value>,
    by_hand: &dyn Fn(&str) -> String,
) -> Vec<String> {
    let mut new: Vec<&Value> = agg
        .values()
        .filter(|a| a["new_runs"].as_i64().unwrap_or(0) > 0)
        .collect();
    let mut old: Vec<&Value> = agg
        .values()
        .filter(|a| a["new_runs"].as_i64().unwrap_or(0) == 0)
        .collect();
    let k = &known.v;
    let sha = k.get("sha256").filter(|v| !v.is_null()).map(py_str);
    let mut l = vec![
        "## Error logs (errors: in the manifest)".to_string(),
        String::new(),
        format!(
            "Known list: {}{}; unknown kinds: {}. {} new kind{}, {} only known.",
            k.get("path")
                .filter(|v| !v.is_null())
                .map(py_str)
                .unwrap_or_else(|| "none".into()),
            sha.map(|s| format!(" (sha256 {})", head(&s, 16)))
                .unwrap_or_default(),
            known.unknown(),
            new.len(),
            if new.len() == 1 { "" } else { "s" },
            old.len()
        ),
        String::new(),
    ];
    new.sort_by_key(|a| first_new_key(a));
    if !new.is_empty() {
        l.extend(
            [
                "### New kinds",
                "",
                "| signature | service | runs | first seed | at (first run) | example |",
                "|---|---|---|---|---|---|",
            ]
            .map(String::from),
        );
        for a in &new {
            l.push(format!(
                "| `{}` | {} | {} | {} | {} | `{}` |",
                py_str(&a["sig"]),
                py_str(&a["svc"]),
                a["new_runs"],
                py_str(&a["first_new_seed"]),
                label_text(&a["first_new_at"]),
                head(&py_str(&a["first_new_ex"]), 100).replace('|', "/")
            ));
        }
        l.push(String::new());
        for a in &new {
            let sig = py_str(&a["sig"]);
            l.push(format!(
                "#### `{}` ({}): `{}`",
                sig,
                py_str(&a["svc"]),
                head(&py_str(&a["tpl"]), 160)
            ));
            l.push(String::new());
            l.push(format!(
                "- example: `{}`",
                head(&py_str(&a["first_new_ex"]), 200)
            ));
            l.push(format!(
                "- first schedule (seed {}): `{}`",
                py_str(&a["first_new_seed"]),
                py_str(&a["first_new_schedule"])
            ));
            if arr(&a["first_new_at"]).iter().any(|x| x == "s") {
                l.push("- written before the workload (boot, provisioning), so it is in every schedule: not shrunk; triage it first".into());
            }
            if let Some(s) = shrunk.get(&sig) {
                let same = s["fp_a"] == s["fp_b"];
                l.push(format!(
                    "- minimal schedule ({} shrink runs): `{}`",
                    s["tries"],
                    py_str(&s["minimal"])
                ));
                l.push(format!(
                    "- run twice: fingerprints `{}` / `{}` ({}), the kind {}",
                    py_str(&s["fp_a"]),
                    py_str(&s["fp_b"]),
                    if same { "identical" } else { "DIFFERENT" },
                    if s["present_a"] == true && s["present_b"] == true {
                        "in both runs"
                    } else {
                        "NOT in both runs"
                    }
                ));
                l.push(format!("- by hand: {}", by_hand(&py_str(&s["minimal"]))));
            }
            l.push(format!(
                "- expected? `hydo errors accept {sig} --reason \"...\"` (or `--during <fault>` to accept it only while a fault is on)"
            ));
            l.push(String::new());
        }
    }
    if !old.is_empty() {
        l.extend(
            [
                "### Known kinds seen",
                "",
                "| signature | service | runs | reason |",
                "|---|---|---|---|",
            ]
            .map(String::from),
        );
        old.sort_by_key(|a| py_str(&a["sig"]));
        for a in old {
            let reason = match a.get("reason") {
                Some(Value::Null) | None => String::new(),
                Some(v) => py_str(v),
            };
            l.push(format!(
                "| `{}` | {} | {} | {} |",
                py_str(&a["sig"]),
                py_str(&a["svc"]),
                a["runs"],
                head(&reason, 100).replace('|', "/")
            ));
        }
        l.push(String::new());
    }
    l
}
