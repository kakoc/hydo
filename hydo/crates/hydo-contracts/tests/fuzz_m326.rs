//! Function fuzzing (Milestone 326): what the third evaluation of `hydo fuzz` left after
//! Milestone 318, end to end on tests/fuzz-m326.

use crate::tmpdir::TmpDir;
use hydo_contracts::fuzz::{FnReport, Report};
use std::path::Path;

/// The fixture's report with `extra` options.
fn fuzz_m326(dir: &Path, extra: &[&str]) -> (i32, Report) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m326");
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

#[allow(dead_code)]
fn one<'a>(rep: &'a Report, name: &str) -> &'a FnReport {
    rep.functions
        .iter()
        .find(|f| f.function.ends_with(name))
        .unwrap_or_else(|| panic!("{name}: {rep:?}"))
}

fn skipped<'a>(rep: &'a Report, name: &str) -> &'a str {
    rep.skipped
        .iter()
        .find(|s| s.contains(name))
        .unwrap_or_else(|| panic!("{name}: {:?}", rep.skipped))
}

#[test]
fn the_bit_operators_read_as_in_rust() {
    let t = TmpDir::new("hydo-contracts-fuzz-m326-bits");
    let fns = [
        "is_nan",
        "flip",
        "flip_signed",
        "times_four",
        "both",
        "low_bits",
    ];
    let mut args: Vec<&str> = Vec::new();
    for f in &fns {
        args.extend(["--fn", f]);
    }
    let (code, rep) = fuzz_m326(t.path(), &args);
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    for f in &fns[..5] {
        let r = one(&rep, &format!("::{f}"));
        assert_eq!(r.verdict, "held", "{r:?}");
        assert!(r.held.iter().all(|h| h.undecided == 0), "{r:?}");
    }
    // the planted mask breaks the clause, and the shrink keeps one of bits 8 to 11
    let low = one(&rep, "::low_bits");
    assert_eq!(low.verdict, "broken", "{low:?}");
    assert_eq!(code, 1);
    // precedence as in Rust: `&` before `==`, the shifts before `&`, `|` loosest
    use hydo_contracts::lang::parse_expr;
    use hydo_contracts::plan::BinOp;
    match parse_expr("a & b == c").unwrap() {
        hydo_contracts::lang::Ast::Bin(BinOp::Eq, l, _) => {
            assert!(matches!(
                *l,
                hydo_contracts::lang::Ast::Bin(BinOp::BitAnd, ..)
            ))
        }
        e => panic!("{e:?}"),
    }
    match parse_expr("a | b ^ c & d << 1").unwrap() {
        hydo_contracts::lang::Ast::Bin(BinOp::BitOr, _, r) => match *r {
            hydo_contracts::lang::Ast::Bin(BinOp::BitXor, _, r) => {
                assert!(matches!(
                    *r,
                    hydo_contracts::lang::Ast::Bin(BinOp::BitAnd, ..)
                ))
            }
            e => panic!("{e:?}"),
        },
        e => panic!("{e:?}"),
    }
    assert_eq!(BinOp::Shl.bits(1, 127), None);
    assert_eq!(BinOp::Shl.bits(3, 4), Some(48));
    assert_eq!(BinOp::Shr.bits(-8, 1), Some(-4));
    assert_eq!(BinOp::Shr.bits(5, 200), Some(0));
    // a run's contracts read them too
    use hydo_contracts::eval::{eval, V};
    use hydo_contracts::plan::{Expr, Ty};
    let e = Expr::Bin(
        BinOp::BitAnd,
        Box::new(Expr::In(0, Ty::U(2))),
        Box::new(Expr::Int("31744".into())),
    );
    assert_eq!(eval(&e, &[Some(0xfc01)], None), Ok(V::Int(0x7c00)));
}

#[test]
fn two_lists_of_one_length_are_generated_and_shrunk_together() {
    let t = TmpDir::new("hydo-contracts-fuzz-m326-lens");
    let (_, rep) = fuzz_m326(t.path(), &["--fn", "halve_into", "--fn", "halve_into_bad"]);
    // half's `requires len(self) == len(src)` turned away 99 % of the inputs
    let ok = one(&rep, "::halve_into");
    assert_eq!(ok.verdict, "held", "{ok:?}");
    assert_eq!(ok.rejected, 0, "{ok:?}");
    assert!(ok.calls >= 300, "{ok:?}");
    // the shrink shortens both lists to the third element that the bug needs
    let bad = one(&rep, "::halve_into_bad");
    assert_eq!(bad.verdict, "broken", "{bad:?}");
    assert_eq!(bad.rejected, 0, "{bad:?}");
    let input = &bad.broken[0].input;
    assert_eq!(input, "dst = [0, 0, 0], src = [0, 0, 2]", "{bad:?}");
}

#[test]
fn patterns_take_named_fields_and_two_dots() {
    let t = TmpDir::new("hydo-contracts-fuzz-m326-pats");
    let (_, rep) = fuzz_m326(t.path(), &["--fn", "unit", "--fn", "unit_misspelt"]);
    let u = one(&rep, "units::unit");
    assert_eq!(u.verdict, "held", "{u:?}");
    assert!(u.held.iter().all(|h| h.undecided == 0), "{u:?}");
    let bad = skipped(&rep, "::unit_misspelt");
    assert!(
        bad.contains("`UnknownUnit` has no field `begin`; its fields are `start`, `end`"),
        "{bad}"
    );
    use hydo_contracts::lang::{parse_expr, Pat};
    let p = match parse_expr("matches(r, Err(E::U { start: 0, end, .. }))").unwrap() {
        hydo_contracts::lang::Ast::Matches(_, p) => p,
        e => panic!("{e:?}"),
    };
    assert_eq!(
        p,
        Pat::Variant(
            "Err".into(),
            Some(vec![Pat::Fields(
                "U".into(),
                vec![("start".into(), Pat::Int(0)), ("end".into(), Pat::Any)]
            )])
        )
    );
    assert_eq!(p.to_string(), "Err(U { start: 0, end: _, .. })");
    // `{ .. }` is any fields
    assert!(matches!(
        parse_expr("matches(r, E::U { .. })").unwrap(),
        hydo_contracts::lang::Ast::Matches(_, Pat::Variant(_, None))
    ));
    assert!(parse_expr("matches(r, E::U { .. , x })").is_err());
}

#[test]
fn fn_names_a_module_or_a_path_prefix() {
    use hydo_contracts::fuzz::names_fn;
    let f = "pkg::codec::hex::encode";
    for w in [
        "encode",
        "hex::encode",
        "codec::",
        "codec::hex::",
        "crate::codec::",
        "hex::",
        "pkg::codec::",
    ] {
        assert!(names_fn(f, w), "{w}");
    }
    for w in ["code", "dec::", "codec", "encode::"] {
        assert!(!names_fn(f, w), "{w}");
    }
    assert!(names_fn("pkg::Ledger::apply", "Ledger::"));
    // a module's name alone selects its functions when no function has that name
    let t = TmpDir::new("hydo-contracts-fuzz-m326-module");
    let (_, rep) = fuzz_m326(t.path(), &["--fn", "units"]);
    let mut got: Vec<&str> = rep.functions.iter().map(|f| f.function.as_str()).collect();
    got.sort();
    assert_eq!(got, ["fuzz-m326::units::unit"], "{rep:?}");
    assert!(
        rep.skipped
            .iter()
            .any(|s| s.contains("units::unit_misspelt")),
        "{:?}",
        rep.skipped
    );
    let (_, rep) = fuzz_m326(t.path(), &["--fn", "units::"]);
    assert_eq!(rep.functions.len(), 1, "{rep:?}");
}

/// A copy of the fixture at `dir/crate`, with `edit` applied to its `src/lib.rs`.
fn fixture_copy(dir: &Path, edit: &dyn Fn(String) -> String) -> std::path::PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m326");
    let to = dir.join("crate");
    std::fs::create_dir_all(to.join("src")).unwrap();
    std::fs::copy(from.join("Cargo.toml"), to.join("Cargo.toml")).unwrap();
    let lib = std::fs::read_to_string(from.join("src/lib.rs")).unwrap();
    std::fs::write(to.join("src/lib.rs"), edit(lib)).unwrap();
    to
}

#[test]
fn a_private_field_names_the_build_line_with_the_crates_constructor() {
    let t = TmpDir::new("hydo-contracts-fuzz-m326-private");
    let list = |args: &[&str]| -> Vec<hydo_contracts::fuzz::Skipped> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz-m326");
        let pkg = fixture.canonicalize().unwrap();
        let sources = vec![pkg.join("src/lib.rs")];
        let (_, skipped, _) = hydo_contracts::fuzz::find(&pkg, &sources, "fuzz-m326");
        skipped
            .into_iter()
            .filter(|s| args.iter().any(|a| s.name.ends_with(a)))
            .collect()
    };
    let why = |name: &str| -> String {
        list(&[name])
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{name}"))
            .why
    };
    // `new` first, then `from_*` and `From`; a private function is not proposed; a `read` line
    // with a method that reads the value
    let w = why("::widths");
    assert!(
        w.contains("write `// hydo: build B from bits::B::new(x: u8)`"),
        "{w}"
    );
    assert!(w.contains("`bits::B::from_bits(bits: u16)`"), "{w}");
    assert!(w.contains("`bits::B::from(v: u8)`"), "{w}");
    assert!(!w.contains("secret"), "{w}");
    assert!(w.contains("`// hydo: read B as u16 by to_bits()`"), "{w}");
    // `#[derive(Default)]`
    let d = why("::d_of");
    assert!(
        d.contains("`// hydo: build D from bits::D::default()`"),
        "{d}"
    );
    // no constructor: what to add instead
    let b = why("::bare_of");
    assert!(b.contains("the crate has none that hydo can call"), "{b}");
    assert!(b.contains("`// hydo: read Bare as u8 by n()`"), "{b}");
    // the proposed lines work as written
    let line = w
        .split("write `")
        .nth(1)
        .and_then(|r| r.split('`').next())
        .unwrap()
        .to_string();
    let read = w
        .split("and `")
        .nth(1)
        .and_then(|r| r.split('`').next())
        .unwrap()
        .to_string();
    let krate = fixture_copy(t.path(), &|lib| {
        lib.replace(
            "// hydo: ensures len(result) == len(v)\npub fn widths",
            &format!(
                "{line}\n{read}\n// hydo: ensures len(result) == len(v)\n\
                 // hydo: ensures len(v) == 0 || result[0] == v[0]\npub fn widths"
            ),
        )
    });
    let out = t.path().join("out2");
    let code = hydo_contracts::fuzz::main(&[
        krate.to_string_lossy().into(),
        "--out".into(),
        out.to_string_lossy().into(),
        "--cases".into(),
        "200".into(),
        "--fn".into(),
        "widths".into(),
    ]);
    let rep: Report =
        serde_json::from_str(&std::fs::read_to_string(out.join("fuzz.json")).unwrap()).unwrap();
    assert_eq!(code, 0, "{rep:?}");
    assert_eq!(one(&rep, "::widths").verdict, "held", "{rep:?}");
}

#[test]
fn a_harness_that_does_not_build_names_the_contract_line_and_the_compiler_help() {
    let t = TmpDir::new("hydo-contracts-fuzz-m326-build");
    let (_, rep) = fuzz_m326(
        t.path(),
        &["--fn", "echo", "--fn", "code_of", "--fn", "same"],
    );
    // a `read` line that names a method the type does not have
    let echo = skipped(&rep, "::echo");
    assert!(echo.contains("error[E0599]"), "{echo}");
    assert!(
        echo.contains(
            "caused by the line `// hydo: read text::Text as String by as_text()` (src/lib.rs:28)"
        ),
        "{echo}"
    );
    // one error at the values before and after the call is one error
    assert!(!echo.contains("more error"), "{echo}");
    // a `build` line whose argument the constructor does not take: the compiler's error at the
    // line, with its help, and no value that does not fit
    let code = skipped(&rep, "::code_of");
    assert!(code.contains("error[E0308]: mismatched types"), "{code}");
    assert!(
        code.contains("caused by the line `// hydo: build code::Code from code::Code::new(x: u16)` (src/lib.rs:49)"),
        "{code}"
    );
    assert!(code.contains("expected `u8`, found `u16`"), "{code}");
    assert!(code.contains("help: "), "{code}");
    // a `with` line whose type does not fit the bound
    let same = skipped(&rep, "::same");
    assert!(same.contains("error[E0277]"), "{same}");
    assert!(
        same.contains("`// hydo: with T = String` (line 57)"),
        "{same}"
    );
    // the harness's line numbers mean nothing to the user: no column of the harness's line
    assert!(!same.contains(":59:"), "{same}");
    // the whole output, kept apart for each function, and every build in build.log
    for (s, name) in [(echo, "echo"), (code, "code_of"), (same, "same")] {
        let log = t.path().join("out/harness").join(format!("{name}.log"));
        assert!(s.contains(&log.display().to_string()), "{s}");
        assert!(s.contains("--show-harness"), "{s}");
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("The compiler's output:"), "{text}");
        assert!(text.contains("-->"), "{text}");
        assert!(
            t.path()
                .join("out/harness")
                .join(format!("{name}.rs"))
                .is_file(),
            "{name}"
        );
    }
    let all = std::fs::read_to_string(t.path().join("out/build.log")).unwrap();
    assert!(all.contains(" of the copy: failed"), "{all}");
    assert!(all.contains("mismatched types"), "{all}");
}
