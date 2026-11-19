//! The parts of `hydo update` that need no program around them: the release list, the release
//! notes' section, how an install is recognised from its path, and `hydo doctor`'s daily look.
//! The whole command, with a fake release server and fake brew, cargo and claude programs, is
//! tested in tests/update.rs.

use super::*;
use std::cell::Cell;

#[test]
fn the_release_list_gives_the_hydo_releases_oldest_first() {
    let l = br##"[{"tag_name":"v0.4.0","body":"# hydo 0.4.0\n\n## Before you upgrade\n\n- new fingerprints\n\n## New\n\n- more"},
        {"tag_name":"skill-v2.3.3"},{"tag_name":"v0.10.0","draft":true},
        {"tag_name":"v0.9.0","prerelease":true},{"tag_name":"v0.3.10"},{"tag_name":"v0.3.9"},
        {"tag_name":"v1.0"},{"tag_name":"v0.5.0-rc.1"},{"tag_name":"action-v1.0.0"}]"##;
    let r = hydo_releases(l).unwrap();
    let v: Vec<&str> = r.iter().map(|r| r.version.as_str()).collect();
    assert_eq!(v, ["0.3.9", "0.3.10", "0.4.0"]);
    assert_eq!(
        before_you_upgrade(&r[2].notes).as_deref(),
        Some("- new fingerprints")
    );
    assert_eq!(before_you_upgrade("# x\n\n## New\n- a\n"), None);
    assert!(hydo_releases(b"{}").is_err());
}

#[test]
fn an_install_is_recognised_by_its_path() {
    let d = std::env::temp_dir().join(format!("hydo-update-kind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let mk = |rel: &str, files: &[&str]| -> PathBuf {
        let root = d.join(rel);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        for f in files {
            std::fs::write(root.join(f), "\"hydo 0.4.0 (registry+...)\"").unwrap();
        }
        root.join("bin/hydo")
    };
    let home = d.join("home/.hydo");
    let brew = mk("brew/Cellar/hydo/0.4.0/libexec", &["SHA256SUMS"]);
    assert_eq!(
        kind_of(&brew, None, &home, "0.4.0"),
        Kind::Brew {
            prefix: d.join("brew")
        }
    );
    let cargo = mk("cargo", &[".crates2.json"]);
    assert_eq!(
        kind_of(&cargo, None, &home, "0.4.0"),
        Kind::Cargo {
            root: d.join("cargo")
        }
    );
    let archive = mk("home/.hydo", &["SHA256SUMS", "VERSION"]);
    assert_eq!(
        kind_of(&archive, None, &home, "0.4.0"),
        Kind::Archive { root: home.clone() }
    );
    let cache = mk("home/.hydo/versions/0.4.0", &["SHA256SUMS"]);
    assert_eq!(
        kind_of(&cache, None, &home, "0.4.0"),
        Kind::Cache { home: home.clone() }
    );
    // another version's folder of the cache is a release tree like any other
    let other_version = mk("home/.hydo/versions/0.3.4", &["SHA256SUMS"]);
    assert!(matches!(
        kind_of(&other_version, None, &home, "0.4.0"),
        Kind::Archive { .. }
    ));
    assert!(matches!(
        kind_of(&cargo, Some(&d), &home, "0.4.0"),
        Kind::Other { .. }
    ));
    let bare = mk("bare", &[]);
    assert!(matches!(
        kind_of(&bare, None, &home, "0.4.0"),
        Kind::Other { .. }
    ));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_doctor_looks_once_a_day_and_keeps_a_failed_look_too() {
    let d = std::env::temp_dir().join(format!("hydo-update-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let f = d.join(CHECK_FILE);
    let calls = Cell::new(0);
    let list = br#"[{"tag_name":"v0.4.0"},{"tag_name":"v0.9.0"}]"#.to_vec();
    let ok = |_: &str| {
        calls.set(calls.get() + 1);
        Ok(list.clone())
    };
    let down = |_: &str| -> Result<Vec<u8>, String> {
        calls.set(calls.get() + 1);
        Err("offline".into())
    };
    assert_eq!(
        newest_known(&f, "http://a", 1000, &ok).as_deref(),
        Some("0.9.0")
    );
    assert_eq!(
        newest_known(&f, "http://a", 2000, &ok).as_deref(),
        Some("0.9.0")
    );
    assert_eq!(
        calls.get(),
        1,
        "the second look within a day is the kept answer"
    );
    // another list, or a day later: a new look; a failed look is kept as no answer
    assert_eq!(newest_known(&f, "http://b", 2000, &down), None);
    assert_eq!(newest_known(&f, "http://b", 3000, &down), None);
    assert_eq!(
        calls.get(),
        2,
        "a failed look is not repeated within the day"
    );
    assert_eq!(
        newest_known(&f, "http://b", 2000 + DAY, &ok).as_deref(),
        Some("0.9.0")
    );
    // a file that cannot be written is no error
    assert_eq!(
        newest_known(Path::new("/nonexistent/x/y.json"), "http://a", 1, &ok).as_deref(),
        Some("0.9.0")
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_options() {
    let a = |v: &[&str]| parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(a(&["--check"]).unwrap().check);
    assert!(a(&["-y"]).unwrap().yes);
    assert_eq!(
        a(&["--version", "v0.5.0"]).unwrap().version.as_deref(),
        Some("0.5.0")
    );
    assert!(a(&["--version", "0.5"]).is_err());
    assert!(a(&["--check", "--yes"]).is_err());
    assert!(a(&["--bogus"]).is_err());
}
