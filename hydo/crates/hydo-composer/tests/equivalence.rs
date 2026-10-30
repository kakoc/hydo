//! hydo-composer against hydo/onboard/hydo_composer.py (the reference): every case that
//! tests/py_ref.py prints (the M98 corpora of test_composer.py, every plan and hydo/ folder in the
//! repository with drawn compositions and regressions in every form, and the M98 folder variants)
//! is run here too and must give the same value or the same error.
//!
//! The reference runs as `python3 tests/py_ref.py --work <dir>` (set HYDO_PYTHON to use another
//! interpreter). Without an interpreter the test fails rather than passing silently.

use hydo_composer as hc;
use hydo_manifest::py::{json_dumps, CRASH_PREFIX};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-composer-eq-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

type Out = Result<Value, (&'static str, String)>;

fn werr(e: hc::WError) -> (&'static str, String) {
    match e {
        hc::WError::Invalid(m) => ("ValueError", m),
        hc::WError::Crash(m) => ("crash", m),
    }
}

fn merr(e: hc::ManifestError) -> (&'static str, String) {
    if e.message.starts_with(CRASH_PREFIX) {
        ("crash", e.message)
    } else {
        ("ValueError", e.message)
    }
}

fn entries(es: Vec<hc::Entry>) -> Value {
    Value::Array(
        es.into_iter()
            .map(|(e, r, c, s)| json!([e as i64, r as i64, c as u64, s as i64]))
            .collect(),
    )
}

fn steps(a: &Value) -> Option<i128> {
    a.get("steps").and_then(Value::as_i64).map(i128::from)
}

fn saved(a: &Value) -> Option<hc::Saved> {
    a.get("saved").and_then(Value::as_object).map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
            .collect()
    })
}

fn s<'a>(a: &'a Value, k: &str) -> &'a str {
    a[k].as_str().unwrap_or_default()
}

fn snapshot(root: &Path) -> Value {
    use std::os::unix::fs::PermissionsExt;
    fn walk(d: &Path, root: &Path, out: &mut BTreeMap<String, Value>) {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        v.sort_by_key(|e| e.file_name());
        for e in v {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().display().to_string();
            let md = std::fs::symlink_metadata(&p).unwrap();
            let mode = format!("0o{:o}", md.permissions().mode() & 0o7777);
            if md.file_type().is_symlink() {
                out.insert(
                    rel,
                    json!({"link": std::fs::read_link(&p).unwrap().display().to_string()}),
                );
            } else if md.is_dir() {
                out.insert(rel, json!({ "dir": mode }));
                walk(&p, root, out);
            } else {
                let text = String::from_utf8_lossy(&std::fs::read(&p).unwrap()).into_owned();
                out.insert(rel, json!({"mode": mode, "text": text}));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    serde_json::to_value(out).unwrap()
}

fn run(op: &str, a: &Value, work: &Path, n: usize) -> Out {
    let meta = &a["meta"];
    Ok(match op {
        "plan" => {
            let kinds: Vec<String> = a["cmds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["kind"].as_str().unwrap().to_string())
                .collect();
            let i = |k: &str| i128::from(a[k].as_i64().unwrap());
            entries(hc::plan(i("seed"), i("q"), i("steps"), &kinds).map_err(merr)?)
        }
        "render_w" => json!(hc::render_w(
            &hc::parse_seed(s(a, "w"), meta).map_err(werr)?
        )),
        "kept" => {
            let d = hc::parse_seed(s(a, "w"), meta).map_err(werr)?;
            entries(hc::kept_of(&d, steps(a).unwrap(), meta).map_err(werr)?)
        }
        "candidates" => json!(hc::candidates(s(a, "w"), meta, steps(a)).map_err(werr)?),
        "stable_w" => {
            json!(hc::stable_w(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "key_of" => json!(hc::key_of(s(a, "x"))),
        "render_resolved" => json!(hc::render_resolved(
            &hc::parse_resolved(s(a, "v")).map_err(|e| ("ValueError", e))?
        )),
        "entries_of" => {
            entries(hc::entries_of(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "describe" => json!(hc::describe(s(a, "w"), meta, steps(a)).map_err(werr)?),
        "contents_of" => {
            json!(hc::contents_of(s(a, "w"), meta, steps(a), saved(a).as_ref()).map_err(werr)?)
        }
        "expand" => {
            let mut m = a["m"].clone();
            let folder = hc::scan_folder(s(a, "dir")).map_err(merr)?;
            let mut warnings = Vec::new();
            let meta = hc::expand(&mut m, folder.as_ref(), &mut warnings).map_err(merr)?;
            json!({"m": m, "meta": meta, "warnings": warnings})
        }
        "shell_lines" => json!(hc::shell_lines(meta)),
        "test_from_meta" => hc::test_from_meta(meta),
        "stage" => {
            let folder = hc::scan_folder(s(a, "dir")).map_err(merr)?;
            let root = work.join(format!("rs-stage-{n}"));
            let onboard = crate_dir().join("../../onboard");
            hc::stage(meta, &root, &onboard, folder.as_ref()).map_err(|e| ("crash", e))?;
            let snap = snapshot(&root);
            let _ = std::fs::remove_dir_all(&root);
            snap
        }
        "regression_status" => {
            hc::status_json(&hc::regression_status(&a["r"], meta, steps(a)).map_err(merr)?)
        }
        "regression_warning" => json!(hc::regression_warning(&a["r"], meta).map_err(merr)?),
        "draw" => {
            let seq: Vec<u64> = a["seq"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap())
                .collect();
            let mut it = seq.into_iter();
            let mut next = || it.next().expect("draw took more values than Python");
            json!(hc::draw(&mut next, meta))
        }
        "with_w" => {
            let w = hc::with_w(s(a, "schedule"), s(a, "w"));
            let f = hc::fits(&w);
            json!([w, f])
        }
        "w_of" => json!(hc::w_of(s(a, "schedule"))),
        "saved_regression_text" => {
            json!(hc::saved_regression_text(&a["x"]).map_err(|e| ("crash", e))?)
        }
        other => panic!("unknown op {other}"),
    })
}

#[test]
fn composer_matches_python() {
    let work = scratch("work");
    let script = crate_dir().join("tests/py_ref.py");
    let mut child = Command::new(python())
        .arg(&script)
        .arg("--work")
        .arg(&work)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 is needed to run the reference (set HYDO_PYTHON)");
    let reader = BufReader::new(child.stdout.take().unwrap());
    let mut metas: BTreeMap<i64, Value> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut summary = Value::Null;
    let mut bad = Vec::new();
    for (n, line) in reader.lines().enumerate() {
        let case: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let op = case["op"].as_str().unwrap().to_string();
        let mut args = case["args"].clone();
        match op.as_str() {
            "meta" => {
                metas.insert(args["id"].as_i64().unwrap(), args["meta"].clone());
                continue;
            }
            "summary" => {
                summary = case["out"]["ok"].clone();
                continue;
            }
            _ => {}
        }
        if let Some(id) = args.get("meta").and_then(|m| m.get("$meta")) {
            args["meta"] = metas[&id.as_i64().unwrap()].clone();
        }
        let want = &case["out"];
        let got = run(&op, &args, &work, n);
        let same = match (&got, want.get("ok"), want.get("err")) {
            // a staged tree is compared as a mapping (os.walk and read_dir list in other orders)
            (Ok(v), Some(w), _) if op == "stage" => v == w,
            (Ok(v), Some(w), _) => json_dumps(v) == json_dumps(w),
            (Err((k, m)), None, Some(e)) => {
                e == k && (*k == "crash" || want["msg"].as_str() == Some(m.as_str()))
            }
            _ => false,
        };
        if !same && bad.len() < 10 {
            if let (Ok(Value::Object(g)), Some(Value::Object(w))) = (&got, want.get("ok")) {
                for (k, v) in w {
                    if g.get(k) != Some(v) {
                        bad.push(format!(
                            "{op}: key {k}: python {} rust {:?}",
                            json_dumps(v).chars().take(300).collect::<String>(),
                            g.get(k)
                                .map(|x| json_dumps(x).chars().take(300).collect::<String>())
                        ));
                    }
                }
                for k in g.keys() {
                    if !w.contains_key(k) {
                        bad.push(format!("{op}: key {k} only in rust"));
                    }
                }
            }
            bad.push(format!(
                "{op} {}\n  python: {}\n  rust:   {:?}",
                json_dumps(&case["args"])
                    .chars()
                    .take(600)
                    .collect::<String>(),
                json_dumps(want).chars().take(1200).collect::<String>(),
                got.map(|v| json_dumps(&v).chars().take(1200).collect::<String>())
            ));
        }
        *counts.entry(op).or_default() += 1;
    }
    assert!(child.wait().unwrap().success(), "py_ref.py failed");
    let _ = std::fs::remove_dir_all(&work);
    assert!(bad.is_empty(), "differences:\n{}", bad.join("\n"));
    let total: usize = counts.values().sum();
    // the M98 corpora as test_composer.py draws them
    assert_eq!(summary["w_values"], json!(3000));
    assert_eq!(summary["resolved"], json!(514));
    assert_eq!(summary["removed"], json!(86));
    assert!(summary["plans"].as_u64().unwrap() >= 15, "{summary}");
    assert_eq!(summary["ops"].as_u64().unwrap() as usize, total);
    eprintln!(
        "composer equivalence: {total} cases identical to hydo_composer.py: {counts:?}; {summary}"
    );
}
