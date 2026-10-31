//! hydo-manifest against the answers of hydo-build (the Python reference, frozen): for every
//! manifest candidate in the repository (the pinned list of tracked *.yaml, *.yml and *.json), on
//! both architectures, for the cases the onboarding unit tests built plus the branches of the
//! validator (tests/fixtures/cases.json), for YAML-subset texts (tests/fixtures/yaml_cases.json),
//! for hydo/ folders (tests/fixtures/folders/), with `--set`, and for seeded mutants of the
//! repository manifests: the same acceptance, the same message, the same normalised data, the
//! same warnings and the same `--check` lines. Then the image path (manifest_space.Space): every
//! accepted manifest read back from its text and its plan meta, as an image carries them.
//!
//! The reference answers are in `tests/golden/answers.txt`. They were recorded by running
//! `tests/py_dump.py --batch` (hydo-build's own steps, Python 3.15.0 on the Mac) on exactly these
//! requests at commit 2a58c4b, the last commit with the Python builder; `git show
//! 2a58c4b:hydo/crates/hydo-manifest/tests/py_dump.py` and `git show 2a58c4b:hydo/onboard/hydo-build`
//! restore them. One line per request: an error message is kept verbatim (a long one as its start
//! and the digest of the whole), an accepted manifest as
//! SHA-256 digests (32 bits each) of its normalised data, warnings, `--check` lines and plan meta (keys
//! sorted, the repository root written `<root>` and the scratch directory `<tmp>`). Where
//! hydo-build stopped with a traceback, the line says `crash` and Rust must refuse.
//!
//! The input set is pinned in the same file (`I` lines: each repository file with the SHA-256 of
//! its bytes, or of its whole `hydo/` folder for a folder plan), so `git ls-files` does not decide
//! it: when an input changed or vanished since the recording, the test fails and names it. Recorded
//! answers of other tests (`*/tests/golden*`) and the Python unit tests' fixtures
//! (`hydo/onboard/tests`) are not in the pinned set.
//!
//! From now on Rust is the reference: after a deliberate change, `HYDO_BLESS=1 cargo test -p
//! hydo-manifest --test equivalence` rewrites the golden from the Rust answers (the pinned list
//! from the tracked files as they are now); review the diff of `tests/golden/answers.txt` before
//! committing.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use hydo_manifest::{check_summary, from_embedded_meta, load, py::CRASH_PREFIX, LoadOptions};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo_root() -> PathBuf {
    crate_dir().join("../../..").canonicalize().unwrap()
}

fn golden_path() -> PathBuf {
    crate_dir().join("tests/golden/answers.txt")
}

fn bless() -> bool {
    std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1")
}

/// One request.
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
    fn label(&self) -> String {
        match self {
            Req::Load { path, arch, sets } => format!("{path} --arch {arch} {sets:?}"),
            Req::Embedded { text, compose } => format!("embedded {text} {compose:?}"),
        }
    }
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

/// The repository's manifest candidates as they are tracked now (used only to record: HYDO_BLESS).
fn tracked_now(root: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("ls-files")
        .output()
        .expect("git ls-files");
    let mut v: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| l.ends_with(".yaml") || l.ends_with(".yml") || l.ends_with(".json"))
        // hydo/schema/: the project format's own example documents (tests/format.rs reads them)
        // and the results format's (M115)
        .filter(|l| !l.starts_with("hydo/onboard/tests/") && !l.starts_with("hydo/schema/"))
        .filter(|l| !l.contains("/tests/golden"))
        // the CLI tests' recorded run and exploration directories (results format, M115)
        .filter(|l| !l.starts_with("hydo/crates/hydo-cli/tests/fixtures/"))
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

/// The digest that pins one repository input: its bytes, or for `<dir>/hydo/hydo.yaml` every
/// file of that `hydo/` folder (relative names, the executable bit and bytes).
fn input_digest(root: &Path, rel: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let p = root.join(rel);
    let folder = p
        .parent()
        .filter(|d| rel.ends_with("/hydo/hydo.yaml") && d.file_name().is_some_and(|n| n == "hydo"));
    let Some(dir) = folder else {
        return match std::fs::read(&p) {
            Ok(b) => sha256_hex(&b),
            Err(_) => "missing".into(),
        };
    };
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let q = e.path();
            if q.is_dir() {
                stack.push(q);
            } else {
                files.push(q);
            }
        }
    }
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        let relf = f.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
        let exec = std::fs::metadata(&f)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
        h.update(relf.as_bytes());
        h.update([0, u8::from(exec)]);
        h.update(std::fs::read(&f).unwrap_or_default());
        h.update([0]);
    }
    h.finalize().iter().map(|x| format!("{x:02x}")).collect()
}

/// Python's own messages that changed between versions: before 3.13, "too many values to unpack
/// (expected 2)" had no ", got N". The recording and Rust are compared without that part.
fn version_neutral(v: &Value) -> Value {
    match v {
        Value::String(s) => {
            static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = RE.get_or_init(|| regex::Regex::new(r"\(expected (\d+), got \d+\)").unwrap());
            if !s.contains(", got ") {
                return v.clone();
            }
            Value::String(re.replace_all(s, "(expected $1)").into_owned())
        }
        Value::Array(a) => Value::Array(a.iter().map(version_neutral).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), version_neutral(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Paths written the same on every machine: the scratch directory and the repository root.
struct Norm {
    pairs: Vec<(String, &'static str)>,
}

impl Norm {
    fn new(root: &Path, tmp: &Path) -> Norm {
        let mut pairs = Vec::new();
        for (p, name) in [(tmp, "<tmp>"), (root, "<root>")] {
            let s = p.to_string_lossy().into_owned();
            if let Ok(c) = p.canonicalize() {
                let c = c.to_string_lossy().into_owned();
                if c != s {
                    pairs.push((c, name));
                }
            }
            pairs.push((s, name));
        }
        Norm { pairs }
    }
    fn s(&self, x: &str) -> String {
        let mut x = x.to_string();
        for (from, to) in &self.pairs {
            if x.contains(from.as_str()) {
                x = x.replace(from.as_str(), to);
            }
        }
        x
    }
    /// Keys sorted (an object compares as a map, as Value equality did), paths normalised.
    fn v(&self, v: &Value) -> Value {
        match v {
            Value::String(s) => Value::String(self.s(s)),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.v(x)).collect()),
            Value::Object(o) => {
                let mut keys: Vec<&String> = o.keys().collect();
                keys.sort();
                let mut m = serde_json::Map::new();
                for k in keys {
                    m.insert(self.s(k), self.v(&o[k]));
                }
                Value::Object(m)
            }
            other => other.clone(),
        }
    }
}

fn h32(v: Option<&Value>, n: &Norm) -> String {
    let t = n.v(&version_neutral(v.unwrap_or(&Value::Null))).to_string();
    sha256_hex(t.as_bytes())[..8].to_string()
}

/// A message as a JSON string; a long one as its first 60 characters, `…` and the 64-bit digest of
/// the whole text (the golden stays small, and a difference still shows where it starts).
fn short_message(e: &str) -> String {
    if e.chars().count() <= 80 {
        return serde_json::to_string(e).unwrap();
    }
    let head: String = e.chars().take(60).collect();
    let s = format!("{head}…#{}", &sha256_hex(e.as_bytes())[..16]);
    serde_json::to_string(&s).unwrap()
}

/// One answer as a golden line's kind and payload.
fn record_of(v: &Value, n: &Norm) -> (String, String) {
    let ok = v["ok"].as_bool().unwrap_or(false);
    if !ok {
        let e = version_neutral(&v["error"]);
        let e = n.s(e.as_str().unwrap_or(""));
        // the reference's traceback, or Rust's refusal where hydo-build had one (HYDO_BLESS)
        let kind = if v["crash"].as_bool().unwrap_or(false) || e.starts_with(CRASH_PREFIX) {
            "crash"
        } else {
            "err"
        };
        return (kind.into(), short_message(&e));
    }
    if v.get("oci") == Some(&Value::Bool(true)) {
        return ("oci".into(), String::new());
    }
    let mut p = String::new();
    for (k, short) in [
        ("data", 'd'),
        ("warnings", 'w'),
        ("check", 'c'),
        ("meta", 'm'),
    ] {
        let _ = write!(p, "{short}:{} ", h32(v.get(k), n));
    }
    ("ok".into(), p.trim_end().to_string())
}

#[derive(Default)]
struct Tally {
    same_ok: usize,
    same_err: usize,
    crash: usize,
    oci_skipped: usize,
    mismatches: Vec<String>,
}

/// The comparison of the live test, against a recorded line: the reference's (kind, payload) and
/// Rust's answer.
fn compare(label: &str, gk: &str, gp: &str, rs: &Value, n: &Norm, t: &mut Tally) {
    let rok = rs["ok"].as_bool().unwrap_or(false);
    let (rk, rp) = record_of(rs, n);
    if gk == "crash" {
        // hydo-build stopped with a traceback: refused here too
        if rok {
            t.mismatches
                .push(format!("{label}: hydo-build crashed ({gp}), Rust accepts"));
        } else {
            t.crash += 1;
        }
        return;
    }
    let pok = gk == "ok" || gk == "oci";
    if pok != rok {
        t.mismatches.push(format!(
            "{label}: hydo-build {} / Rust {}",
            if pok {
                "accepted".to_string()
            } else {
                format!("refused: {gp}")
            },
            if rok {
                "accepts".to_string()
            } else {
                format!("refuses: {rp}")
            }
        ));
        return;
    }
    if !pok {
        if gp != rp {
            t.mismatches
                .push(format!("{label}: messages differ\n  py: {gp}\n  rs: {rp}"));
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
    if gk == "oci" || rk == "oci" {
        t.oci_skipped += 1;
        return;
    }
    if gp == rp {
        t.same_ok += 1;
    } else {
        t.mismatches.push(format!(
            "{label}: accepted by both, but the digests differ\n  py: {gp}\n  rs: {rp}\n  Rust's answer: {}",
            n.v(rs)
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

/// Where the reference answers come from in this run.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Golden,
    Bless,
}

fn mode() -> Mode {
    if bless() {
        Mode::Bless
    } else {
        Mode::Golden
    }
}

struct Golden {
    inputs: Vec<(String, String)>,
    lines: Vec<(String, String, String, String)>,
}

fn read_golden() -> Golden {
    let text = std::fs::read_to_string(golden_path()).unwrap_or_else(|e| {
        panic!(
            "{}: {e} (HYDO_BLESS=1 writes it from the Rust answers)",
            golden_path().display()
        )
    });
    let mut g = Golden {
        inputs: Vec::new(),
        lines: Vec::new(),
    };
    for l in text.lines() {
        if l.starts_with('#') || l.is_empty() {
            continue;
        }
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        match f[0] {
            "I" => g.inputs.push((f[1].to_string(), f[2].to_string())),
            "R" => {
                // `^` repeats the group of the line before
                let grp = match (f[1], g.lines.last()) {
                    ("^", Some(prev)) => prev.0.clone(),
                    (x, _) => x.to_string(),
                };
                g.lines.push((
                    grp,
                    f[2].to_string(),
                    f[3].to_string(),
                    f.get(4).unwrap_or(&"").to_string(),
                ))
            }
            other => panic!("golden: unknown line kind {other:?}"),
        }
    }
    g
}

const GOLDEN_HEADER: &str = "\
# hydo-manifest's equivalence test: the answers of hydo-build (the Python reference).
# Recorded by tests/py_dump.py --batch (hydo-build's own steps) with Python 3.15.0 on the Mac at
# commit 2a58c4b (restore: git show 2a58c4b:hydo/crates/hydo-manifest/tests/py_dump.py and
# git show 2a58c4b:hydo/onboard/hydo-build). Rewritten from Rust only by HYDO_BLESS=1.
# I <tab> repository input <tab> sha256 of its bytes (of its whole hydo/ folder for a folder plan)
# R <tab> group <tab> request <tab> ok|err|crash|oci <tab> 32-bit digests of data (d), warnings (w),
#   --check lines (c) and meta (m) (keys sorted, paths as <root> and <tmp>) | the message (JSON string;
#   over 80 characters: its first 60, … and the digest of the whole); a group ^ is the line before's
";

#[test]
fn same_as_hydo_build() {
    let root = repo_root();
    let fixtures = crate_dir().join("tests/fixtures");
    let tmp = std::env::temp_dir().join(format!("hydo-manifest-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("cases")).unwrap();
    std::fs::create_dir_all(tmp.join("yaml")).unwrap();
    std::fs::create_dir_all(tmp.join("embedded")).unwrap();
    let norm = Norm::new(&root, &tmp);
    let arches = ["x86_64", "arm64"];
    let mode = mode();
    let golden = if mode == Mode::Golden {
        Some(read_golden())
    } else {
        None
    };

    // the pinned repository inputs
    let pinned: Vec<String> = match &golden {
        Some(g) => {
            let changed: Vec<String> = g
                .inputs
                .iter()
                .filter(|(rel, d)| &input_digest(&root, rel) != d)
                .map(|(rel, _)| rel.clone())
                .collect();
            assert!(
                changed.is_empty(),
                "{} repository inputs changed (or vanished) since the answers were recorded: {changed:?}\n\
                 If the change is deliberate, re-record from Rust with HYDO_BLESS=1 and review the diff.",
                changed.len()
            );
            g.inputs.iter().map(|(r, _)| r.clone()).collect()
        }
        None => tracked_now(&root),
    };
    let tracked: Vec<String> = pinned
        .iter()
        .map(|r| root.join(r).to_string_lossy().into_owned())
        .collect();

    let mut groups: BTreeMap<&str, Vec<Req>> = BTreeMap::new();
    // 1. every pinned manifest candidate, both architectures
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
        groups.entry("--set").or_default().push(Req::Load {
            path: p.to_string_lossy().into_owned(),
            arch: "x86_64".into(),
            sets: sets
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        });
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
    for (name, sets) in [
        ("p-ok", vec![("nm", "xyz")]),
        ("p-ok", vec![("zz", "1")]),
        ("p-deep", vec![("y", "{{x}}")]),
    ] {
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

    let mut all: Vec<(String, Req)> = Vec::new();
    for (g, rs) in &groups {
        for r in rs {
            all.push((g.to_string(), r.clone()));
        }
    }
    let rust: Vec<Value> = all.iter().map(|(_, r)| rust_side(r)).collect();
    // the reference's answers to the loads, as (kind, payload)
    let reference =
        |i: usize, answers: &[Value]| -> (String, String) { record_of(&answers[i], &norm) };
    let mut refs: Vec<(String, String)> = (0..all.len())
        .map(|i| match mode {
            Mode::Bless => reference(i, &rust),
            Mode::Golden => (String::new(), String::new()),
        })
        .collect();
    if let Some(g) = &golden {
        for (i, (grp, r)) in all.iter().enumerate() {
            let want = (grp.clone(), norm.s(&r.label()));
            match g.lines.get(i) {
                Some((gg, gl, k, p)) if (gg, gl) == (&want.0, &want.1) => {
                    refs[i] = (k.clone(), p.clone())
                }
                other => panic!(
                    "request {i} differs from the recording: now {want:?}, recorded {:?}",
                    other.map(|(a, b, _, _)| (a, b))
                ),
            }
        }
    }

    // the image's view of every accepted x86_64 load: its text and its plan meta
    let mut embedded: Vec<Req> = Vec::new();
    for (i, (_, r)) in all.iter().enumerate() {
        let Req::Load { path, arch, sets } = r else {
            continue;
        };
        if refs[i].0 != "ok" || arch != "x86_64" || !sets.is_empty() {
            continue;
        }
        let src = &rust[i];
        let compose = src["data"]
            .get("_compose")
            .filter(|c| !c.is_null())
            .map(|c| {
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
    let n0 = all.len();
    for r in &embedded {
        all.push(("image path (embedded)".to_string(), r.clone()));
    }
    let rust2: Vec<Value> = embedded.iter().map(rust_side).collect();
    for (j, r) in embedded.iter().enumerate() {
        let i = n0 + j;
        let rec = match mode {
            Mode::Bless => record_of(&rust2[j], &norm),
            Mode::Golden => {
                let g = golden.as_ref().unwrap();
                let want = ("image path (embedded)".to_string(), norm.s(&r.label()));
                match g.lines.get(i) {
                    Some((gg, gl, k, p)) if (gg, gl) == (&want.0, &want.1) => {
                        (k.clone(), p.clone())
                    }
                    other => panic!(
                        "request {i} differs from the recording: now {want:?}, recorded {:?}",
                        other.map(|(a, b, _, _)| (a, b))
                    ),
                }
            }
        };
        refs.push(rec);
    }
    if let Some(g) = &golden {
        assert_eq!(
            g.lines.len(),
            all.len(),
            "the recording has {} requests, this run {}",
            g.lines.len(),
            all.len()
        );
    }

    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for (i, ((grp, r), (k, p))) in all.iter().zip(&refs).enumerate() {
        let rs = if i < n0 { &rust[i] } else { &rust2[i - n0] };
        compare(
            &norm.s(&r.label()),
            k,
            p,
            rs,
            &norm,
            tallies.entry(grp.clone()).or_default(),
        );
    }

    if mode != Mode::Golden {
        let mut out = String::from(GOLDEN_HEADER);
        for rel in &pinned {
            let _ = writeln!(out, "I\t{rel}\t{}", input_digest(&root, rel));
        }
        let mut last: Option<&String> = None;
        for ((grp, r), (k, p)) in all.iter().zip(&refs) {
            let g = if Some(grp) == last { "^" } else { grp.as_str() };
            let _ = writeln!(out, "R\t{g}\t{}\t{k}\t{p}", norm.s(&r.label()));
            last = Some(grp);
        }
        std::fs::create_dir_all(golden_path().parent().unwrap()).unwrap();
        std::fs::write(golden_path(), out).unwrap();
        eprintln!("wrote {}", golden_path().display());
    }

    let mut report = String::new();
    let mut bad = Vec::new();
    for (g, t) in &tallies {
        report.push_str(&format!(
            "{g}: {} accepted by both with the same data, {} refused by both with the same message, {} refused where hydo-build crashed, {} with an image not read, {} mismatches\n",
            t.same_ok,
            t.same_err,
            t.crash,
            t.oci_skipped,
            t.mismatches.len()
        ));
        bad.extend(t.mismatches.iter().cloned());
    }
    report.push_str(&format!(
        "({} pinned repository files, {} fixture cases, {} yaml texts, {} folders, {nmut} mutants)\n",
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
        bad.iter().take(30).cloned().collect::<Vec<_>>().join("\n")
    );
}
