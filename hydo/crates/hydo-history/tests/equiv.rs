//! hydo-history against hydo/history/hydohist.py (kept as the reference until every port is
//! done): the histories of the Python unit tests and a seeded random corpus (valid and violating
//! register, CAS and list-append histories, single- and multi-key, with transactions, malformed
//! lines, check declarations and final statuses) must give byte-identical answers. Larger runs:
//! tests/equiv.py (docs/HISTORY.md, M105).

use std::path::{Path, PathBuf};
use std::process::Command;

fn python_available() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("import sys; sys.exit(sys.version_info < (3, 9))")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("hydo-hist-eq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn py(args: &[&str]) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/equiv.py");
    let st = Command::new("python3")
        .arg(&script)
        .args(args)
        .status()
        .expect("python3");
    assert!(st.success(), "equiv.py {args:?}");
}

fn compare(d: &Path, name: &str) -> usize {
    let cases = d.join(format!("{name}.jsonl"));
    let want = d.join(format!("{name}.py.jsonl"));
    py(&["py", cases.to_str().unwrap(), want.to_str().unwrap()]);
    let cases = std::fs::read_to_string(&cases).unwrap();
    let want = std::fs::read_to_string(&want).unwrap();
    let cases: Vec<&str> = cases.lines().filter(|l| !l.trim().is_empty()).collect();
    let want: Vec<&str> = want.lines().collect();
    assert_eq!(cases.len(), want.len());
    for (i, (c, w)) in cases.iter().zip(&want).enumerate() {
        let (got, _) = hydo_history::equiv::answer(c);
        assert_eq!(&got, w, "{name} case {i} differs: {c}");
    }
    cases.len()
}

#[test]
fn answers_match_python() {
    if !python_available() {
        eprintln!("SKIPPED: python3 >= 3.9 is needed to run hydohist.py");
        return;
    }
    let d = scratch();
    py(&["units", d.join("units.jsonl").to_str().unwrap()]);
    let n = compare(&d, "units");
    py(&[
        "gen",
        d.join("rand.jsonl").to_str().unwrap(),
        "--n",
        "400",
        "--seed",
        "105",
    ]);
    let m = compare(&d, "rand");
    eprintln!("identical answers: {n} unit-test cases, {m} random cases");
    let _ = std::fs::remove_dir_all(&d);
}
