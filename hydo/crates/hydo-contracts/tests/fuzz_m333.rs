//! Function fuzzing (Milestone 333): what the fourth evaluation of `hydo fuzz` found, end to
//! end on tests/fuzz-m333.

use crate::tmpdir::TmpDir;
use hydo_contracts::fuzz::{FnReport, Report};
use std::path::Path;

/// The fixture's report with `extra` options.
fn fuzz_m333(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m333");
    let out = dir.join("out");
    let mut args: Vec<String> = vec![
        fixture.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
        "--cases".into(),
        "300".into(),
    ];
    args.extend(extra.iter().map(|s| s.to_string()));
    let code = hydo_contracts::fuzz::main(&args);
    let text = std::fs::read_to_string(out.join("fuzz.json")).unwrap_or_default();
    (code, serde_json::from_str(&text).unwrap_or_default())
}

fn one<'a>(rep: &'a Report, name: &str) -> &'a FnReport {
    rep.functions
        .iter()
        .find(|f| f.function.ends_with(name))
        .unwrap_or_else(|| panic!("{name}: {rep:?}"))
}

#[allow(dead_code)]
fn skipped<'a>(rep: &'a Report, name: &str) -> &'a str {
    rep.skipped
        .iter()
        .find(|s| s.contains(name))
        .unwrap_or_else(|| panic!("{name}: {:?}", rep.skipped))
}

/// The fixture's functions run apart, with `--fn` for each name.
fn run_fns(t: &TmpDir, fns: &[&str], extra: &[&str]) -> (i32, Report) {
    let mut args: Vec<&str> = Vec::new();
    for f in fns {
        args.extend(["--fn", f]);
    }
    args.extend(extra);
    fuzz_m333(t.path(), &args)
}

#[test]
fn a_comparison_with_values_not_read_is_not_judged_never_broken() {
    let t = TmpDir::new("hydo-contracts-fuzz-m333-unread");
    let (_, rep) = run_fns(&t, &["wrapped", "wrapped_some"], &[]);
    for f in ["::wrapped", "::wrapped_some"] {
        let r = one(&rep, f);
        assert_eq!(r.verdict, "not judged", "{r:?}");
        assert!(r.broken.is_empty(), "{r:?}");
        let why = &r.held[0].undecided_why;
        assert!(why.contains("Wrapping<u8>"), "{why}");
    }
}
