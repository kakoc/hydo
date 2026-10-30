//! hydo-compose against hydo/onboard/hydo_compose.py, which it replaces: for every case of
//! tests/cases.json (the unit-test Compose projects, Docker's voting app with its lock file, and
//! real-world Compose files under tests/fixtures/realworld/, each with and without
//! `--network loopback` / `--no-addr` / `--no-resolve`, plus option and error cases), the same
//! exit status, stdout, stderr and files (sha256 and mode) as the Python recorded in
//! tests/golden.json by tests/make_golden.py. No Python and no network are needed here.
//!
//! Where the Python stopped with a traceback (golden "crash"), the Rust must exit with the same
//! status, write the same files (none) and print one `hydo-compose: ` line instead.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn here() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn copytree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    let mut v: Vec<_> = std::fs::read_dir(src)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let (s, d) = (e.path(), dst.join(e.file_name()));
        let md = std::fs::symlink_metadata(&s).unwrap();
        if md.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&s).unwrap(), &d).unwrap();
        } else if md.is_dir() {
            copytree(&s, &d);
        } else {
            std::fs::copy(&s, &d).unwrap();
        }
    }
}

fn snap(d: &Path, base: &Path, out: &mut BTreeMap<String, (String, String)>) {
    let mut v: Vec<_> = std::fs::read_dir(d)
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    v.sort_by_key(|e| e.file_name());
    for e in v {
        let p = e.path();
        let md = std::fs::symlink_metadata(&p).unwrap();
        if md.file_type().is_symlink() {
            continue;
        }
        if md.is_dir() {
            snap(&p, base, out);
        } else {
            let h: String = Sha256::digest(std::fs::read(&p).unwrap())
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect();
            let mode = format!("0o{:o}", md.permissions().mode() & 0o777);
            out.insert(
                p.strip_prefix(base).unwrap().display().to_string(),
                (h, mode),
            );
        }
    }
}

fn s<'a>(v: &'a serde_json::Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn prepare(case: &serde_json::Value, d: &Path) {
    let src = top().join(s(case, "fixture"));
    copytree(&src, &d.join("proj"));
    std::fs::create_dir_all(d.join("home")).unwrap();
    let setup = s(case, "setup");
    let proj = d.join("proj");
    if setup == "rm .env" {
        std::fs::remove_file(proj.join(".env")).unwrap();
    } else if setup == "dotenv" {
        std::fs::copy(proj.join("example.env"), proj.join(".env")).unwrap();
    } else if let Some(fs) = setup.strip_prefix("touch:") {
        for f in fs.split(',') {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(proj.join(f))
                .unwrap();
        }
    } else if let Some(j) = setup.strip_prefix("json:") {
        std::fs::write(proj.join(s(case, "compose")), j).unwrap();
    } else if let Some(w) = setup.strip_prefix("write:") {
        let (f, body) = w.split_once(':').unwrap();
        std::fs::write(proj.join(f), body).unwrap();
    }
    let lock = d.join("lock.json");
    match s(case, "lock") {
        "unit" => {
            std::fs::copy(here().join("fixtures/unit-lock.json"), &lock).unwrap();
        }
        "partial" => {
            let full: serde_json::Value = serde_json::from_slice(
                &std::fs::read(here().join("fixtures/unit-lock.json")).unwrap(),
            )
            .unwrap();
            let mut imgs = full["images"].as_object().unwrap().clone();
            imgs.remove("redis:7.2.4");
            std::fs::write(&lock, serde_json::json!({ "images": imgs }).to_string()).unwrap();
        }
        "voting" => {
            std::fs::copy(
                top().join("hydo/onboard/examples/compose-voting/hydo-compose.lock.json"),
                &lock,
            )
            .unwrap();
        }
        "fixture" => {
            std::fs::copy(src.join("lock.json"), &lock).unwrap();
        }
        _ => {}
    }
}

fn argv(case: &serde_json::Value) -> Vec<String> {
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect();
    if case["raw"].as_bool().unwrap_or(false) {
        return args;
    }
    let mut v = vec![format!("proj/{}", s(case, "compose"))];
    if !args.iter().any(|a| a == "--out") {
        v.extend(["--out".to_string(), "proj/hydo.yaml".to_string()]);
    }
    if s(case, "lock") != "none" {
        v.extend(["--lock".to_string(), "lock.json".to_string()]);
    } else {
        v.extend(["--lock".to_string(), "nolock.json".to_string()]);
    }
    v.extend(args);
    v
}

#[test]
fn compose_equals_python() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_slice(&std::fs::read(here().join("cases.json")).unwrap()).unwrap();
    let gold: serde_json::Value =
        serde_json::from_slice(&std::fs::read(here().join("golden.json")).unwrap()).unwrap();
    let base = std::env::temp_dir().join(format!("hydo-compose-eq-{}", std::process::id()));
    let mut bad = Vec::new();
    let (mut n, mut crashes) = (0, 0);
    for case in &cases {
        let name = s(case, "name");
        let g = &gold[name];
        let d = base.join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let d = d.canonicalize().unwrap();
        prepare(case, &d);
        let mut before = BTreeMap::new();
        snap(&d, &d, &mut before);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_hydo-compose"));
        cmd.args(argv(case))
            .current_dir(&d)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", d.join("home"))
            .env("LANG", "C.UTF-8");
        if let Some(e) = case["env"].as_object() {
            for (k, v) in e {
                cmd.env(k, v.as_str().unwrap());
            }
        }
        let o = cmd.output().unwrap();
        let mut after = BTreeMap::new();
        snap(&d, &d, &mut after);
        let files: BTreeMap<String, serde_json::Value> = after
            .into_iter()
            .filter(|(k, v)| before.get(k) != Some(v))
            .map(|(k, (h, m))| (k, serde_json::json!([h, m])))
            .collect();
        let norm = |b: &[u8]| String::from_utf8_lossy(b).replace(&d.display().to_string(), "<DIR>");
        let (out, err) = (norm(&o.stdout), norm(&o.stderr));
        let mut why = Vec::new();
        if o.status.code() != g["rc"].as_i64().map(|x| x as i32) {
            why.push(format!("rc {:?} != {}", o.status.code(), g["rc"]));
        }
        if out != s(g, "stdout") {
            why.push(format!("stdout:\n{out}\n--- python:\n{}", s(g, "stdout")));
        }
        if g.get("crash").is_some() {
            crashes += 1;
            if !(err.starts_with("hydo-compose: ") && err.trim_end().lines().count() == 1) {
                why.push(format!(
                    "python crashed ({}); rust stderr: {err}",
                    s(g, "crash")
                ));
            }
        } else if let Some(h) = g.get("stderr_sha256").and_then(|x| x.as_str()) {
            // a long stderr is recorded by its hash and its first 1500 characters
            let got: String = Sha256::digest(err.as_bytes())
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect();
            if got != h {
                why.push(format!(
                    "stderr (sha256 {got} != {h}):\n{}\n--- python (start):\n{}",
                    err.chars().take(1500).collect::<String>(),
                    s(g, "stderr")
                ));
            }
        } else if err != s(g, "stderr") {
            why.push(format!("stderr:\n{err}\n--- python:\n{}", s(g, "stderr")));
        }
        let want: BTreeMap<String, serde_json::Value> = g["files"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if files != want {
            why.push(format!("files: {files:?}\n--- python: {want:?}"));
        }
        if why.is_empty() {
            let _ = std::fs::remove_dir_all(&d);
        } else {
            bad.push(format!(
                "== {name} (kept in {})\n{}",
                d.display(),
                why.join("\n")
            ));
        }
        n += 1;
    }
    if bad.is_empty() {
        let _ = std::fs::remove_dir_all(&base);
    }
    assert!(
        bad.is_empty(),
        "{} of {n} cases differ:\n{}",
        bad.len(),
        bad.join("\n")
    );
    eprintln!("hydo-compose: {n} cases identical to hydo_compose.py ({crashes} where the Python stopped with a traceback)");
}
