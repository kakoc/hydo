//! `hydo migrate` (M114) on the project format's own example pair: `--check` reports and writes
//! nothing, a rewrite gives the canonical file byte for byte, and a second run changes nothing.

use std::path::PathBuf;
use std::process::Command;

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/examples/project/v1")
}

fn hydo(args: &[&str], dir: &std::path::Path) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_hydo"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run hydo");
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn migrate_rewrites_a_retired_spelling() {
    let d = std::env::temp_dir().join(format!("hydo-migrate-{}", std::process::id()));
    std::fs::create_dir_all(d.join("hydo")).unwrap();
    let old = examples().join("retired-history-check-in-test/hydo.yaml");
    let new = std::fs::read_to_string(
        examples().join("retired-history-check-in-test-migrated/hydo.yaml"),
    )
    .unwrap();
    let f = d.join("hydo/hydo.yaml");
    std::fs::copy(&old, &f).unwrap();
    let before = std::fs::read_to_string(&f).unwrap();

    let (rc, _, err) = hydo(&["migrate", "--check"], &d);
    assert_eq!(rc, 1, "{err}");
    assert!(
        err.contains("format: 1 added") && err.contains("would be rewritten"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(&f).unwrap(),
        before,
        "--check wrote"
    );

    let (rc, out, _) = hydo(&["migrate", "hydo/hydo.yaml", "--stdout"], &d);
    assert_eq!((rc, out.as_str()), (0, new.as_str()));

    let (rc, _, err) = hydo(&["migrate"], &d);
    assert_eq!(rc, 0, "{err}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), new);

    let (rc, _, err) = hydo(&["migrate", "--check"], &d);
    assert_eq!(rc, 0, "{err}");
    assert!(err.contains("already project format 1"), "{err}");

    let (rc, _, err) = hydo(&["migrate", "--bogus"], &d);
    assert_eq!(rc, 2, "{err}");
    let _ = std::fs::remove_dir_all(&d);
}
