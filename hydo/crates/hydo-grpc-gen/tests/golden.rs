//! The generator against the Go generator's recorded answers (tests/golden/, written by the Go
//! hydo-grpc-gen of Milestone 91 before it was deleted in Milestone 113):
//! - every file of four schemas (the demo, the stack, the kitchen sink, a torture schema with
//!   custom options, proto2 defaults, groups, extensions and imports), byte for byte, and the
//!   printed summary;
//! - 105 hints files against the demo schema: the exit status and the error message, or the
//!   plan's sha256.
//!
//! `HYDO_BLESS=1` rewrites the golden files from the Rust output (review the diff).

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

fn here() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repo() -> PathBuf {
    here().join("../../..")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "hydo-grpc-gen-golden-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn bless() -> bool {
    std::env::var("HYDO_BLESS").is_ok_and(|v| v == "1")
}

fn files(d: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    fn walk(base: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut ents: Vec<_> = fs::read_dir(d).unwrap().flatten().collect();
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push((
                    p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                    fs::read(&p).unwrap(),
                ));
            }
        }
    }
    walk(d, d, &mut out);
    out
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn schemas_like_go() {
    let demo = repo().join("hydo/grpcgen/demo");
    let stack = repo().join("hydo/stack");
    let td = here().join("tests/testdata");
    let cases: Vec<(&str, Vec<String>)> = vec![
        (
            "demo",
            vec![
                demo.join("proto").display().to_string(),
                "--hints".into(),
                demo.join("hints.yaml").display().to_string(),
            ],
        ),
        (
            "stack",
            vec![
                stack.join("proto").display().to_string(),
                "--hints".into(),
                stack.join("grpc-hints.yaml").display().to_string(),
            ],
        ),
        ("kitchen", vec![td.join("kitchen").display().to_string()]),
        ("torture", vec![td.join("torture").display().to_string()]),
    ];
    for (name, mut a) in cases {
        let t = tmp(name);
        let out = t.join("out");
        a.extend(args(&["--out", out.to_str().unwrap(), "--no-build"]));
        let summary = match hydo_grpc_gen::gen::run(&a, &Default::default()) {
            Ok(s) => s,
            Err(f) => panic!("{name}: {} (exit {})", f.0, f.1),
        };
        let summary = summary.replace(out.to_str().unwrap(), "OUT");
        let g = here().join("tests/golden/schemas");
        if bless() {
            let _ = fs::remove_dir_all(g.join(name));
            copy_dir(&out, &g.join(name));
            fs::write(g.join(format!("{name}.stdout")), &summary).unwrap();
        }
        assert_eq!(
            summary,
            fs::read_to_string(g.join(format!("{name}.stdout"))).unwrap(),
            "{name}: summary"
        );
        let got = files(&out);
        let want = files(&g.join(name));
        assert_eq!(
            got.iter().map(|f| &f.0).collect::<Vec<_>>(),
            want.iter().map(|f| &f.0).collect::<Vec<_>>(),
            "{name}: file list"
        );
        for ((n, a), (_, b)) in got.iter().zip(&want) {
            assert!(a == b, "{name}: {n} differs from the Go generator's");
        }
        // the generation is a pure function of its inputs
        let _ = fs::remove_dir_all(&out);
        hydo_grpc_gen::gen::run(&a, &Default::default())
            .map_err(|f| f.0)
            .unwrap();
        assert_eq!(files(&out), got, "{name}: two runs differ");
        fs::remove_dir_all(&t).unwrap();
    }
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

/// The Go run's stderr for one hints case: "hydo-grpc-gen: <msg>" for exit 1, the usage for 2.
fn expected_hints() -> Vec<(i32, String)> {
    let text = fs::read_to_string(here().join("tests/golden/hints/expected.txt")).unwrap();
    let mut out: Vec<(i32, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("== ") {
            let code: i32 = rest.split_whitespace().nth(2).unwrap().parse().unwrap();
            out.push((code, String::new()));
        } else {
            let last = out.last_mut().unwrap();
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    out
}

#[test]
fn hints_like_go() {
    let cases = fs::read_to_string(here().join("tests/golden/hints/cases.txt")).unwrap();
    let want = expected_hints();
    let plans: Vec<String> = fs::read_to_string(here().join("tests/golden/hints/plans.txt"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let demo = repo().join("hydo/grpcgen/demo/proto");
    let t = tmp("hints");
    fs::create_dir_all(t.join("rec")).unwrap();
    let hp = t.join("rec/h.yaml");
    let out = t.join("rec/hcase");
    let mut n = 0;
    for (i, case) in cases.lines().enumerate() {
        let i = i + 1;
        fs::write(&hp, format!("{case}\n")).unwrap();
        let a = args(&[
            demo.to_str().unwrap(),
            "--hints",
            hp.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--no-build",
        ]);
        let (code, err) = match hydo_grpc_gen::gen::run(&a, &Default::default()) {
            Ok(_) => (0, String::new()),
            Err(f) if f.1 == 2 => (2, format!("{}\n", f.0)),
            Err(f) => (f.1, format!("hydo-grpc-gen: {}\n", f.0)),
        };
        let err = err.replace(hp.to_str().unwrap(), "rec/h.yaml");
        assert_eq!(
            (code, &err),
            (want[i - 1].0, &want[i - 1].1),
            "case {i}: {case}"
        );
        if code == 0 {
            let sha = hex(&Sha256::digest(
                fs::read(out.join("grpc/plan.json")).unwrap(),
            ));
            assert!(
                plans.contains(&format!("plan {i} {sha}")),
                "case {i}: plan.json differs from the Go generator's: {case}"
            );
        }
        n += 1;
    }
    assert_eq!(n, 105);
    fs::remove_dir_all(&t).unwrap();
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `--descriptor-set`: sets written by protoc 36.2 (`--include_imports`, one with source info)
/// and by the Go generator itself, read back as the Go generator read them (custom options of a
/// decoded set come after the known fields, as unknown fields of Go's descriptorpb).
#[test]
fn descriptor_sets_like_go() {
    let g = here().join("tests/golden/ds");
    for name in [
        "go-torture",
        "protoc-torture",
        "protoc-demo",
        "protoc-kitchen",
    ] {
        let t = tmp(&format!("ds-{name}"));
        let out = t.join("out");
        let input = g.join(format!("{name}.in.pb"));
        let a = args(&[
            "--descriptor-set",
            input.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--no-build",
        ]);
        hydo_grpc_gen::gen::run(&a, &Default::default())
            .map_err(|f| f.0)
            .unwrap();
        let got = fs::read(out.join("grpc/desc.pb")).unwrap();
        if bless() {
            fs::write(g.join(format!("{name}.out.pb")), &got).unwrap();
        }
        assert!(
            got == fs::read(g.join(format!("{name}.out.pb"))).unwrap(),
            "{name}: the schema differs from the Go generator's"
        );
        let plan = fs::read_to_string(out.join("grpc/plan.json")).unwrap();
        assert!(
            plan.contains(&format!("\"descriptor-set:{name}.in.pb\"")),
            "{name}: sources"
        );
        fs::remove_dir_all(&t).unwrap();
    }
    // the demo compiled by protoc and by the generator: one schema
    assert_eq!(
        fs::read(g.join("protoc-demo.out.pb")).unwrap(),
        fs::read(here().join("tests/golden/schemas/demo/grpc/desc.pb")).unwrap()
    );
    let t = tmp("ds-bad");
    let bad = here().join("tests/golden/hints/cases.txt");
    let a = args(&[
        "--descriptor-set",
        bad.to_str().unwrap(),
        "--out",
        t.join("o").to_str().unwrap(),
        "--no-build",
    ]);
    let e = hydo_grpc_gen::gen::run(&a, &Default::default())
        .err()
        .unwrap();
    assert_eq!(
        (e.1, e.0),
        (
            1,
            format!(
                "{}: not a FileDescriptorSet: proto: cannot parse invalid wire-format data",
                bad.display()
            )
        )
    );
    fs::remove_dir_all(&t).unwrap();
}
