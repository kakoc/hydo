//! Every test of the workspace is in a test tier (Milestone 171; docs/CI.md, "Test tiers").
//!
//! `ci/test-tiers.txt` names every test binary of the workspace, as cargo-nextest names it, with
//! its tier:
//!
//! - `fast`: run on every push and pull request (for the packages the change can affect,
//!   `ci/affected-packages.sh`) and in the full tier;
//! - `full`: run only in the full tier: nightly, on demand, and before a release
//!   (docs/RELEASING.md step 1), which runs every test of the workspace with
//!   `cargo test --locked --release --workspace`.
//!
//! The ids: `<package>` for a library's unit tests, `<package>::bin/<name>` for a program's,
//! `<package>::<name>` for an integration test (tests/<name>.rs), `<package>::example/<name>` and
//! `<package>::bench/<name>` for an example or bench with `test = true`, and `<package>::doctest`
//! for a library's documentation examples (cargo-nextest does not run those, so they are `full`).
//!
//! This test fails when a test binary of the workspace has no line (a new tests/<name>.rs, a new
//! program), when a line names a binary that does not exist, when a line is doubled or its tier is
//! unknown, and when a library with `doctest = false` has a documentation example that rustdoc
//! would test (it would never run).

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn top() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

fn metadata() -> Value {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--offline",
        ])
        .current_dir(top())
        .output()
        .expect("cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn kinds(t: &Value) -> Vec<&str> {
    t["kind"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k.as_str().unwrap())
        .collect()
}

/// The test binaries of the workspace (nextest's ids) and, per library with `doctest = false`,
/// its source directory.
fn binaries(meta: &Value) -> (Vec<String>, Vec<(String, PathBuf)>) {
    let members: Vec<&str> = meta["workspace_members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    let mut ids = Vec::new();
    let mut no_doctest = Vec::new();
    for p in meta["packages"].as_array().unwrap() {
        if !members.contains(&p["id"].as_str().unwrap()) {
            continue;
        }
        let pkg = p["name"].as_str().unwrap();
        for t in p["targets"].as_array().unwrap() {
            let name = t["name"].as_str().unwrap();
            let k = kinds(t);
            let lib = k
                .iter()
                .any(|k| matches!(*k, "lib" | "rlib" | "proc-macro" | "dylib"));
            if t["test"].as_bool().unwrap_or(false) {
                let id = if lib {
                    pkg.to_string()
                } else if k.contains(&"bin") {
                    format!("{pkg}::bin/{name}")
                } else if k.contains(&"test") {
                    format!("{pkg}::{name}")
                } else if k.contains(&"example") {
                    format!("{pkg}::example/{name}")
                } else if k.contains(&"bench") {
                    format!("{pkg}::bench/{name}")
                } else {
                    panic!("{pkg}: a test target of kind {k:?}")
                };
                ids.push(id);
            }
            if lib {
                if t["doctest"].as_bool().unwrap_or(false) {
                    ids.push(format!("{pkg}::doctest"));
                } else {
                    let src = PathBuf::from(t["src_path"].as_str().unwrap());
                    no_doctest.push((pkg.to_string(), src.parent().unwrap().to_path_buf()));
                }
            }
        }
    }
    ids.sort();
    (ids, no_doctest)
}

fn tiers() -> BTreeMap<String, String> {
    let path = top().join("ci/test-tiers.txt");
    let text = std::fs::read_to_string(&path).unwrap();
    let mut m = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        assert_eq!(
            f.len(),
            2,
            "ci/test-tiers.txt:{}: `<id> <tier>`: {l}",
            n + 1
        );
        assert!(
            matches!(f[1], "fast" | "full"),
            "ci/test-tiers.txt:{}: the tier is fast or full: {l}",
            n + 1
        );
        assert!(
            m.insert(f[0].to_string(), f[1].to_string()).is_none(),
            "ci/test-tiers.txt:{}: {} is listed twice",
            n + 1,
            f[0]
        );
    }
    m
}

#[test]
fn every_test_binary_is_in_a_tier() {
    let (ids, _) = binaries(&metadata());
    let t = tiers();
    let missing: Vec<&String> = ids.iter().filter(|i| !t.contains_key(*i)).collect();
    let stale: Vec<&String> = t.keys().filter(|i| !ids.contains(i)).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "ci/test-tiers.txt is not the workspace's list of test binaries.\n\
         Not in a tier (add a line `<id> fast`, or `full` for a slow test whose code rarely \
         changes; docs/CI.md \"Test tiers\"): {missing:?}\n\
         Listed but not a test binary any more (remove the line): {stale:?}"
    );
    for (id, tier) in &t {
        if id.ends_with("::doctest") {
            assert_eq!(
                tier, "full",
                "{id}: cargo-nextest does not run documentation examples, so they are `full`"
            );
        }
    }
}

/// Whether a fenced block's info string makes rustdoc test it (Rust code that is not `ignore`d).
fn is_tested_example(info: &str) -> bool {
    const RUST: &[&str] = &[
        "rust",
        "no_run",
        "should_panic",
        "compile_fail",
        "test_harness",
        "standalone_crate",
        "edition2015",
        "edition2018",
        "edition2021",
        "edition2024",
    ];
    let tokens: Vec<&str> = info
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();
    tokens.iter().all(|t| RUST.contains(t))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn no_documentation_example_is_switched_off() {
    let (_, no_doctest) = binaries(&metadata());
    let mut found = Vec::new();
    for (pkg, dir) in no_doctest {
        let mut files = Vec::new();
        rust_files(&dir, &mut files);
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap();
            let mut open = false;
            for (n, l) in text.lines().enumerate() {
                let l = l.trim_start();
                let Some(doc) = l.strip_prefix("///").or_else(|| l.strip_prefix("//!")) else {
                    continue;
                };
                let Some(info) = doc.trim_start().strip_prefix("```") else {
                    continue;
                };
                if open {
                    open = false;
                    continue;
                }
                open = true;
                if is_tested_example(info.trim()) {
                    found.push(format!("{pkg}: {}:{}", f.display(), n + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "documentation examples in libraries with `doctest = false` (they would never run): \
         {found:#?}\nRemove `doctest = false` from the package's [lib] and add `<package>::doctest \
         full` to ci/test-tiers.txt, or mark the block `text` or `ignore`."
    );
}

#[test]
fn the_tier_check_sees_documentation_examples() {
    assert!(is_tested_example(""));
    assert!(is_tested_example("no_run"));
    assert!(is_tested_example("rust,should_panic"));
    assert!(!is_tested_example("text"));
    assert!(!is_tested_example("ignore"));
    assert!(!is_tested_example("sh"));
    assert!(!is_tested_example("rust,ignore"));
}
