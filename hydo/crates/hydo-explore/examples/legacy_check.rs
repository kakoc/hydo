//! legacy_check: does a results-format directory hold everything the old files of the same run
//! held? (M115's proof; a development tool, not shipped.)
//!
//!   cargo run -p hydo-explore --example legacy_check -- <old dir> <new dir> [<old dir> <new dir>]...
//!
//! An old directory is what hydo wrote before M115 for a run (run.json, one.checks.log,
//! one.checks.txt, one.errors.json) or an exploration (runs.jsonl, shrink.jsonl, report.json,
//! errors.json, properties.txt, sweep-points.jsonl, fc-traces.jsonl, fc-adaptive.json,
//! consoles/<tag>.errors.json, consoles/<tag>.checks.log|txt, report.md, the printed summary in
//! `stdout` next to the directory). The new directory is the same run or exploration made by the
//! results-format hydo (result.json, or exploration.json + events.jsonl + report.md), read with
//! the results reader. Every value of every old file is looked up at its place in the new model
//! (the mapping below, key by key); a key the mapping does not know is reported as UNMAPPED, a
//! value that differs or is absent as MISSING. Wall times, worker numbers and paths (two runs, two
//! directories) are compared by presence only. Exit status 1 when anything is missing.

use hydo_explore::results::render_properties;
use hydo_results::reader::{read_events, read_exploration, read_run};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Keys whose values are not a function of the schedule: compared by presence only.
const TIMING: &[&str] = &[
    "secs",
    "sweep_wall_s",
    "runs_per_hour",
    "total_wall_s",
    "stage1_wall_s",
    "stage2_wall_s",
    "mean_run_s",
    "worker_boot_s",
    "quiet_wait_s",
    "worker",
    "console",
];

#[derive(Default)]
struct Check {
    checked: usize,
    files: usize,
    /// Files of shrink candidates the shrink did not use ([`unused_candidate`]).
    unused: usize,
    bad: Vec<String>,
}

impl Check {
    fn miss(&mut self, at: &str, what: String) {
        self.bad.push(format!("MISSING {at}: {what}"));
    }
    fn unmapped(&mut self, at: &str, key: &str) {
        self.bad.push(format!("UNMAPPED {at}: key {key}"));
    }
    /// `old` (from an old file) must equal `new` (from the model).
    fn eq(&mut self, at: &str, key: &str, old: &Value, new: Option<&Value>) {
        self.checked += 1;
        let null = Value::Null;
        let n = new.unwrap_or(&null);
        let timing = TIMING.contains(&key);
        let ok = if timing {
            old.is_null() || !n.is_null()
        } else if key == "line" {
            // the engine's RESULT line: the same but for its wall time (secs=)
            let re = regex::Regex::new(r"secs=\S+").unwrap();
            let s = |v: &Value| {
                re.replace_all(v.as_str().unwrap_or(""), "secs=N")
                    .into_owned()
            };
            s(old) == s(n)
        } else {
            same(old, n)
        };
        if !ok {
            self.miss(at, format!("{key} = {old} (new: {n})"));
        }
    }
}

/// JSON equality with numbers compared as numbers (1 == 1.0) and an absent value as null.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for p in path.split('.') {
        cur = match p.parse::<usize>() {
            Ok(i) => cur.get(i)?,
            Err(_) => cur.get(p)?,
        };
    }
    (!cur.is_null()).then_some(cur)
}

fn read(p: &Path) -> Option<String> {
    std::fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn json_of(p: &Path) -> Option<Value> {
    serde_json::from_str(&read(p)?).ok()
}

fn lines_of(p: &Path) -> Vec<Value> {
    read(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Wall times and worker numbers as `N` (report.md, the printed summary).
fn norm(text: &str) -> String {
    let re = regex::Regex::new(
        r#"("?(?:sweep_wall_s|runs_per_hour|total_wall_s|stage1_wall_s|stage2_wall_s|mean_run_s|worker_boot_s|workers|worker)"?(?::| \|) )[0-9.]+"#,
    )
    .unwrap();
    re.replace_all(text, "${1}N").into_owned()
}

fn ids(v: &Value, kind: &str, status: Option<&str>) -> Vec<Value> {
    let mut out: Vec<Value> = v
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| f["kind"] == kind && status.is_none_or(|s| f["status"] == s))
        .map(|f| f["id"].clone())
        .collect();
    out.sort_by_key(|x| x.to_string());
    out
}

fn sorted(v: &Value) -> Value {
    let mut a = v.as_array().cloned().unwrap_or_default();
    a.sort_by_key(|x| x.to_string());
    Value::Array(a)
}

/// One old run record (runs.jsonl, a failure of report.json, run.json) against a new run.
fn run_fields(c: &mut Check, at: &str, old: &Map<String, Value>, new: &Value) {
    for (k, v) in old {
        let n = |p: &str| get(new, p).cloned();
        let got = match k.as_str() {
            "seed" => n("seed"),
            "tag" => n("tag"),
            "schedule" => n("schedule"),
            "class" => n("class"),
            "fp" => n("fingerprints.fp"),
            "wfp" => n("fingerprints.wfp"),
            "secs" => n("secs"),
            "worker" => n("worker"),
            "verdict" => n("verdict.text"),
            "guest_verdict" => n("verdict.guest"),
            "pre_errlog_verdict" => n("verdict.before_error_logs"),
            "cached" => n("cached"),
            "line" => n("result_line"),
            "tjk" | "tjn" | "pctf" | "pctw" | "xtn" | "qn" => n(&format!("counters.{k}")),
            "branch_tjlo" => n("branch.tjlo"),
            "branch_held" => n("branch.held"),
            "errors_new" => {
                c.eq(
                    at,
                    k,
                    &sorted(v),
                    Some(&Value::Array(ids(new, "error-log", Some("new")))),
                );
                continue;
            }
            "errors_known" => Some(json!(ids(new, "error-log", Some("known")).len())),
            "assert_failures" => {
                c.eq(
                    at,
                    k,
                    &sorted(v),
                    Some(&Value::Array(ids(new, "assertion", None))),
                );
                continue;
            }
            // report.json failures: the re-runs (checked by the caller)
            "repro_fp" | "repro_verdict" | "reproduced" | "timing_only" => continue,
            k if k.starts_with("repro") && k.ends_with("_fp") => continue,
            // run.json: the document's own fields (checked by the caller)
            "image" | "image_sha256" | "engine_args" => continue,
            _ => {
                c.unmapped(at, k);
                continue;
            }
        };
        c.eq(at, k, v, got.as_ref());
    }
}

/// `<tag>.checks.log` (HYDO-ASSERT records of the host's history checks) against a run's
/// history-check findings.
fn checks_log(c: &mut Check, at: &str, text: &str, new: &Value) {
    let fs: Vec<&Value> = new
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| f["kind"] == "history-check")
        .collect();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let (decl, js) = match line.split_once(' ') {
            Some(("HYDO-ASSERT-DECL", j)) => (true, j),
            Some(("HYDO-ASSERT", j)) => (false, j),
            _ => {
                c.unmapped(at, line);
                continue;
            }
        };
        let rec: Value = serde_json::from_str(js).unwrap_or(Value::Null);
        let id = rec["id"].as_str().unwrap_or_default().to_string();
        let Some(f) = fs.iter().find(|f| f["id"] == id.as_str()) else {
            c.miss(at, format!("history check {id}"));
            continue;
        };
        c.eq(
            at,
            "msg",
            &rec["msg"],
            f.get("details").and_then(|d| d.get("description")),
        );
        c.eq(at, "t", &rec["t"], Some(&json!("always")));
        if !decl {
            let st = if rec["ok"] == true { "ok" } else { "violation" };
            c.eq(at, "ok", &json!(st), Some(&f["status"]));
            c.eq(
                at,
                "details",
                &rec["details"],
                f.get("details").and_then(|d| d.get("details")),
            );
        }
    }
}

/// `<tag>.checks.txt` (the checks' report, then the verdict) made again from the findings.
fn checks_txt(c: &mut Check, at: &str, text: &str, new: &Value) {
    let mut rep: Vec<String> = Vec::new();
    for f in new
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| f["kind"] == "history-check")
    {
        let d = &f["details"];
        let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
        rep.push(format!(
            "== {} ({}, history {}): {}",
            s(&f["id"]),
            s(&d["check"]),
            s(&d["history"]),
            s(&f["status"]).to_uppercase()
        ));
        rep.push(s(&f["message"]));
        if let Some(e) = d.get("explanation").and_then(Value::as_str) {
            rep.push(e.to_string());
        }
        for w in d
            .get("warnings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .take(20)
        {
            rep.push(format!("warning: {}", s(w)));
        }
        rep.push(String::new());
    }
    let made = format!(
        "{}\n{}\n",
        rep.join("\n"),
        get(new, "verdict.text")
            .and_then(Value::as_str)
            .unwrap_or("None")
    );
    c.checked += 1;
    if made != text {
        c.miss(
            at,
            "the checks' report made from the findings differs from the file".into(),
        );
    }
}

/// One run's error-log analysis (consoles/<tag>.errors.json, one.errors.json's kinds).
fn errlog_kinds(c: &mut Check, at: &str, kinds: &[(Value, bool)], new: &Value) {
    let fs: Vec<&Value> = new
        .get("findings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|f| f["kind"] == "error-log")
        .collect();
    for (k, is_new) in kinds {
        let sig = k["sig"].as_str().unwrap_or_default();
        let st = if *is_new { "new" } else { "known" };
        let Some(f) = fs.iter().find(|f| f["id"] == sig && f["status"] == st) else {
            c.miss(at, format!("error kind {sig} ({st})"));
            continue;
        };
        for (key, v) in k.as_object().into_iter().flatten() {
            let got = match key.as_str() {
                "sig" | "new" => continue,
                "runs" => {
                    c.eq(at, key, v, Some(&json!(1)));
                    continue;
                }
                "svc" => f["details"].get("service"),
                "src" => f["details"].get("source"),
                "tpl" => f["details"].get("template"),
                "ex" => f["details"].get("example"),
                "n" => f["details"].get("lines"),
                "at" => f["details"].get("at"),
                "entry" => f["details"].get("known_entry"),
                "reason" => f["details"].get("reason"),
                _ => {
                    c.unmapped(at, key);
                    continue;
                }
            };
            c.eq(at, key, v, got);
        }
    }
}

fn errlog_context(c: &mut Check, at: &str, old: &Value, new: &Value, keys: &[&str]) {
    for (k, v) in old.as_object().into_iter().flatten() {
        if !keys.contains(&k.as_str()) {
            if !["kinds", "new", "known"].contains(&k.as_str()) {
                c.unmapped(at, k);
            }
            continue;
        }
        c.eq(at, k, v, get(new, &format!("error_logs.{k}")));
    }
}

fn check_run(c: &mut Check, old: &Path, new: &Path) {
    let run = read_run(&new.join(hydo_results::RUN_DOC)).expect("a result.json");
    let nv = serde_json::to_value(&run).unwrap();
    for e in std::fs::read_dir(old).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let at = format!("{}", p.display());
        c.files += 1;
        match name.as_str() {
            "run.json" => {
                let o = json_of(&p).unwrap();
                let o = o.as_object().unwrap();
                run_fields(c, &at, o, &nv);
                c.eq(&at, "image", &o["image"], get(&nv, "image.path"));
                c.eq(
                    &at,
                    "image_sha256",
                    &o["image_sha256"],
                    get(&nv, "image.sha256"),
                );
                c.eq(
                    &at,
                    "engine_args",
                    &o["engine_args"],
                    get(&nv, "engine.args"),
                );
            }
            "one.checks.log" => checks_log(c, &at, &read(&p).unwrap(), &nv),
            "one.checks.txt" => checks_txt(c, &at, &read(&p).unwrap(), &nv),
            "one.errors.json" => {
                let o = json_of(&p).unwrap();
                errlog_context(
                    c,
                    &at,
                    &o,
                    &nv,
                    &["known_file", "known_sha256", "unknown", "steps", "boots"],
                );
                let kinds: Vec<(Value, bool)> = o["kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|k| {
                        let mut k = k.clone();
                        let n = k["new"] == true;
                        k.as_object_mut().unwrap().remove("new");
                        (k, n)
                    })
                    .collect();
                errlog_kinds(c, &at, &kinds, &nv);
            }
            "one.log" | "one.recovery.log" => {
                c.checked += 1;
                let listed = run
                    .artifacts
                    .as_ref()
                    .is_some_and(|a| a.consoles.contains(&name));
                if !listed || read(&p) != read(&new.join(&name)) {
                    c.miss(
                        &at,
                        format!("{name}: not listed in result.json, or not the same console"),
                    );
                }
            }
            "engine.log" => {
                c.checked += 1;
                if !new.join("engine.log").is_file() {
                    c.miss(&at, "engine.log".into());
                }
            }
            _ => c.unmapped(&at, &format!("file {name}")),
        }
    }
}

fn check_exploration(c: &mut Check, old: &Path, new: &Path) {
    let doc = read_exploration(&new.join(hydo_results::EXPLORATION_DOC)).expect("exploration.json");
    // (a converted fixture has no stream)
    let evp = new.join(hydo_results::EVENTS);
    let ev = if evp.is_file() {
        read_events(&evp).expect("events.jsonl")
    } else {
        hydo_results::Stream::default()
    };
    assert!(
        ev.skipped.is_empty(),
        "the reader skipped records it should know"
    );
    let dv = serde_json::to_value(&doc).unwrap();
    let runs: Vec<Value> = ev
        .runs()
        .map(|r| serde_json::to_value(r).unwrap())
        .collect();
    let by_tag: HashMap<String, &Value> = runs
        .iter()
        .filter_map(|r| r["tag"].as_str().map(|t| (t.to_string(), r)))
        .collect();
    let branch = doc.mode == "branch";
    let first = |key: &Value| -> Option<&Value> {
        runs.iter().find(|r| {
            if branch {
                r["tag"] == *key
            } else {
                r["role"] == "first" && r["seed"] == *key
            }
        })
    };
    let mut files: Vec<PathBuf> = std::fs::read_dir(old)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    files.sort();
    for p in files {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let at = format!("{}", p.display());
        c.files += 1;
        match name.as_str() {
            "runs.jsonl" => {
                for (i, o) in lines_of(&p).iter().enumerate() {
                    let key = if branch { &o["tag"] } else { &o["seed"] };
                    let at = format!("{at}:{}", i + 1);
                    match first(key) {
                        Some(n) => run_fields(c, &at, o.as_object().unwrap(), n),
                        None => c.miss(&at, format!("no run for {key}")),
                    }
                }
            }
            "sweep-points.jsonl" => {
                for o in lines_of(&p) {
                    c.eq(
                        &at,
                        "point",
                        &o["point"],
                        first(&o["seed"]).and_then(|r| r.get("point")),
                    );
                }
            }
            "report.json" => report_json(c, &at, &json_of(&p).unwrap(), &dv),
            "errors.json" => error_kinds(c, &at, &json_of(&p).unwrap(), &dv),
            "properties.txt" => {
                c.checked += 1;
                let made = doc.properties.as_ref().map(render_properties);
                if made.as_deref() != read(&p).as_deref() {
                    c.miss(
                        &at,
                        "the property table made from exploration.json differs".into(),
                    );
                }
            }
            "shrink.jsonl" => {
                let cand: Vec<&Value> = runs
                    .iter()
                    .filter(|r| {
                        ["shrink", "error-shrink", "minimal", "error-minimal"]
                            .contains(&r["role"].as_str().unwrap_or(""))
                    })
                    .collect();
                let mut i = 0;
                for (ln, o) in lines_of(&p).iter().enumerate() {
                    let at = format!("{at}:{}", ln + 1);
                    let (want_roles, sched): (&[&str], &Value) =
                        match (o.get("shrink"), o.get("stable")) {
                            (Some(s), _) => (&["shrink", "error-shrink"], s),
                            (_, Some(s)) => (&["minimal"], s),
                            _ => {
                                c.unmapped(&at, &o.to_string());
                                continue;
                            }
                        };
                    c.checked += 1;
                    // the next run of the chain with this schedule; only confirmations may lie
                    // between (the old file did not list them)
                    let mut found = false;
                    while i < cand.len() {
                        let r = cand[i];
                        i += 1;
                        let role = r["role"].as_str().unwrap_or("");
                        if want_roles.contains(&role) && r["schedule"] == *sched {
                            c.eq(&at, "verdict", &o["verdict"], get(r, "verdict.text"));
                            found = true;
                            break;
                        }
                        if !["minimal", "error-minimal"].contains(&role) {
                            break;
                        }
                    }
                    if !found {
                        c.miss(&at, format!("shrink run {sched} in order"));
                    }
                }
            }
            "fc-traces.jsonl" => {
                for o in lines_of(&p) {
                    let tag = o["tag"].as_str().unwrap_or_default();
                    let Some(n) = by_tag.get(tag).filter(|r| r["role"] == "trace") else {
                        c.miss(&at, format!("trace run {tag}"));
                        continue;
                    };
                    for (k, v) in o.as_object().unwrap() {
                        let got = match k.as_str() {
                            "tag" => n.get("tag"),
                            "schedule" => n.get("schedule"),
                            "pair" | "dir" | "match" | "step" | "count" | "reused" => {
                                get(n, &format!("trace.{k}"))
                            }
                            "verdict" => get(n, "verdict.text"),
                            "fp" => get(n, "fingerprints.fp"),
                            "secs" => n.get("secs"),
                            "console" => get(n, "artifacts.consoles.0"),
                            _ => {
                                c.unmapped(&at, k);
                                continue;
                            }
                        };
                        c.eq(&at, k, v, got);
                    }
                }
            }
            "fc-adaptive.json" => {
                let o = json_of(&p).unwrap();
                for (k, v) in o.as_object().unwrap() {
                    let n = get(&dv, &format!("freeze_sweep.{k}"));
                    if k == "summary" {
                        for (kk, vv) in v.as_object().unwrap() {
                            c.eq(&at, kk, vv, n.and_then(|x| x.get(kk)));
                        }
                    } else if k == "rounds" {
                        // a round's wall time (secs) is not a function of the schedules
                        let strip = |x: Option<&Value>| {
                            let mut x = x.cloned().unwrap_or(Value::Null);
                            for r in x.as_array_mut().into_iter().flatten() {
                                if let Some(o) = r.as_object_mut() {
                                    o.remove("secs");
                                }
                            }
                            x
                        };
                        c.eq(&at, k, &strip(Some(v)), Some(&strip(n)));
                    } else {
                        c.eq(&at, k, v, n);
                    }
                }
            }
            "report.md" => {
                c.checked += 2;
                let made = hydo_explore::md::report(&doc);
                if read(&new.join("report.md")).as_deref() != Some(made.as_str()) {
                    c.miss(
                        &at,
                        "report.md is not the report made from exploration.json".into(),
                    );
                }
                if norm(&read(&p).unwrap()) != norm(&made) {
                    c.miss(
                        &at,
                        "report.md differs from the old one (beyond wall times)".into(),
                    );
                }
            }
            "consoles" => consoles(c, &p, &by_tag),
            "points.txt" | "consoles-merged" | "hydo_assert_report" => {}
            n if n.starts_with("worker-") && n.ends_with(".err") => {}
            _ => c.unmapped(&at, &format!("file {name}")),
        }
    }
    // the printed summary (`stdout` next to each directory): the same line, made from the model
    let so = old.parent().unwrap().join("stdout");
    if let Some(t) = read(&so) {
        c.checked += 1;
        c.files += 1;
        // the explorer's JSON line (`hydo explore` prints a line about report.md after it)
        let json_line = |t: &str| {
            t.lines()
                .rfind(|l| l.starts_with('{'))
                .unwrap_or("")
                .to_string()
        };
        let last = json_line(&t);
        let now = json_line(&read(&new.parent().unwrap().join("stdout")).unwrap_or_default());
        let made = hydo_explore::md::summary_line(&doc);
        if norm(&last) != norm(&now) || (doc.mode != "schedule" && norm(&now) != norm(&made)) {
            c.miss(
                &so.display().to_string(),
                format!("the printed summary {last}"),
            );
        }
    }
}

/// A shrink candidate the shrink did not use: a later candidate of a round whose earlier
/// candidate kept the failure. It finished after its round was decided (whether it ran at all
/// depends on timing), so the stream does not list it; the old explorer wrote its error-log
/// analysis next to its console, and nothing else.
fn unused_candidate(tag: &str, by_tag: &HashMap<String, &Value>) -> bool {
    let mut parts = tag.rsplitn(3, '-');
    let (Some(i), Some(rnd), Some(prefix)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (Ok(i), Ok(_)) = (i.parse::<usize>(), rnd.parse::<usize>()) else {
        return false;
    };
    (0..i).any(|j| {
        by_tag
            .get(&format!("{prefix}-{rnd}-{j}"))
            .is_some_and(|r| r["shrink"]["kept"] == true)
    })
}

fn consoles(c: &mut Check, d: &Path, by_tag: &HashMap<String, &Value>) {
    for e in std::fs::read_dir(d).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        let at = p.display().to_string();
        if let Some(tag) = name.strip_suffix(".errors.json") {
            c.files += 1;
            let Some(n) = by_tag.get(tag) else {
                if unused_candidate(tag, by_tag) {
                    c.unused += 1;
                } else {
                    c.miss(&at, format!("no run {tag}"));
                }
                continue;
            };
            let o = json_of(&p).unwrap();
            errlog_context(c, &at, &o, n, &["steps", "boots", "truncated"]);
            let mut kinds: Vec<(Value, bool)> = Vec::new();
            for (list, is_new) in [("new", true), ("known", false)] {
                for k in o[list].as_array().into_iter().flatten() {
                    kinds.push((k.clone(), is_new));
                }
            }
            errlog_kinds(c, &at, &kinds, n);
        } else if let Some(tag) = name.strip_suffix(".checks.log") {
            c.files += 1;
            match by_tag.get(tag) {
                Some(n) => checks_log(c, &at, &read(&p).unwrap(), n),
                None if unused_candidate(tag, by_tag) => c.unused += 1,
                None => c.miss(&at, format!("no run {tag}")),
            }
        } else if let Some(tag) = name.strip_suffix(".checks.txt") {
            c.files += 1;
            match by_tag.get(tag) {
                Some(n) => checks_txt(c, &at, &read(&p).unwrap(), n),
                None if unused_candidate(tag, by_tag) => c.unused += 1,
                None => c.miss(&at, format!("no run {tag}")),
            }
        }
    }
}

fn report_json(c: &mut Check, at: &str, o: &Value, dv: &Value) {
    let branch = dv["mode"] == "branch";
    for (k, v) in o.as_object().unwrap() {
        match k.as_str() {
            "summary" => {
                for (sk, sv) in v.as_object().unwrap() {
                    let path = match sk.as_str() {
                        "strategy" => "config.strategy".to_string(),
                        "image" => "image.path".into(),
                        "workers" => "engine.workers".into(),
                        "ram_mib" | "vcpus" | "cpu" | "guest_kernel" => format!("engine.{sk}"),
                        "stage1_points" | "stage2_points" | "trace_runs" | "stage1_failures"
                        | "stage2_failures" | "stage1_wall_s" | "stage2_wall_s" | "fc_window"
                        | "fc_links" | "fc_centres" => format!("freeze_sweep.summary.{sk}"),
                        "schedules"
                        | "branch_points"
                        | "runs"
                        | "variants_per_point"
                        | "tjpm"
                        | "sweep_wall_s"
                        | "runs_per_hour"
                        | "mean_run_s"
                        | "worker_boot_s"
                        | "total_wall_s"
                        | "failures"
                        | "reproduced"
                        | "reproduced_standalone"
                        | "timing_only_failures"
                        | "repro_runs"
                        | "quiet_wait_s"
                        | "cache_hits"
                        | "error_kinds_new"
                        | "runs_with_new_error_kinds" => {
                            format!("summary.{sk}")
                        }
                        _ => {
                            c.unmapped(&format!("{at} summary"), sk);
                            continue;
                        }
                    };
                    c.eq(&format!("{at} summary"), sk, sv, get(dv, &path));
                }
            }
            "failures" => {
                let nf = dv["failures"].as_array().cloned().unwrap_or_default();
                for (i, f) in v.as_array().unwrap().iter().enumerate() {
                    let at = format!("{at} failures[{i}]");
                    let key = if branch { &f["tag"] } else { &f["seed"] };
                    let Some(n) = nf.iter().find(|x| {
                        if branch {
                            x["run"]["tag"] == *key
                        } else {
                            x["run"]["seed"] == *key
                        }
                    }) else {
                        c.miss(&at, format!("failure {key}"));
                        continue;
                    };
                    run_fields(c, &at, f.as_object().unwrap(), &n["run"]);
                    for (fk, fv) in f.as_object().unwrap() {
                        let got = match fk.as_str() {
                            "repro_fp" => get(n, "reproductions.0.fp"),
                            "repro_verdict" => get(n, "reproductions.0.verdict"),
                            "reproduced" => n.get("reproduced"),
                            "timing_only" => n.get("timing_only"),
                            k if k.starts_with("repro") && k.ends_with("_fp") => {
                                let kk: usize = k[5..k.len() - 3].parse().unwrap_or(0);
                                get(n, &format!("reproductions.{}.fp", kk.saturating_sub(1)))
                            }
                            _ => continue,
                        };
                        c.eq(&at, fk, fv, got);
                    }
                }
            }
            "shrunk" => {
                for (i, s) in v.as_array().unwrap().iter().enumerate() {
                    let at = format!("{at} shrunk[{i}]");
                    let n = &dv["shrinks"][i];
                    for (sk, sv) in s.as_object().unwrap() {
                        let got = match sk.as_str() {
                            "fp_a" => get(n, "confirmations.0.fp"),
                            "fp_b" => get(n, "confirmations.1.fp"),
                            "class" | "signature" | "from_seed" | "minimal" | "verdict"
                            | "tries" | "min_class" | "min_verdict" | "composition"
                            | "commands" | "contents" | "stable" | "stable_schedule"
                            | "stable_note" => n.get(sk.as_str()),
                            _ => {
                                c.unmapped(&at, sk);
                                continue;
                            }
                        };
                        c.eq(&at, sk, sv, got);
                    }
                }
            }
            "errors" => error_kinds(c, &format!("{at} errors"), v, dv),
            _ => c.unmapped(at, k),
        }
    }
}

fn error_kinds(c: &mut Check, at: &str, o: &Value, dv: &Value) {
    let ek = &dv["error_kinds"];
    for (k, v) in o.as_object().unwrap() {
        match k.as_str() {
            "known_file" | "known_sha256" | "unknown" => c.eq(at, k, v, ek.get(k.as_str())),
            "kinds" => {
                for (i, x) in v.as_array().unwrap().iter().enumerate() {
                    let at = format!("{at} kinds[{i}]");
                    let n = &ek["kinds"][i];
                    for (xk, xv) in x.as_object().unwrap() {
                        let got = match xk.as_str() {
                            "sig" => n.get("signature"),
                            "svc" => n.get("service"),
                            "src" => n.get("source"),
                            "tpl" => n.get("template"),
                            "ex" => n.get("example"),
                            "runs" | "new_runs" | "seeds" | "first_seed" | "first_schedule"
                            | "first_at" | "lines" | "reason" | "new" => n.get(xk.as_str()),
                            "first_new_seed" => get(n, "first_new.seed"),
                            "first_new_schedule" => get(n, "first_new.schedule"),
                            "first_new_at" => get(n, "first_new.at"),
                            "first_new_ex" => get(n, "first_new.example"),
                            "shrunk" => {
                                for (sk, sv) in xv.as_object().into_iter().flatten() {
                                    let g = match sk.as_str() {
                                        "minimal" | "tries" => get(n, &format!("shrink.{sk}")),
                                        "fp_a" => get(n, "shrink.confirmations.0.fp"),
                                        "fp_b" => get(n, "shrink.confirmations.1.fp"),
                                        "verdict" => get(n, "shrink.confirmations.0.verdict"),
                                        "present_a" => get(n, "shrink.confirmations.0.present"),
                                        "present_b" => get(n, "shrink.confirmations.1.present"),
                                        _ => {
                                            c.unmapped(&at, sk);
                                            continue;
                                        }
                                    };
                                    c.eq(&at, sk, sv, g);
                                }
                                if xv.is_null() {
                                    c.eq(&at, "shrunk", xv, n.get("shrink"));
                                }
                                continue;
                            }
                            _ => {
                                c.unmapped(&at, xk);
                                continue;
                            }
                        };
                        c.eq(&at, xk, xv, got);
                    }
                }
            }
            _ => c.unmapped(at, k),
        }
    }
}

// ---------------------------------------------------------------- --convert (test fixtures)

fn sv(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

/// An old run record (runs.jsonl / report.json failure / run.json) as a run of the results
/// format, as far as the old record says (findings: the error kinds' signatures and the failed
/// properties' ids).
fn conv_run(o: &Value) -> hydo_results::Run {
    let text = sv(&o["verdict"]).unwrap_or_default();
    let mut r = hydo_results::Run::new(
        &sv(&o["schedule"]).unwrap_or_default(),
        hydo_results::Verdict {
            kind: hydo_explore::results::kind_of(&text),
            text,
            guest: sv(&o["guest_verdict"]),
            before_error_logs: sv(&o["pre_errlog_verdict"]),
        },
        &sv(&o["fp"]).unwrap_or_default(),
    );
    r.fingerprints.wfp = sv(&o["wfp"]);
    r.seed = o["seed"].as_i64();
    r.tag = sv(&o["tag"]);
    r.class = sv(&o["class"]);
    r.secs = o["secs"].as_f64();
    r.worker = o["worker"].as_u64();
    r.role = Some(hydo_results::role::FIRST.into());
    r.findings.extend(hydo_explore::results::invariant_finding(
        &hydo_explore::results::guest_verdict(o.as_object().unwrap()),
    ));
    for id in o["assert_failures"].as_array().into_iter().flatten() {
        r.findings.push(hydo_results::Finding {
            kind: "assertion".into(),
            id: sv(id).unwrap_or_default(),
            status: "violation".into(),
            ..Default::default()
        });
    }
    for id in o["errors_new"].as_array().into_iter().flatten() {
        r.findings.push(hydo_results::Finding {
            kind: "error-log".into(),
            id: sv(id).unwrap_or_default(),
            status: "new".into(),
            ..Default::default()
        });
    }
    r
}

fn conv_kinds(e: &Value) -> hydo_results::ErrorKinds {
    hydo_results::ErrorKinds {
        known_file: sv(&e["known_file"]),
        known_sha256: sv(&e["known_sha256"]),
        unknown: sv(&e["unknown"]).unwrap_or_else(|| "report".into()),
        kinds: e["kinds"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|k| {
                let strs = |v: &Value| -> Vec<String> {
                    v.as_array().into_iter().flatten().filter_map(sv).collect()
                };
                let sh = &k["shrunk"];
                hydo_results::ErrorKind {
                    signature: sv(&k["sig"]).unwrap_or_default(),
                    service: sv(&k["svc"]).unwrap_or_default(),
                    source: sv(&k["src"]).unwrap_or_default(),
                    template: sv(&k["tpl"]).unwrap_or_default(),
                    example: sv(&k["ex"]).unwrap_or_default(),
                    new: k["new"] == true,
                    runs: k["runs"].as_u64().unwrap_or(0),
                    new_runs: k["new_runs"].as_u64().unwrap_or(0),
                    seeds: k["seeds"].as_array().cloned().unwrap_or_default(),
                    first_seed: k["first_seed"].clone(),
                    first_schedule: sv(&k["first_schedule"]).unwrap_or_default(),
                    first_at: strs(&k["first_at"]),
                    lines: k["lines"].as_i64().unwrap_or(0),
                    first_new: k.get("first_new_seed").map(|s| hydo_results::FirstNew {
                        seed: s.clone(),
                        schedule: sv(&k["first_new_schedule"]).unwrap_or_default(),
                        at: strs(&k["first_new_at"]),
                        example: sv(&k["first_new_ex"]).unwrap_or_default(),
                    }),
                    reason: k.get("reason").cloned(),
                    shrink: (!sh.is_null()).then(|| hydo_results::ErrorShrink {
                        minimal: sv(&sh["minimal"]).unwrap_or_default(),
                        tries: sh["tries"].as_u64().unwrap_or(0),
                        confirmations: ["a", "b"]
                            .iter()
                            .map(|x| hydo_results::Reproduction {
                                tag: None,
                                fp: sv(&sh[format!("fp_{x}")]),
                                verdict: if *x == "a" { sv(&sh["verdict"]) } else { None },
                                present: sh[format!("present_{x}")].as_bool(),
                            })
                            .collect(),
                        identical: sh["fp_a"] == sh["fp_b"],
                        repro_command: None,
                    }),
                }
            })
            .collect(),
    }
}

/// Old files -> the results format (how the CLI test fixtures were moved to it in M115). What
/// the old files did not hold stays absent; legacy_check of the old directory against the result
/// finds every old value in it.
fn convert(old: &Path, new: &Path) {
    std::fs::create_dir_all(new).unwrap();
    if let Some(rep) = json_of(&old.join("report.json")) {
        let s = &rep["summary"];
        let strategy = sv(&s["strategy"]).unwrap_or_default();
        let mut doc = hydo_results::Exploration {
            mode: if strategy.starts_with("sweep:") {
                "sweep"
            } else {
                "seeds"
            }
            .into(),
            config: hydo_results::Config {
                sweep: strategy.strip_prefix("sweep:").map(String::from),
                strategy,
                ..Default::default()
            },
            image: hydo_results::Image {
                path: sv(&s["image"]).unwrap_or_default(),
                ..Default::default()
            },
            engine: hydo_results::Engine {
                workers: s["workers"].as_u64(),
                ram_mib: Some(s["ram_mib"].as_u64().unwrap_or(1024)),
                vcpus: Some(s["vcpus"].as_u64().unwrap_or(1)),
                cpu: Some(sv(&s["cpu"]).unwrap_or_else(|| "v2".into())),
                guest_kernel: sv(&s["guest_kernel"]),
                ..Default::default()
            },
            summary: serde_json::from_value(s.clone()).unwrap(),
            artifacts: hydo_results::ExplorationArtifacts {
                events: hydo_results::EVENTS.into(),
                report: Some("report.md".into()),
                consoles: Some("consoles".into()),
            },
            ..Default::default()
        };
        for f in rep["failures"].as_array().into_iter().flatten() {
            let mut reps = vec![hydo_results::Reproduction {
                tag: None,
                fp: sv(&f["repro_fp"]),
                verdict: sv(&f["repro_verdict"]),
                present: None,
            }];
            for k in 2.. {
                match f.get(format!("repro{k}_fp")) {
                    Some(x) => reps.push(hydo_results::Reproduction {
                        fp: sv(x),
                        ..Default::default()
                    }),
                    None => break,
                }
            }
            doc.failures.push(hydo_results::Failure {
                signature: hydo_explore::report::sig_key(f["verdict"].as_str().unwrap_or("")),
                reproduced: f["reproduced"] == true,
                reproductions: reps,
                timing_only: f["timing_only"].as_bool(),
                run: conv_run(f),
            });
        }
        for x in rep["shrunk"].as_array().into_iter().flatten() {
            let mut sh: hydo_results::Shrink = serde_json::from_value(json!({
                "v": 1, "type": "shrink", "class": x["class"], "signature": x["signature"],
                "from_seed": x["from_seed"], "minimal": x["minimal"], "verdict": x["verdict"],
                "tries": x["tries"]}))
            .unwrap();
            sh.min_class = sv(&x["min_class"]);
            sh.min_verdict = sv(&x["min_verdict"]);
            sh.confirmations = ["fp_a", "fp_b"]
                .iter()
                .map(|k| hydo_results::Reproduction {
                    fp: sv(&x[*k]),
                    ..Default::default()
                })
                .collect();
            sh.identical = x["fp_a"] == x["fp_b"];
            sh.composition = sv(&x["composition"]);
            sh.commands = x["commands"]
                .as_array()
                .map(|a| a.iter().filter_map(sv).collect());
            sh.contents = x["contents"].as_object().cloned();
            sh.stable = sv(&x["stable"]);
            sh.stable_schedule = sv(&x["stable_schedule"]);
            sh.stable_note = sv(&x["stable_note"]);
            doc.shrinks.push(sh);
        }
        let errs = json_of(&old.join("errors.json")).or_else(|| rep.get("errors").cloned());
        doc.error_kinds = errs.as_ref().map(conv_kinds);
        hydo_results::write_document(&new.join(hydo_results::EXPLORATION_DOC), &doc).unwrap();
        if let Some(md) = read(&old.join("report.md")) {
            std::fs::write(new.join("report.md"), md).unwrap();
        }
        return;
    }
    let rj = json_of(&old.join("run.json"));
    let errs = json_of(&old.join("one.errors.json"));
    let mut r = rj.as_ref().map(conv_run).unwrap_or_else(|| {
        hydo_results::Run::new(
            "",
            hydo_results::Verdict {
                kind: "error".into(),
                text: "(converted from one.errors.json, which has no verdict)".into(),
                ..Default::default()
            },
            "",
        )
    });
    r.role = None;
    if let Some(j) = &rj {
        r.image = Some(hydo_results::Image {
            path: sv(&j["image"]).unwrap_or_default(),
            sha256: sv(&j["image_sha256"]),
            ..hydo_results::Image::default()
        });
        r.result_line = sv(&j["line"]);
        r.engine = Some(hydo_results::Engine {
            args: j["engine_args"]
                .as_array()
                .map(|a| a.iter().filter_map(sv).collect()),
            ..Default::default()
        });
    }
    if let Some(e) = &errs {
        r.findings.retain(|f| f.kind != "error-log");
        for k in e["kinds"].as_array().into_iter().flatten() {
            let mut d = Map::new();
            for (from, to) in [
                ("svc", "service"),
                ("src", "source"),
                ("tpl", "template"),
                ("ex", "example"),
                ("n", "lines"),
                ("at", "at"),
                ("entry", "known_entry"),
                ("reason", "reason"),
            ] {
                if let Some(v) = k.get(from) {
                    d.insert(to.into(), v.clone());
                }
            }
            r.findings.push(hydo_results::Finding {
                kind: "error-log".into(),
                id: sv(&k["sig"]).unwrap_or_default(),
                status: if k["new"] == true { "new" } else { "known" }.into(),
                message: format!(
                    "[{}] {}",
                    sv(&k["svc"]).unwrap_or_default(),
                    sv(&k["tpl"]).unwrap_or_default()
                ),
                details: d,
            });
        }
        r.error_logs = Some(hydo_results::ErrorLogs {
            known_file: sv(&e["known_file"]),
            known_sha256: sv(&e["known_sha256"]),
            unknown: sv(&e["unknown"]),
            steps: e["steps"].as_i64(),
            boots: e["boots"].as_array().cloned().unwrap_or_default(),
            truncated: false,
        });
    }
    hydo_results::write_document(&new.join(hydo_results::RUN_DOC), &r).unwrap();
}

fn main() {
    let mut a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some("--convert") && a.len() == 3 {
        convert(Path::new(&a[1]), Path::new(&a[2]));
        return;
    }
    // --base OLD NEW [SUB]: every OLD/<name>/<SUB> against NEW/<name>/<SUB> (SUB default "out")
    if a.first().map(String::as_str) == Some("--base") && a.len() >= 3 {
        let sub = a.get(3).cloned().unwrap_or_else(|| "out".into());
        let (ob, nb) = (PathBuf::from(&a[1]), PathBuf::from(&a[2]));
        let mut names: Vec<String> = std::fs::read_dir(&ob)
            .unwrap()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        a = names
            .iter()
            .flat_map(|n| {
                [
                    ob.join(n).join(&sub).display().to_string(),
                    nb.join(n).join(&sub).display().to_string(),
                ]
            })
            .collect();
    }
    if a.is_empty() || !a.len().is_multiple_of(2) {
        eprintln!("usage: legacy_check <old dir> <new dir> [<old dir> <new dir>]...");
        std::process::exit(2);
    }
    let mut total = Check::default();
    for pair in a.chunks(2) {
        let (old, new) = (Path::new(&pair[0]), Path::new(&pair[1]));
        let mut c = Check::default();
        if old.join("run.json").is_file() || old.join("one.errors.json").is_file() {
            check_run(&mut c, old, new);
        } else {
            check_exploration(&mut c, old, new);
        }
        println!(
            "{}: {} values from {} files, {} missing or unmapped ({} files of unused shrink candidates)",
            old.display(),
            c.checked,
            c.files,
            c.bad.len(),
            c.unused
        );
        for b in c.bad.iter().take(40) {
            println!("  {b}");
        }
        total.checked += c.checked;
        total.files += c.files;
        total.unused += c.unused;
        total.bad.extend(c.bad);
    }
    println!(
        "legacy_check: {} pairs, {} values from {} old files, {} missing or unmapped ({} files of shrink candidates that finished after their round was decided, not recorded)",
        a.len() / 2,
        total.checked,
        total.files,
        total.bad.len(),
        total.unused
    );
    std::process::exit(if total.bad.is_empty() { 0 } else { 1 });
}
