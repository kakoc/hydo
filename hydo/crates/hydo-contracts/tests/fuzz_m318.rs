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
        first.held[0].undecided_why.contains("// hydo: read G as"),
        "{first:?}"
    );
}

#[test]
fn associated_result_types_are_read_and_an_unread_result_is_named() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-assoc");
    let (_, rep) = fuzz_m318(
        t.path(),
        &["--fn", "shout", "--fn", "measure", "--fn", "index"],
    );
    // heck's `Self::Owned` is a `String` for `str`
    let shout = one(&rep, "<str as Shout>::shout");
    assert_eq!(shout.verdict, "held", "{shout:?}");
    assert!(shout.held[0].reached.unwrap_or(0) > 0, "{shout:?}");
    // `type Out = u16;` in the crate's `impl`
    assert_eq!(one(&rep, "<u8 as Measure>::measure").verdict, "held");
    // a result hydo does not read: not judged, with its type, and no advice about the inputs
    let index = one(&rep, "<u8 as Index>::index");
    assert_eq!(index.verdict, "not judged", "{index:?}");
    assert!(index.held[0].undecided_why.contains("HashMap"), "{index:?}");
    assert!(index.held[0].reached_undecided > 0, "{index:?}");
    assert!(
        index.advice.iter().all(|a| !a.contains("shape")),
        "{index:?}"
    );
}

#[test]
fn a_name_that_two_modules_define_resolves_by_the_module_its_use_lines_and_a_full_path() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-names");
    let (_, rep) = fuzz_m318(
        t.path(),
        &["--fn", "parse", "--fn", "check", "--fn", "parse_full"],
    );
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    // the function's own module
    assert_eq!(one(&rep, "duration::parse").verdict, "held");
    // a `use` line of the function's file
    assert_eq!(one(&rep, "user::check").verdict, "held");
    // a full path
    assert_eq!(one(&rep, "::parse_full").verdict, "held");
}

#[test]
fn const_generics_static_references_and_closures_are_fuzzed() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-shapes");
    let fns = [
        "head",
        "total",
        "keep",
        "tag",
        "map_left",
        "left_or_else",
        "count_if",
        "tally",
    ];
    let mut args: Vec<&str> = Vec::new();
    for f in &fns {
        args.extend(["--fn", f]);
    }
    let (_, rep) = fuzz_m318(t.path(), &args);
    // a const generic parameter takes its value from a `with` line
    assert_eq!(one(&rep, "::head").verdict, "held", "{rep:?}");
    assert_eq!(one(&rep, "::[u8; 3]::total").verdict, "held");
    // a `&'static` parameter gets a leaked value
    assert_eq!(one(&rep, "::keep").verdict, "held");
    assert_eq!(one(&rep, "::tag").verdict, "held");
    // a closure parameter takes a generated closure
    assert_eq!(one(&rep, "::map_left").verdict, "held");
    assert_eq!(one(&rep, "::left_or_else").verdict, "held");
    let count = one(&rep, "::count_if");
    assert_eq!(count.verdict, "held");
    // a closure whose result hydo cannot generate is refused with advice that works
    let tally = rep
        .skipped
        .iter()
        .find(|s| s.contains("::tally"))
        .unwrap_or_else(|| panic!("{:?}", rep.skipped));
    assert!(tally.contains("property function"), "{tally}");
    assert!(!tally.contains("with F = i64"), "{tally}");
}

#[test]
fn a_generated_closure_shows_and_reads_back_the_same() {
    use hydo_contracts::fuzz::rust::{fn_bound, Closure, Ctor, FTy, Pass};
    use hydo_contracts::fuzz::value::{parse, show, Val};
    let b = fn_bound("FnOnce(i32) -> i64").unwrap();
    let int = |n: &str, bits| FTy::Int {
        name: n.into(),
        signed: true,
        bits,
    };
    let c = Closure::new(&b, Some(&int("i32", 32)), &int("i64", 64));
    let t = FTy::Built {
        ty: "_".into(),
        build: Some(Ctor {
            path: c.path(),
            args: vec![
                ("shape".into(), int("u8", 8), Pass::Value),
                ("k".into(), int("i64", 64), Pass::Value),
            ],
        }),
        read: None,
    };
    for (i, text) in [
        (0, "|_| 5"),
        (1, "|x| try_from(x).unwrap_or(5)"),
        (2, "|x| try_from(x).unwrap_or(5).wrapping_add(1)"),
    ] {
        let v = Val::Struct(vec![
            ("shape".into(), Val::Int(i)),
            ("k".into(), Val::Int(5)),
        ]);
        assert_eq!(show(&v, &t), text);
        assert_eq!(parse(text, &t), Ok(v));
    }
}

#[test]
fn the_default_strings_hold_characters_whose_case_changes_them() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-case");
    let (code, rep) = fuzz_m318(
        t.path(),
        &["--fn", "title_is_idempotent", "--cases", "1000"],
    );
    assert_eq!(code, 1, "{rep:?}");
    let f = one(&rep, "::title_is_idempotent");
    assert_eq!(f.verdict, "broken", "{f:?}");
    // the shrink ends at one character whose upper case is two or three letters
    let input = f.broken[0].input.as_str();
    assert!(
        ["s = \"ß\"", "s = \"ﬄ\"", "s = \"ŉ\""].contains(&input),
        "{input}"
    );
}

#[test]
fn read_lines_collect_take_a_turbofish_and_build_lines_take_a_generic_type() {
    let t = TmpDir::new("hydo-contracts-fuzz-m318-read");
    let (_, rep) = fuzz_m318(
        t.path(),
        &["--fn", "iter_of", "--fn", "iter2_of", "--fn", "try_push"],
    );
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    assert_eq!(one(&rep, "::iter_of").verdict, "held");
    assert_eq!(one(&rep, "::iter2_of").verdict, "held");
    let push = one(&rep, "::try_push");
    assert_eq!(push.verdict, "held", "{push:?}");
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
