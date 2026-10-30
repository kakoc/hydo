//! hydo-errors against hydo/onboard/hydo_errors.py (the reference). Every case tests/py_ref.py
//! prints is run here too and must give the same JSON (or the same refusal):
//! synthetic consoles against known lists and schedules, every load_known message, fault
//! windows, accept, for_run_dir, and with HYDO_ERRLOG_CORPUS=<dir> (a copy of the bare-metal
//! machine's /var/tmp/m94 with known/redis-errors.yaml and known/stack.yaml, the lists those runs
//! used) every recorded console and exploration of M94. `recorded_files` then also compares the
//! Rust output with the files M94's runs wrote (one.errors.json, consoles/<tag>.errors.json,
//! errors.json and report.md's section), byte for byte.
//!
//! The reference runs as `python3 tests/py_ref.py` (HYDO_PYTHON to use another interpreter).

use hydo_errors as he;
use hydo_manifest::py::{json_dumps, json_dumps_indent};
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-errors-eq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn srcs(v: &Value) -> Vec<(String, String)> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let n = p[0].as_str().unwrap().to_string();
            let t = he::read_text_lossy(Path::new(p[1].as_str().unwrap())).unwrap();
            (n, t)
        })
        .collect()
}

fn known(a: &Value) -> Result<he::KnownList, he::ErrorsError> {
    he::load_known(a["known"].as_str())
}

fn view(k: &he::KnownList) -> Value {
    json!({"path": k.path, "sha256": k.sha256, "unknown": k.unknown,
           "known": k.known.iter().map(|e| Value::Object(e.fields.clone())).collect::<Vec<_>>()})
}

fn reses() -> Vec<Map<String, Value>> {
    [
        json!({"verdict": "HYDO-ORACLE: OK invariants=1 held"}),
        json!({"verdict": "HYDO-ORACLE: VIOLATION [x] y"}),
        json!({"fp": "0123456789abcdef"}),
    ]
    .into_iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect()
}

type Out = Result<Value, (&'static str, String)>;

fn ee(e: he::ErrorsError) -> (&'static str, String) {
    ("ErrorsError", e.0)
}

fn run(op: &str, a: &Value, work: &Path, n: usize) -> (Out, Option<PathBuf>) {
    let mut dir = None;
    let out = (|| -> Out {
        Ok(match op {
            "analyse" => {
                let mut k = known(a).map_err(ee)?;
                if let Some(u) = a["unknown"].as_str() {
                    k.unknown = u.to_string();
                }
                let an = he::analyse(&srcs(&a["srcs"]), a["schedule"].as_str().unwrap(), &k);
                let mut o = Map::new();
                o.insert(
                    "an".into(),
                    an.as_ref().map(|x| x.to_json()).unwrap_or(Value::Null),
                );
                o.insert("summary".into(), json!(he::summary_lines(an.as_ref(), 12)));
                let applied: Vec<Value> = reses()
                    .into_iter()
                    .map(|mut r| {
                        he::apply(&mut r, an.as_ref(), &k);
                        Value::Object(r)
                    })
                    .collect();
                o.insert("apply".into(), Value::Array(applied));
                if let Some(an) = &an {
                    let p = work.join(format!("rs-run-{n}.json"));
                    he::write_run_json(&p, an, &k).unwrap();
                    o.insert(
                        "run_json".into(),
                        json!(std::fs::read_to_string(&p).unwrap()),
                    );
                    let _ = std::fs::remove_file(&p);
                }
                Value::Object(o)
            }
            "load_known" => view(&he::load_known(a["path"].as_str()).map_err(ee)?),
            "fault_windows" => json!(he::fault_windows(
                a["schedule"].as_str().unwrap(),
                a["steps"].as_i64().unwrap()
            )),
            "label_rank" => json!(he::label_rank(a["lab"].as_str().unwrap(), 20)),
            "explore" => {
                let k = known(a).map_err(ee)?;
                let runs: Vec<(Value, String, Option<he::Analysis>)> = a["runs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|r| {
                        let sched = r[1].as_str().unwrap().to_string();
                        let an = he::analyse(&srcs(&r[2]), &sched, &k);
                        (r[0].clone(), sched, an)
                    })
                    .collect();
                let agg = he::collect(&runs);
                let md = he::report_md(&agg, &k, a["shrunk"].as_object().unwrap(), &|s| {
                    format!("`hydo run \"{s}\"`")
                });
                json!({"agg": agg.iter().map(|x| Value::Object(x.to_json())).collect::<Vec<_>>(), "md": md})
            }
            "accept" => {
                let d = work.join(format!("rs-accept-{n}"));
                std::fs::create_dir_all(&d).unwrap();
                dir = Some(d.clone());
                let p = d.join("sub/known-errors.yaml");
                if let Some(b) = a["before"].as_str() {
                    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                    std::fs::write(&p, b).unwrap();
                }
                let strs = |k: &str| -> Vec<String> {
                    a[k].as_array()
                        .unwrap()
                        .iter()
                        .map(|x| x.as_str().unwrap().to_string())
                        .collect()
                };
                let add = he::accept(
                    &p.display().to_string(),
                    a["kinds"].as_array().unwrap(),
                    a["reason"].as_str(),
                    a["by"].as_str().unwrap(),
                    &strs("with"),
                    &strs("during"),
                    Some("2026-10-30"),
                )
                .map_err(ee)?;
                json!({"add": add, "text": std::fs::read_to_string(&p).unwrap()})
            }
            "for_run_dir" => {
                let d = work.join(format!("rs-run-dir-{n}"));
                std::fs::create_dir_all(&d).unwrap();
                dir = Some(d.clone());
                for (name, text) in a["files"].as_object().unwrap() {
                    std::fs::write(d.join(name), text.as_str().unwrap()).unwrap();
                }
                let mut printed: Vec<String> = Vec::new();
                let res = he::for_run_dir(
                    &d,
                    a["schedule"].as_str().unwrap(),
                    a["res"].as_object().unwrap().clone(),
                    a["known"].as_str(),
                    &mut |l| printed.push(l.to_string()),
                );
                let p = d.join("one.errors.json");
                json!({"res": res, "printed": printed,
                       "one_errors": std::fs::read_to_string(&p).ok()})
            }
            other => panic!("unknown op {other}"),
        })
    })();
    (out, dir)
}

#[test]
fn errors_match_python() {
    let work = scratch("work");
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/py_ref.py");
    let mut cmd = Command::new(python());
    cmd.arg(&script).arg("--work").arg(&work);
    let corpus = std::env::var("HYDO_ERRLOG_CORPUS").ok();
    if let Some(c) = &corpus {
        cmd.arg("--corpus").arg(c);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 is needed to run the reference (set HYDO_PYTHON)");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut bad = Vec::new();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut summary = Value::Null;
    for (n, line) in reader.lines().enumerate() {
        let case: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let op = case["op"].as_str().unwrap().to_string();
        if op == "summary" {
            summary = case["out"]["ok"].clone();
            continue;
        }
        let (got, dir) = run(&op, &case["args"], &work, n);
        let norm = |s: String| match &dir {
            Some(d) => s.replace(
                &json_dumps(&json!(d.display().to_string()))[1..]
                    .trim_end_matches('"')
                    .to_string(),
                "<dir>",
            ),
            None => s,
        };
        let want = &case["out"];
        let same = match (&got, want.get("ok"), want.get("err")) {
            (Ok(v), Some(w), _) => norm(json_dumps(v)) == json_dumps(w),
            // a .json list that is not JSON: both refuse it; the decoder's own words differ
            // (serde_json's "key must be a string at line 1 column 2" for Python's "Expecting
            // property name enclosed in double quotes: line 1 column 2 (char 1)")
            (Err((k, _)), None, Some(e))
                if op == "load_known"
                    && case["args"]["path"]
                        .as_str()
                        .is_some_and(|p| p.ends_with(".json"))
                    && want["msg"]
                        .as_str()
                        .is_some_and(|m| m.contains(" line 1 column")) =>
            {
                e == k
            }
            (Err((k, m)), None, Some(e)) => {
                e == k && json_dumps(&json!(norm(m.clone()))) == json_dumps(&want["msg"])
            }
            _ => false,
        };
        if !same && bad.len() < 8 {
            bad.push(format!(
                "{op} {}\n  python: {}\n  rust:   {}",
                json_dumps(&case["args"])
                    .chars()
                    .take(400)
                    .collect::<String>(),
                json_dumps(want).chars().take(3000).collect::<String>(),
                match &got {
                    Ok(v) => norm(json_dumps(v)).chars().take(3000).collect::<String>(),
                    Err(e) => format!("{e:?}"),
                }
            ));
        }
        *counts.entry(op).or_default() += 1;
    }
    assert!(child.wait().unwrap().success(), "py_ref.py failed");
    let _ = std::fs::remove_dir_all(&work);
    assert!(bad.is_empty(), "differences:\n{}", bad.join("\n"));
    let total: usize = counts.values().sum();
    assert_eq!(summary["ops"].as_u64().unwrap() as usize, total);
    if corpus.is_some() {
        assert!(summary["corpus_runs"].as_u64().unwrap() > 0, "{summary}");
    } else {
        eprintln!("NOTE: HYDO_ERRLOG_CORPUS is not set: the M94 consoles were not compared");
    }
    eprintln!(
        "errors equivalence: {total} cases identical to hydo_errors.py: {counts:?}; {summary}"
    );
}

/// The files M94's runs wrote, recomputed in Rust from their consoles and the lists they used.
#[test]
fn recorded_files() {
    let Ok(root) = std::env::var("HYDO_ERRLOG_CORPUS") else {
        eprintln!(
            "SKIPPED: set HYDO_ERRLOG_CORPUS to a copy of the M94 results (see the module doc)"
        );
        return;
    };
    let root = PathBuf::from(root);
    let list_for = |recorded: &Value| -> Option<String> {
        let f = recorded.as_str()?;
        Some(
            if f.ends_with("redis-errors/hydo/known-errors.yaml") || f == "hydo/known-errors.yaml" {
                root.join("known/redis-errors.yaml").display().to_string()
            } else if f.ends_with("stack/known-errors.yaml") {
                root.join("known/stack.yaml").display().to_string()
            } else if f.ends_with("redis-final/fail.yaml") {
                root.join("redis-final/fail.yaml").display().to_string()
            } else {
                panic!("unknown recorded list {f}")
            },
        )
    };
    let (mut nrun, mut ntag, mut nexp, mut nmd) = (0, 0, 0, 0);
    let mut dirs = vec![root.clone()];
    while let Some(d) = dirs.pop() {
        let mut ents: Vec<PathBuf> = std::fs::read_dir(&d)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        ents.sort();
        dirs.extend(ents.iter().filter(|p| p.is_dir()).cloned());
        // hydo run: one.errors.json and the verdict in run.json
        let rec = d.join("one.errors.json");
        if rec.is_file() && d.join("one.log").is_file() {
            let want = std::fs::read_to_string(&rec).unwrap();
            let wj: Value = serde_json::from_str(&want).unwrap();
            let run: Value = std::fs::read_to_string(d.join("run.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let sched = run["schedule"].as_str().unwrap_or("disk=none | run");
            let mut k = he::load_known(list_for(&wj["known_file"]).as_deref()).unwrap();
            k.path = wj["known_file"].as_str().map(str::to_string);
            let out = root.join("..").join(format!("rec-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            for e in std::fs::read_dir(&d).unwrap().filter_map(Result::ok) {
                if e.path().is_file() && e.file_name().to_string_lossy().ends_with(".log") {
                    std::fs::copy(e.path(), out.join(e.file_name())).unwrap();
                }
            }
            let srcs = he::run_files(&out.join("one"));
            let an = he::analyse(&srcs, sched, &k).unwrap();
            let p = out.join("one.errors.json");
            he::write_run_json(&p, &an, &k).unwrap();
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                want,
                "{}",
                rec.display()
            );
            if run.get("errors_new").is_some() {
                let mut res = Map::new();
                res.insert(
                    "verdict".into(),
                    run.get("pre_errlog_verdict")
                        .or(run.get("verdict"))
                        .cloned()
                        .unwrap(),
                );
                he::apply(&mut res, Some(&an), &k);
                assert_eq!(res["verdict"], run["verdict"], "{}", d.display());
                assert_eq!(res["errors_new"], run["errors_new"], "{}", d.display());
                assert_eq!(res["errors_known"], run["errors_known"], "{}", d.display());
            }
            let _ = std::fs::remove_dir_all(&out);
            nrun += 1;
        }
        // an exploration: consoles/<tag>.errors.json, errors.json, report.md's section
        let cons = d.join("consoles");
        let ej = d.join("errors.json");
        if cons.is_dir() && ej.is_file() {
            let ejv: Value = serde_json::from_str(&std::fs::read_to_string(&ej).unwrap()).unwrap();
            let mut k = he::load_known(list_for(&ejv["known_file"]).as_deref()).unwrap();
            k.path = ejv["known_file"].as_str().map(str::to_string);
            let mut sched_of = std::collections::BTreeMap::new();
            for l in std::fs::read_to_string(d.join("runs.jsonl"))
                .unwrap()
                .lines()
            {
                let r: Value = serde_json::from_str(l).unwrap();
                sched_of.insert(
                    format!("seed-{}", r["seed"]),
                    (
                        r["seed"].clone(),
                        r["schedule"].as_str().unwrap().to_string(),
                        r,
                    ),
                );
            }
            let mut runs = Vec::new();
            let mut seeds: Vec<&(Value, String, Value)> = sched_of.values().collect();
            seeds.sort_by_key(|x| x.0.as_i64());
            for (seed, sched, r) in seeds {
                let tag = format!("seed-{seed}");
                let an = he::analyse(&he::run_files(&cons.join(&tag)), sched, &k);
                let rec = cons.join(format!("{tag}.errors.json"));
                if let Some(an) = &an {
                    let want = std::fs::read_to_string(&rec).unwrap();
                    assert_eq!(
                        json_dumps_indent(&an.to_json(), 1, false),
                        want,
                        "{}",
                        rec.display()
                    );
                    let mut res = Map::new();
                    res.insert("verdict".into(), json!("HYDO-ORACLE: OK x"));
                    he::apply(&mut res, Some(an), &k);
                    assert_eq!(res["errors_new"], r["errors_new"], "{tag}");
                    assert_eq!(res["errors_known"], r["errors_known"], "{tag}");
                    ntag += 1;
                } else {
                    assert!(!rec.exists());
                }
                runs.push((seed.clone(), sched.clone(), an));
            }
            let agg = he::collect(&runs);
            let mut shrunk = Map::new();
            let mut kinds = Vec::new();
            for (a, rec) in agg.iter().zip(ejv["kinds"].as_array().unwrap()) {
                let mut m = a.to_json();
                m.insert("new".into(), json!(a.new_runs > 0));
                m.insert("shrunk".into(), rec["shrunk"].clone());
                if !rec["shrunk"].is_null() {
                    shrunk.insert(a.sig.clone(), rec["shrunk"].clone());
                }
                kinds.push(Value::Object(m));
            }
            let mine = json!({"known_file": ejv["known_file"], "known_sha256": k.sha256,
                              "unknown": k.unknown, "kinds": kinds});
            assert_eq!(
                json_dumps_indent(&mine, 1, false),
                std::fs::read_to_string(&ej).unwrap(),
                "{}",
                ej.display()
            );
            if let Ok(md) = std::fs::read_to_string(d.join("report.md")) {
                nmd += 1;
                let start = md.find("## Error logs").unwrap();
                let rest = &md[start..];
                let end = rest[3..].find("\n## ").map(|i| i + 4).unwrap_or(rest.len());
                let want: Vec<&str> = rest[..end].trim_end_matches('\n').split('\n').collect();
                // the explorer's "by hand" line is the explorer's: taken from the recorded report
                let by_hand = |s: &str| -> String {
                    want.iter()
                        .find_map(|l| {
                            l.strip_prefix("- by hand: ")
                                .filter(|x| x.contains(&format!("\"{s}\"")))
                        })
                        .unwrap()
                        .to_string()
                };
                let got = he::report_md(&agg, &k, &shrunk, &by_hand);
                let got: Vec<String> = got
                    .join("\n")
                    .trim_end_matches('\n')
                    .split('\n')
                    .map(str::to_string)
                    .collect();
                assert_eq!(got, want, "{}", d.display());
            }
            nexp += 1;
        }
    }
    assert!(nrun > 0 && nexp > 0);
    eprintln!(
        "recorded M94 files: {nrun} one.errors.json (+ run.json verdicts), {ntag} consoles/<tag>.errors.json, \
         {nexp} errors.json and {nmd} report.md sections identical"
    );
}
