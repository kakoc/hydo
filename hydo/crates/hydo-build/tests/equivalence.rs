//! hydo-build (Rust) against hydo/onboard/hydo-build (Python, the reference until the lead
//! deletes it): `--check`, `--print-shell`, `--print-init` and `--print-world`, on both
//! architectures, for every manifest candidate in the repository (every tracked *.yaml, *.yml
//! and *.json, which includes the hydo/ folder fixtures), for every fixture case of
//! hydo-manifest's tests (written out as JSON plans), and with `--set` and argument errors: the
//! same exit status, the same stdout and the same stderr, byte for byte. Where the Python stops
//! with a traceback, the Rust must refuse with a message (exit status 1, a `hydo-build:` line).
//!
//! The reference runs in-process as `python3 tests/py_ref.py` (set HYDO_PYTHON for another
//! interpreter); without one the test fails rather than passing silently.

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo() -> PathBuf {
    crate_dir().join("../../..").canonicalize().unwrap()
}

fn python() -> String {
    std::env::var("HYDO_PYTHON").unwrap_or_else(|_| "python3".into())
}

fn run_python(reqs: &[Vec<String>]) -> Vec<Value> {
    let mut child = Command::new(python())
        .arg(crate_dir().join("tests/py_ref.py"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python3 is needed to run the reference (set HYDO_PYTHON)");
    let mut input = String::new();
    for r in reqs {
        input.push_str(&json!({ "argv": r }).to_string());
        input.push('\n');
    }
    let mut stdin = child.stdin.take().unwrap();
    let w = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = child.wait_with_output().unwrap();
    w.join().unwrap().unwrap();
    assert!(out.status.success(), "py_ref.py failed");
    let v: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        v.len(),
        reqs.len(),
        "py_ref.py answered {} of {}",
        v.len(),
        reqs.len()
    );
    v
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

fn tracked(root: &Path) -> Vec<String> {
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

const FLAGS: &[&str] = &["--check", "--print-shell", "--print-init", "--print-world"];

#[test]
fn print_and_check_outputs_match_hydo_build() {
    let root = repo();
    let tmp = std::env::temp_dir().join(format!("hydo-build-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let mut groups: BTreeMap<&str, Vec<Vec<String>>> = BTreeMap::new();
    let files = tracked(&root);
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
    // the command line itself
    let any = files
        .iter()
        .find(|f| f.ends_with("onboard/examples/redis/hydo.yaml"))
        .cloned()
        .unwrap_or_else(|| files[0].clone());
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
    let mut crashes: BTreeMap<String, usize> = BTreeMap::new();
    let mut mismatches = Vec::new();
    let mut summary = String::new();
    for (g, reqs) in &groups {
        let py = run_python(reqs);
        let (mut g_same, mut g_crash) = (0, 0);
        for (r, p) in reqs.iter().zip(&py) {
            total += 1;
            let (rc, out, err) = run_rust(r);
            if p["crash"].as_bool().unwrap_or(false) {
                let exc = p["exc"]
                    .as_str()
                    .unwrap_or("")
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if rc == 1 && err.starts_with("hydo-build: ") && out.is_empty() {
                    g_crash += 1;
                    *crashes.entry(exc).or_default() += 1;
                } else {
                    mismatches.push(format!(
                        "{r:?}: Python stops with {}, Rust rc={rc} err={err:?}",
                        p["exc"]
                    ));
                }
                continue;
            }
            let prc = p["rc"].as_i64().unwrap_or(-1) as i32;
            if prc == rc && p["out"].as_str() == Some(&out) && p["err"].as_str() == Some(&err) {
                g_same += 1;
            } else {
                mismatches.push(format!(
                    "{r:?}:\n  py rc={prc} out={:?}\n     err={:?}\n  rs rc={rc} out={out:?}\n     err={err:?}",
                    p["out"], p["err"]
                ));
            }
        }
        same += g_same;
        summary.push_str(&format!(
            "  {g}: {} runs, {g_same} identical, {g_crash} refused where Python stops with a traceback\n",
            reqs.len()
        ));
    }
    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!(
        "hydo-build equivalence: {total} runs, {same} identical, {} Python tracebacks refused with a message ({crashes:?}), {} mismatches\n{summary}",
        crashes.values().sum::<usize>(),
        mismatches.len()
    );
    for m in mismatches.iter().take(25) {
        eprintln!("MISMATCH {m}\n");
    }
    assert!(mismatches.is_empty(), "{} mismatches", mismatches.len());
}
