//! The engine version a plan's `hydo_version:` is checked against (`composer::engine_version`):
//! the `VERSION` file next to the product's onboard/ directory, else the version the program is
//! built with. A hydo built in a checkout has no onboard/ directory next to it; until the
//! TigerBeetle onboarding it then said "this hydo is 0.0.0" and refused every plan.

use hydo_manifest::composer::{check_version, engine_version};
use serde_json::json;

#[test]
fn a_release_version_file_wins() {
    let d = std::env::temp_dir().join(format!("hydo-engine-version-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("hydo/onboard")).unwrap();
    std::fs::write(d.join("hydo/VERSION"), "7.8.9\n").unwrap();
    assert_eq!(engine_version(&d.join("hydo/onboard")), "7.8.9");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn without_a_version_file_the_built_version_counts() {
    let none = std::path::Path::new("/nonexistent-hydo-root/hydo/onboard");
    let v = engine_version(none);
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
    assert_ne!(v, "0.0.0");
    // the tree's hydo/VERSION is the workspace version, so the fallback names the same release
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../onboard");
    assert_eq!(engine_version(&tree), v);
    // and a plan pinned to this major.minor builds
    let mm: Vec<&str> = v.split('.').take(2).collect();
    let mut warn = Vec::new();
    check_version(&json!(mm.join(".")), &v, false, &mut warn).unwrap();
}
