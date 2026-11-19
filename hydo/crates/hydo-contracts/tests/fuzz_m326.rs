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
