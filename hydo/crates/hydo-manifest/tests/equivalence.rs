//! hydo-manifest against hydo-build (the reference): for every manifest candidate in the
//! repository (every tracked *.yaml, *.yml and *.json), on both architectures, for the cases the
//! onboarding unit tests build plus the branches of the validator (tests/fixtures/cases.json),
//! for YAML-subset texts (tests/fixtures/yaml_cases.json) and for hydo/ folders
//! (tests/fixtures/folders/): the same acceptance, the same message, the same normalised data,
//! the same warnings and the same `--check` lines. Then the image path (manifest_space.Space):
//! every accepted manifest read back from its text and its plan meta, as an image carries them.
//!
//! The reference runs as `python3 tests/py_dump.py --batch` (needs python3; set HYDO_PYTHON to
//! use another interpreter). Without an interpreter the test fails rather than passing silently.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hydo_manifest::{check_summary, from_embedded_meta, load, py::CRASH_PREFIX, LoadOptions};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_dir().join("../../..").canonicalize().unwrap()
}

fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
}

/// One request to both sides.
#[derive(Clone, Debug)]
enum Req {
    Load {
        path: String,
        arch: String,
        sets: Vec<(String, String)>,
    },
    Embedded {
        text: String,
        compose: Option<String>,
    },
}

impl Req {
    fn to_json(&self) -> Value {
        match self {
            Req::Load { path, arch, sets } => json!({"path": path, "arch": arch, "sets": sets}),
            Req::Embedded { text, compose } => json!({"embedded": text, "compose": compose}),
        }
    }
    fn label(&self) -> String {
        match self {
            Req::Load { path, arch, sets } => format!("{path} --arch {arch} {sets:?}"),
            Req::Embedded { text, compose } => format!("embedded {text} {compose:?}"),
        }
    }
}

fn run_python(reqs: &[Req]) -> Vec<Value> {
    let script = crate_dir().join("tests/py_dump.py");
    let mut child = Command::new(python())
        .arg(&script)
        .arg("--batch")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 is needed to run the reference (set HYDO_PYTHON)");
    let mut input = String::new();
    for r in reqs {
        input.push_str(&r.to_json().to_string());
        input.push('\n');
    }
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap().unwrap();
    assert!(out.status.success(), "py_dump.py failed");
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        lines.len(),
        reqs.len(),
        "py_dump.py answered {} of {} requests",
        lines.len(),
        reqs.len()
    );
    lines
}

fn rust_side(r: &Req) -> Value {
    match r {
        Req::Load { path, arch, sets } => {
            let opts = LoadOptions {
                arch: arch.clone(),
                sets: sets.clone(),
                ..LoadOptions::default()
            };
            match load(Path::new(path), &opts) {
                Ok(l) => json!({
                    "ok": true,
                    "data": l.manifest.data,
                    "warnings": l.warnings,
                    "check": check_summary(path, &l),
                    "oci": !l.oci_unchecked.is_empty(),
                }),
                Err(e) => json!({"ok": false, "error": e.message}),
            }
        }
        Req::Embedded { text, compose } => {
            let t = std::fs::read_to_string(text).unwrap();
            let c: Option<Value> = compose
                .as_ref()
                .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap());
            match from_embedded_meta(&t, c.as_ref()) {
                Ok((m, meta)) => json!({"ok": true, "data": m.data, "meta": meta}),
                Err(e) => json!({"ok": false, "error": e.message}),
            }
        }
    }
}

fn tracked_candidates(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .output()
        .expect("git ls-files");
    let mut v: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| l.ends_with(".yaml") || l.ends_with(".yml") || l.ends_with(".json"))
        .map(|l| root.join(l).to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[derive(Default)]
struct Tally {
    same_ok: usize,
    same_err: usize,
    crash: usize,
    oci_skipped: usize,
    mismatches: Vec<String>,
}

fn compare(label: &str, py: &Value, rs: &Value, t: &mut Tally) {
    let pok = py["ok"].as_bool().unwrap_or(false);
    let rok = rs["ok"].as_bool().unwrap_or(false);
    if !pok && py["crash"].as_bool().unwrap_or(false) {
        // hydo-build stops with a traceback: refused here too
        if rok {
            t.mismatches.push(format!(
                "{label}: hydo-build crashes ({}), Rust accepts",
                py["error"]
            ));
        } else {
            t.crash += 1;
        }
        return;
    }
    if pok != rok {
        t.mismatches.push(format!(
            "{label}: hydo-build {} / Rust {}",
            if pok {
                "accepts".to_string()
            } else {
                format!("refuses: {}", py["error"])
            },
            if rok {
                "accepts".to_string()
            } else {
                format!("refuses: {}", rs["error"])
            }
        ));
        return;
    }
    if !pok {
        if py["error"] != rs["error"] {
            t.mismatches.push(format!(
                "{label}: messages differ\n  py: {}\n  rs: {}",
                py["error"], rs["error"]
            ));
        } else if rs["error"]
            .as_str()
            .is_some_and(|e| e.starts_with(CRASH_PREFIX))
        {
            t.mismatches
                .push(format!("{label}: Rust says crash, Python refused cleanly"));
        } else {
            t.same_err += 1;
        }
        return;
    }
    if py.get("oci") == Some(&Value::Bool(true)) || rs.get("oci") == Some(&Value::Bool(true)) {
        t.oci_skipped += 1;
        return;
    }
    let mut diffs = Vec::new();
    for k in ["data", "warnings", "check", "meta"] {
        if py.get(k) != rs.get(k) {
            diffs.push(format!(
                "  {k}:\n    py: {}\n    rs: {}",
                py.get(k).map(|v| v.to_string()).unwrap_or_default(),
                rs.get(k).map(|v| v.to_string()).unwrap_or_default()
            ));
        }
    }
    if diffs.is_empty() {
        t.same_ok += 1;
    } else {
        t.mismatches.push(format!(
            "{label}: accepted by both, but\n{}",
            diffs.join("\n")
        ));
    }
}

const MUTANTS_PER_FILE: usize = 60;

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn paths(v: &Value, here: Vec<Value>, out: &mut Vec<Vec<Value>>) {
    out.push(here.clone());
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                let mut p = here.clone();
                p.push(Value::String(k.clone()));
                paths(x, p, out);
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                let mut p = here.clone();
                p.push(json!(i));
                paths(x, p, out);
            }
        }
        _ => {}
    }
}

fn at<'a>(v: &'a mut Value, p: &[Value]) -> &'a mut Value {
    let mut cur = v;
    for k in p {
        cur = match k {
            Value::String(s) => &mut cur[s.as_str()],
            _ => &mut cur[k.as_u64().unwrap() as usize],
        };
    }
    cur
}

fn mutate(m: &mut Value, rng: &mut SplitMix) {
    let mut ps = Vec::new();
    paths(m, Vec::new(), &mut ps);
    let p = ps[1 + rng.below(ps.len() - 1).min(ps.len() - 2)].clone();
    let (last, parent_path) = p.split_last().unwrap();
    let edge = [
        json!(null),
        json!(0),
        json!(1),
        json!(-1),
        json!(true),
        json!(false),
        json!(""),
        json!("x"),
        json!("A b"),
        json!([]),
        json!({}),
        json!(["x"]),
        json!({"a": 1}),
        json!(5000),
        json!("127.0.0.1"),
        json!("{{p}}"),
        json!("10.89.0.7"),
        json!("kill"),
        json!(4096),
        json!("v3"),
        json!("eventually"),
        json!(["kill", "freeze"]),
    ];
    let parent = at(m, parent_path);
    match rng.below(5) {
        0 => match (parent, last) {
            (Value::Object(o), Value::String(k)) => {
                o.shift_remove(k);
            }
            (Value::Array(a), i) => {
                let i = i.as_u64().unwrap() as usize;
                if i < a.len() {
                    a.remove(i);
                }
            }
            _ => {}
        },
        1 => match (parent, last) {
            (Value::Object(o), Value::String(k)) => {
                if let Some(x) = o.shift_remove(k) {
                    o.insert("bogus".into(), x);
                }
            }
            (Value::Array(a), i) => {
                let i = i.as_u64().unwrap() as usize;
                if i < a.len() {
                    let x = a[i].clone();
                    a.push(x);
                }
            }
            _ => {}
        },
        _ => {
            let x = edge[rng.below(edge.len())].clone();
            match (parent, last) {
                (Value::Object(o), Value::String(k)) => {
                    o.insert(k.clone(), x);
                }
                (Value::Array(a), i) => {
                    let i = i.as_u64().unwrap() as usize;
                    if i < a.len() {
                        a[i] = x;
                    }
                }
                _ => {}
            }
        }
    }
}

#[test]
fn same_as_hydo_build() {
    let root = repo_root();
    let fixtures = crate_dir().join("tests/fixtures");
    let tmp = std::env::temp_dir().join(format!("hydo-manifest-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("cases")).unwrap();
    std::fs::create_dir_all(tmp.join("yaml")).unwrap();
    std::fs::create_dir_all(tmp.join("embedded")).unwrap();
    let arches = ["x86_64", "arm64"];

    let mut groups: BTreeMap<&str, Vec<Req>> = BTreeMap::new();
    // 1. every tracked manifest candidate, both architectures
    let tracked = tracked_candidates(&root);
    for p in &tracked {
        for a in arches {
            groups
                .entry("repository files")
                .or_default()
                .push(Req::Load {
                    path: p.clone(),
                    arch: a.into(),
                    sets: vec![],
                });
        }
    }
    // with --set: a declared parameter, an undeclared one
    for (rel, sets) in [
        (
            "hydo/onboard/examples/redis-composed/hydo/hydo.yaml",
            vec![("transfer", "racy")],
        ),
        (
            "hydo/onboard/examples/redis-composed/hydo/hydo.yaml",
            vec![("nope", "1")],
        ),
        ("hydo/onboard/examples/redis/hydo.yaml", vec![("x", "1")]),
    ] {
        let p = root.join(rel);
        if p.exists() {
            groups.entry("--set").or_default().push(Req::Load {
                path: p.to_string_lossy().into_owned(),
                arch: "x86_64".into(),
                sets: sets
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            });
        }
    }
    // 2. the unit tests' cases and the validator's branches
    let cases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(fixtures.join("cases.json")).unwrap())
            .unwrap();
    for c in &cases {
        let p = tmp
            .join("cases")
            .join(format!("{}.json", c["name"].as_str().unwrap()));
        std::fs::write(&p, serde_json::to_string_pretty(&c["manifest"]).unwrap()).unwrap();
        for a in arches {
            groups.entry("fixture cases").or_default().push(Req::Load {
                path: p.to_string_lossy().into_owned(),
                arch: a.into(),
                sets: vec![],
            });
        }
    }
    let mut set_cases = vec![
        ("p-ok", vec![("nm", "xyz")]),
        ("p-ok", vec![("zz", "1")]),
        ("p-deep", vec![("y", "{{x}}")]),
    ];
    for (name, sets) in set_cases.drain(..) {
        let p = tmp.join("cases").join(format!("{name}.json"));
        groups.entry("--set").or_default().push(Req::Load {
            path: p.to_string_lossy().into_owned(),
            arch: "x86_64".into(),
            sets: sets
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        });
    }
    // 3. YAML-subset texts
    let ycases: Vec<Value> =
        serde_json::from_str(&std::fs::read_to_string(fixtures.join("yaml_cases.json")).unwrap())
            .unwrap();
    for c in &ycases {
        let p = tmp
            .join("yaml")
            .join(format!("{}.yaml", c["name"].as_str().unwrap()));
        std::fs::write(&p, c["text"].as_str().unwrap()).unwrap();
        groups.entry("yaml texts").or_default().push(Req::Load {
            path: p.to_string_lossy().into_owned(),
            arch: "x86_64".into(),
            sets: vec![],
        });
    }
    // 4. hydo/ folders
    let mut folders: Vec<PathBuf> = std::fs::read_dir(fixtures.join("folders"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    folders.sort();
    for f in &folders {
        for a in arches {
            groups.entry("hydo/ folders").or_default().push(Req::Load {
                path: f.join("hydo/hydo.yaml").to_string_lossy().into_owned(),
                arch: a.into(),
                sets: vec![],
            });
        }
    }

    // 5. seeded mutants of every repository manifest Rust can read: a key removed or renamed, a
    //    value replaced by another type or an edge value, a list entry removed or doubled
    std::fs::create_dir_all(tmp.join("mutants")).unwrap();
    let mut rng = SplitMix(0x6d75_7461_6e74_7321);
    let mut nmut = 0;
    for p in &tracked {
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        let Ok(base) = hydo_manifest::parse_text(&text, &hydo_manifest::py::basename(p), false)
        else {
            continue;
        };
        if base.get("workload").is_none() && base.get("test").is_none() {
            continue;
        }
        for k in 0..MUTANTS_PER_FILE {
            let mut m = base.clone();
            mutate(&mut m, &mut rng);
            if k % 3 == 0 {
                mutate(&mut m, &mut rng);
            }
            let mp = tmp.join("mutants").join(format!("m{nmut}.json"));
            nmut += 1;
            std::fs::write(&mp, m.to_string()).unwrap();
            groups.entry("mutants").or_default().push(Req::Load {
                path: mp.to_string_lossy().into_owned(),
                arch: "x86_64".into(),
                sets: vec![],
            });
        }
    }

    let mut all: Vec<(&str, Req)> = Vec::new();
    for (g, rs) in &groups {
        for r in rs {
            all.push((g, r.clone()));
        }
    }
    let reqs: Vec<Req> = all.iter().map(|(_, r)| r.clone()).collect();
    let py = run_python(&reqs);

    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut embedded: Vec<Req> = Vec::new();
    for (i, ((g, r), p)) in all.iter().zip(&py).enumerate() {
        let rs = rust_side(r);
        compare(&r.label(), p, &rs, tallies.entry(g).or_default());
        // the image's view of every accepted x86_64 load: its text and its plan meta
        if let (Req::Load { path, arch, sets }, true) = (r, p["ok"] == Value::Bool(true)) {
            if arch == "x86_64" && sets.is_empty() && p["oci"] != Value::Bool(true) {
                let compose = p["data"].get("_compose").filter(|c| !c.is_null()).map(|c| {
                    let mut c = c.clone();
                    c.as_object_mut().unwrap().remove("_folder");
                    let cp = tmp.join("embedded").join(format!("{i}.compose.json"));
                    std::fs::write(&cp, c.to_string()).unwrap();
                    cp.to_string_lossy().into_owned()
                });
                embedded.push(Req::Embedded {
                    text: path.clone(),
                    compose,
                });
            }
        }
    }
    let py2 = run_python(&embedded);
    for (r, p) in embedded.iter().zip(&py2) {
        let rs = rust_side(r);
        compare(
            &r.label(),
            p,
            &rs,
            tallies.entry("image path (embedded)").or_default(),
        );
    }

    let mut report = String::new();
    let mut bad = Vec::new();
    for (g, t) in &tallies {
        report.push_str(&format!(
            "{g}: {} accepted by both with the same data, {} refused by both with the same message, {} refused where hydo-build crashes, {} with an image not read, {} mismatches\n",
            t.same_ok,
            t.same_err,
            t.crash,
            t.oci_skipped,
            t.mismatches.len()
        ));
        bad.extend(t.mismatches.iter().cloned());
    }
    report.push_str(&format!(
        "({} tracked files, {} fixture cases, {} yaml texts, {} folders)\n",
        tracked.len(),
        cases.len(),
        ycases.len(),
        folders.len()
    ));
    eprintln!("{report}");
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}\n\n{report}",
        bad.len(),
        bad.join("\n")
    );
}
