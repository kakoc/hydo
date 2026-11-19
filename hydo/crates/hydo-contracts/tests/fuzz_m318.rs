//! Function fuzzing (Milestone 318): what the third evaluation of `hydo fuzz` found refused or
//! judged wrong, end to end on tests/fuzz-m318.

use crate::tmpdir::TmpDir;
use hydo_contracts::fuzz::{FnReport, Report};
use std::path::Path;

/// The fixture's report with `extra` options.
fn fuzz_m318(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m318");
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

#[test]
fn an_element_of_a_list_of_built_values_compares_as_its_read_value() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-built");
    let (_, rep) = fuzz_m318(t.path(), &["--fn", "bits", "--fn", "first"]);
    // half's `result[0] == self[0]` broke on every call: the element read as its constructor's
    // arguments
    let bits = one(&rep, "::bits");
    assert_eq!(bits.verdict, "held", "{bits:?}");
    assert!(bits.held.iter().all(|h| h.undecided == 0), "{bits:?}");
    // without a `read` line hydo cannot compare it: not judged, with why, never broken
    let first = one(&rep, "::first");
    assert_eq!(first.verdict, "not judged", "{first:?}");
    assert!(
        first.held[0].undecided_why.contains("`read` line"),
        "{first:?}"
    );
}

#[test]
fn values_of_two_kinds_are_not_compared() {
    use hydo_contracts::fuzz::check::{holds, Env};
    use hydo_contracts::fuzz::value::Val;
    use hydo_contracts::lang::parse_expr;
    let entry = vec![(
        "v".to_string(),
        Val::Struct(vec![("bits".into(), Val::Int(7))]),
    )];
    let env = Env {
        entry: &entry,
        after: &entry,
        result: Some(&Val::Int(7)),
    };
    // a structure is never equal to a number by accident: the clause is undecided, not false
    let why = holds(&parse_expr("result == v").unwrap(), &env).unwrap_err();
    assert!(why.contains("compares a number with a structure"), "{why}");
    assert!(holds(&parse_expr("result != v").unwrap(), &env).is_err());
    assert_eq!(holds(&parse_expr("result == 7").unwrap(), &env), Ok(true));
}
