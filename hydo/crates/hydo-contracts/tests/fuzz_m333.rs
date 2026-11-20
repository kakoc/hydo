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

#[test]
fn an_error_with_a_field_of_a_private_module_builds_and_reads() {
    let t = TmpDir::new("hydo-contracts-fuzz-m333-perr");
    let (_, rep) = run_fns(&t, &["digits"], &[]);
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    let r = one(&rep, "::digits");
    assert_eq!(r.verdict, "held", "{r:?}");
    assert!(r.held.iter().all(|h| h.undecided == 0), "{r:?}");
}

#[test]
fn constructors_without_arguments_and_fallible_ones_build() {
    let t = TmpDir::new("hydo-contracts-fuzz-m333-ctors");
    let (_, rep) = run_fns(&t, &["req_len", "pre_text", "tiny_of", "num_zero"], &[]);
    // a constructor without arguments
    let r = one(&rep, "::req_len");
    assert_eq!(r.verdict, "held", "{r:?}");
    // one that returns a `Result`, and one that returns an `Option`: an `Err` or a `None`
    // throws the input away, counted apart from `requires`
    for f in ["::pre_text", "::tiny_of"] {
        let r = one(&rep, f);
        assert_eq!(r.verdict, "held", "{r:?}");
        let z = r.rejected_by.iter().find(|x| x.line == 0);
        assert!(z.is_some_and(|z| z.inputs > 0), "{r:?}");
        assert!(r.calls + z.unwrap().inputs <= r.cases, "{r:?}");
        assert!(r.calls >= 30, "{r:?}");
    }
    // `)?` says so of a constructor that hydo does not find in the crate's sources
    let (cs, errs) =
        hydo_contracts::lang::scan("// hydo: build A from other::a(x: u8)?\npub fn f() {}\n");
    assert!(errs.is_empty(), "{errs:?}");
    assert!(cs[0].builds[0].fallible, "{cs:?}");
    assert_eq!(cs[0].builds[0].ctor, "other::a");
    // the refusal proposes only lines that build
    let why = skipped(&rep, "num_zero");
    assert!(!why.contains("new_unchecked"), "{why}");
    assert!(why.contains("num::Num::try_new(x: u8)"), "{why}");
    assert!(why.contains("<num::Num as From<u8>>::from(v: u8)"), "{why}");
}

#[test]
fn an_associated_type_of_a_type_parameter_reads_through_its_with_line() {
    let t = TmpDir::new("hydo-contracts-fuzz-m333-assoc");
    let (_, rep) = run_fns(&t, &["small::"], &[]);
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    // a parameter (`push`), a result (`pop`, `into_vec`), and a closure's argument (`retain`)
    for f in ["::push", "::pop", "::into_vec", "::retain"] {
        let r = one(&rep, f);
        assert_eq!(r.verdict, "held", "{r:?}");
        assert!(r.held.iter().all(|h| h.undecided == 0), "{r:?}");
    }
    // `<A as Store>::Item` is `A::Item`
    use hydo_contracts::fuzz::rust::{parse_ty, TyAst};
    assert_eq!(
        parse_ty("<A as Store>::Item").unwrap(),
        TyAst::Path(vec!["A".into(), "Item".into()], vec![])
    );
}
