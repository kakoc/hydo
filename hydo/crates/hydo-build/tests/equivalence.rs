//! hydo-build (Rust) against the answers of hydo/onboard/hydo-build (the Python reference, frozen):
//! `--check`, `--print-shell` and `--print-world` (and, until M136, `--print-init`, now a refusal
//! that one `--set and flag combinations` line still records), on both architectures, for every
//! manifest candidate in the repository (the pinned list of tracked *.yaml, *.yml and *.json, which
//! includes the hydo/ folder fixtures), for every fixture case of hydo-manifest's tests (written out
//! as JSON plans), for seeded mutants of the repository manifests, and with `--set` and argument
//! errors: the same exit status, the same stdout and the same stderr. Where the Python stopped with
//! a traceback, the Rust must refuse with a message (exit status 1, a `hydo-build:` line, no stdout).
//!
//! The reference answers are in `tests/golden/answers.txt`, recorded by running `tests/py_ref.py`
//! (the Python hydo-build in-process, Python 3.15.0 on the Mac) on exactly these command lines at
//! commit 2a58c4b, the last commit with the Python builder (`git show
//! 2a58c4b:hydo/crates/hydo-build/tests/py_ref.py` and `git show 2a58c4b:hydo/onboard/hydo-build`
//! restore them). One line per run: the exit status, stdout as its SHA-256 (64 bits) and length
//! (verbatim when short), stderr verbatim (a long one as its first 60 characters and the digest of
//! the whole); the repository root is written `<root>` and the scratch directory `<tmp>`.
//!
//! The input set is pinned in the same file (`I` lines: each repository manifest candidate, the
//! guest files that `--print-shell` reads (until M136 also the shell init files `--print-init`
//! read), each with the SHA-256 of its
//! bytes, or of its whole `hydo/` folder for a folder plan), so `git ls-files` does not decide it:
//! when an input changed or vanished since the recording, the test fails and names it. Recorded
//! answers of other tests (`*/tests/golden*`) and the Python unit tests' fixtures
//! (`hydo/onboard/tests`) are not in the pinned set.
//!
//! From now on Rust is the reference: after a deliberate change, `HYDO_BLESS=1 cargo test -p
//! hydo-build --test equivalence` rewrites the golden from the Rust answers (the pinned list from
//! the files as they are now); review the diff of `tests/golden/answers.txt` before committing.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo() -> PathBuf {
    crate_dir().join("../../..").canonicalize().unwrap()
}

fn golden_path() -> PathBuf {
    crate_dir().join("tests/golden/answers.txt")
}

fn run_rust(argv: &[String]) -> (i32, String, String) {
    let ctx = hydo_build::Ctx {
        onboard: repo().join("hydo/onboard"),
        release: hydo_build::Release::None,
    };
    let mut o: Vec<u8> = Vec::new();
    let mut e: Vec<u8> = Vec::new();
    let rc = {
        let mut io = hydo_build::Io {
            out: &mut o,
            err: &mut e,
        };
        hydo_build::main_with(&ctx, argv, &mut io)
    };
    (
        rc,
        String::from_utf8_lossy(&o).into_owned(),
        String::from_utf8_lossy(&e).into_owned(),
    )
}

/// The manifest candidates as they are tracked now (used only to record: HYDO_BLESS).
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
        // hydo/schema/: the project format's example documents (hydo-manifest tests/format.rs)
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

/// The guest files the printed outputs contain (pinned, not run as manifests).
fn guest_files_now(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join("hydo/onboard"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".sh"))
        .map(|n| format!("hydo/onboard/{n}"))
        .collect();
    v.push("hydo/history/hydo_hist.sh".into());
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

/// Python's own message that changed between versions: before 3.13, "too many values to unpack
/// (expected 2)" had no ", got N". Compared without that part, as hydo-manifest's test does.
fn version_neutral(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\(expected (\d+), got \d+\)").unwrap())
        .replace_all(s, "(expected $1)")
        .into_owned()
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
}

/// A text as a JSON string; a long one as its first 60 characters, `…` and the 64-bit digest of
/// the whole (the golden stays small, and a difference still shows where it starts).
fn short_text(e: &str) -> String {
    if e.chars().count() <= 80 {
        return serde_json::to_string(e).unwrap();
    }
    let head: String = e.chars().take(60).collect();
    let s = format!("{head}…#{}", &sha256_hex(e.as_bytes())[..16]);
    serde_json::to_string(&s).unwrap()
}

/// stdout as a digest and its length (verbatim when short).
fn out_text(o: &str) -> String {
    if o.chars().count() <= 80 {
        return serde_json::to_string(o).unwrap();
    }
    format!("#{}/{}", &sha256_hex(o.as_bytes())[..16], o.len())
}

/// One run as a golden line's payload: `run <rc> <stdout> <stderr>` or `crash <exception>`.
fn record(rc: i32, out: &str, err: &str, n: &Norm) -> String {
    format!(
        "run\t{rc}\t{}\t{}",
        out_text(&n.s(out)),
        short_text(&n.s(&version_neutral(err)))
    )
}

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
        json!(2.5),
        json!("12"),
        json!(" 7 "),
        json!("bridge"),
        json!({"x": "y"}),
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

/// 16 per file: 1,104 mutants over the 69 repository manifests with a workload or a test (M103 ran
/// 12 per file over a larger set of candidates, 1,104 in all).
const MUTANTS_PER_FILE: usize = 16;

const FLAGS: &[&str] = &["--check", "--print-shell", "--print-world"];

const GOLDEN_HEADER: &str = "\
# hydo-build's equivalence test: the answers of the Python hydo-build (the reference).
# Recorded by tests/py_ref.py (the Python hydo-build in-process) with Python 3.15.0 on the Mac at
# commit 2a58c4b (restore: git show 2a58c4b:hydo/crates/hydo-build/tests/py_ref.py and
# git show 2a58c4b:hydo/onboard/hydo-build). Rewritten from Rust only by HYDO_BLESS=1.
# I <tab> repository input <tab> sha256 of its bytes (of its whole hydo/ folder for a folder plan)
# G <tab> guest file that the printed outputs contain <tab> sha256 of its bytes
# R <tab> group <tab> argv <tab> run <tab> exit status <tab> stdout <tab> stderr
#   (paths as <root> and <tmp>; stdout over 80 characters as #<64-bit sha256>/<bytes>; stderr as a
#   JSON string, over 80 characters its first 60, … and the digest of the whole)
# R <tab> group <tab> argv <tab> crash <tab> the Python exception (the reference stopped with a
#   traceback; Rust must exit 1 with a hydo-build: line and no stdout)
# A group ^ is the line before's; an argv ^ <rest> has the line before's first argument; a stderr ^
#   is the line before's.
";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Golden,
    Bless,
}

fn mode() -> Mode {
    if std::env::var_os("HYDO_BLESS").is_some_and(|v| v == "1") {
        Mode::Bless
    } else {
        Mode::Golden
    }
}

struct Golden {
    inputs: Vec<(String, String)>,
    guest: Vec<(String, String)>,
    /// (group, argv, payload)
    lines: Vec<(String, String, String)>,
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
        guest: Vec::new(),
        lines: Vec::new(),
    };
    for l in text.lines() {
        if l.starts_with('#') || l.is_empty() {
            continue;
        }
        let f: Vec<&str> = l.splitn(4, '\t').collect();
        match f[0] {
            "I" => g.inputs.push((f[1].to_string(), f[2].to_string())),
            "G" => g.guest.push((f[1].to_string(), f[2].to_string())),
            "R" => {
                let line = Compact::decode(g.lines.last(), f[1], f[2], f[3]);
                g.lines.push(line);
            }
            other => panic!("golden: unknown line kind {other:?}"),
        }
    }
    g
}

/// The golden's `R` lines repeat little: `^` for the group of the line before, `^ <rest>` for an
/// argv whose first argument (the manifest) is the line before's, and `^` for the same stderr.
struct Compact;

impl Compact {
    fn first(label: &str) -> &str {
        label.split(' ').next().unwrap_or("")
    }
    fn err_of(payload: &str) -> Option<&str> {
        payload
            .strip_prefix("run\t")
            .and_then(|p| p.splitn(3, '\t').nth(2))
    }
    fn encode(prev: Option<&(String, String, String)>, g: &str, label: &str, p: &str) -> String {
        let Some((pg, pl, pp)) = prev else {
            return format!("{g}\t{label}\t{p}");
        };
        let g = if pg == g { "^" } else { g };
        let first = Compact::first(label);
        let label = if first == Compact::first(pl) {
            format!("^{}", &label[first.len()..])
        } else {
            label.to_string()
        };
        let p = match (Compact::err_of(p), Compact::err_of(pp)) {
            (Some(e), Some(pe)) if e == pe => format!("{}^", &p[..p.len() - e.len()]),
            _ => p.to_string(),
        };
        format!("{g}\t{label}\t{p}")
    }
    fn decode(
        prev: Option<&(String, String, String)>,
        g: &str,
        label: &str,
        p: &str,
    ) -> (String, String, String) {
        let none = (String::new(), String::new(), String::new());
        let (pg, pl, pp) = prev.unwrap_or(&none);
        let g = if g == "^" { pg.clone() } else { g.to_string() };
        let label = match label.strip_prefix('^') {
            Some(rest) => format!("{}{rest}", Compact::first(pl)),
            None => label.to_string(),
        };
        let p = match (p.strip_suffix('^'), Compact::err_of(pp)) {
            (Some(head), Some(pe)) if p.starts_with("run\t") => format!("{head}{pe}"),
            _ => p.to_string(),
        };
        (g, label, p)
    }
}

#[test]
fn print_and_check_outputs_match_hydo_build() {
    let root = repo();
    let tmp = std::env::temp_dir().join(format!("hydo-build-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let norm = Norm::new(&root, &tmp);
    let mode = mode();
    let golden = (mode == Mode::Golden).then(read_golden);

    // the pinned inputs: manifest candidates and the guest files the outputs contain
    let (pinned, guest): (Vec<String>, Vec<String>) = match &golden {
        Some(g) => {
            let changed: Vec<String> = g
                .inputs
                .iter()
                .chain(&g.guest)
                .filter(|(rel, d)| &input_digest(&root, rel) != d)
                .map(|(rel, _)| rel.clone())
                .collect();
            assert!(
                changed.is_empty(),
                "{} repository inputs changed (or vanished) since the answers were recorded: {changed:?}\n\
                 If the change is deliberate, re-record from Rust with HYDO_BLESS=1 and review the diff.",
                changed.len()
            );
            let names = |v: &[(String, String)]| v.iter().map(|(r, _)| r.clone()).collect();
            (names(&g.inputs), names(&g.guest))
        }
        None => (tracked_now(&root), guest_files_now(&root)),
    };
    let files: Vec<String> = pinned
        .iter()
        .map(|r| root.join(r).to_string_lossy().into_owned())
        .collect();

    let mut groups: BTreeMap<&str, Vec<Vec<String>>> = BTreeMap::new();
    for f in &files {
        for arch in ["x86_64", "arm64"] {
            for flag in FLAGS {
                groups
                    .entry("tracked manifest candidates")
                    .or_default()
                    .push(vec![
                        f.clone(),
                        "--arch".into(),
                        arch.into(),
                        flag.to_string(),
                    ]);
            }
        }
        groups
            .entry("--set and flag combinations")
            .or_default()
            .extend([
                vec![
                    f.clone(),
                    "--set".into(),
                    "version=zz".into(),
                    "--print-shell".into(),
                ],
                vec![f.clone(), "--print-world".into(), "--print-init".into()],
            ]);
    }
    // hydo-manifest's fixture cases (validation branches), as JSON plans
    let cases: Vec<Value> = serde_json::from_str(
        &std::fs::read_to_string(root.join("hydo/crates/hydo-manifest/tests/fixtures/cases.json"))
            .unwrap(),
    )
    .unwrap();
    for (i, c) in cases.iter().enumerate() {
        let p = tmp.join(format!("case{i:03}.json"));
        std::fs::write(&p, c["manifest"].to_string()).unwrap();
        for arch in ["x86_64", "arm64"] {
            for flag in FLAGS {
                groups.entry("fixture cases").or_default().push(vec![
                    p.to_string_lossy().into_owned(),
                    "--arch".into(),
                    arch.into(),
                    flag.to_string(),
                ]);
            }
        }
    }
    // seeded mutants of every repository manifest: a value replaced by another type or an edge
    // value, a key removed or renamed, a list entry removed or doubled
    let mut rng = SplitMix(0x6879_646f_2d62_7569);
    let mut nmut = 0;
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        let Ok(base) = hydo_manifest::parse_text(&text, &hydo_manifest::py::basename(f), false)
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
            let p = tmp.join(format!("mutant{nmut:05}.json"));
            nmut += 1;
            std::fs::write(&p, m.to_string()).unwrap();
            let arch = if k % 4 == 0 { "arm64" } else { "x86_64" };
            for flag in FLAGS {
                groups.entry("mutants").or_default().push(vec![
                    p.to_string_lossy().into_owned(),
                    "--arch".into(),
                    arch.into(),
                    flag.to_string(),
                ]);
            }
        }
    }
    // the command line itself
    let any = root
        .join("hydo/onboard/examples/redis/hydo.yaml")
        .to_string_lossy()
        .into_owned();
    groups.entry("argument handling").or_default().extend([
        vec![any.clone(), "--bogus".into()],
        vec![
            any.clone(),
            "--set".into(),
            "novalue".into(),
            "--check".into(),
        ],
        vec![
            any.clone(),
            "--arch".into(),
            "riscv".into(),
            "--print-shell".into(),
        ],
        vec!["/nonexistent/hydo.yaml".into(), "--check".into()],
    ]);

    let mut total = 0;
    let mut same = 0;
    let mut crashes = 0;
    let mut mismatches = Vec::new();
    let mut summary = String::new();
    let mut recorded = String::from(GOLDEN_HEADER);
    if mode != Mode::Golden {
        for rel in &pinned {
            let _ = writeln!(recorded, "I\t{rel}\t{}", input_digest(&root, rel));
        }
        for rel in &guest {
            let _ = writeln!(recorded, "G\t{rel}\t{}", input_digest(&root, rel));
        }
    }
    let mut gi = 0;
    let mut prev: Option<(String, String, String)> = None;
    for (g, reqs) in &groups {
        let (mut g_same, mut g_crash) = (0, 0);
        for r in reqs {
            total += 1;
            let label = norm.s(&r.join(" "));
            let (rc, out, err) = run_rust(r);
            let mine = record(rc, &out, &err, &norm);
            let reference = match mode {
                Mode::Golden => {
                    let gl = golden.as_ref().unwrap().lines.get(gi);
                    gi += 1;
                    match gl {
                        Some((gg, ga, p)) if gg == g && ga == &label => p.clone(),
                        other => panic!(
                            "run {} differs from the recording: now {g:?} {label:?}, recorded {:?}",
                            gi - 1,
                            other.map(|(a, b, _)| (a, b))
                        ),
                    }
                }
                Mode::Bless => mine.clone(),
            };
            let _ = writeln!(
                recorded,
                "R\t{}",
                Compact::encode(prev.as_ref(), g, &label, &reference)
            );
            prev = Some((g.to_string(), label.clone(), reference.clone()));
            if let Some(exc) = reference.strip_prefix("crash\t") {
                if rc == 1 && err.starts_with("hydo-build: ") && out.is_empty() {
                    g_crash += 1;
                } else {
                    mismatches.push(format!(
                        "{label}: Python stopped with {exc}, Rust rc={rc} err={err:?}"
                    ));
                }
                continue;
            }
            if reference == mine {
                g_same += 1;
            } else {
                mismatches.push(format!(
                    "{label}:\n  recorded {reference}\n  Rust     {mine}\n  Rust stdout {:?}\n  Rust stderr {:?}",
                    norm.s(&out).chars().take(400).collect::<String>(),
                    norm.s(&err)
                ));
            }
        }
        same += g_same;
        crashes += g_crash;
        summary.push_str(&format!(
            "  {g}: {} runs, {g_same} identical, {g_crash} refused where Python stopped with a traceback\n",
            reqs.len()
        ));
    }
    if let Some(gd) = &golden {
        assert_eq!(
            gd.lines.len(),
            total,
            "the recording has {} runs, this test {total}",
            gd.lines.len()
        );
    } else {
        std::fs::create_dir_all(golden_path().parent().unwrap()).unwrap();
        std::fs::write(golden_path(), &recorded).unwrap();
        eprintln!("wrote {}", golden_path().display());
    }
    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!(
        "hydo-build equivalence: {total} runs, {same} identical, {crashes} Python tracebacks refused with a message, {} mismatches ({} pinned manifest candidates, {} fixture cases, {nmut} mutants)\n{summary}",
        mismatches.len(),
        files.len(),
        cases.len()
    );
    for m in mismatches.iter().take(25) {
        eprintln!("MISMATCH {m}\n");
    }
    assert!(mismatches.is_empty(), "{} mismatches", mismatches.len());
}
