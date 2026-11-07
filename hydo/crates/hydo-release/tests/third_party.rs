//! The component lists are complete (Milestone 158): every pinned asset and guest program, every
//! literal destination of build-release.sh and every Rust crate compiled into a shipped program
//! has a license, its full license texts and a source. A failure says what to add and where.

use hydo_release::check;
use hydo_release::model::{List, Platform};
use hydo_release::rust::Crate;
use std::collections::{BTreeMap, BTreeSet};

fn repo() -> std::path::PathBuf {
    hydo_release::model::repo_root()
}

fn complete(p: Platform) {
    let errs = check::all(&repo(), p).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        errs.is_empty(),
        "{} is incomplete ({} problems):\n{}",
        p.list_path(),
        errs.len(),
        errs.join("\n")
    );
}

#[test]
fn linux_list_is_complete() {
    complete(Platform::Linux);
}

#[test]
fn mac_list_is_complete() {
    complete(Platform::Mac);
}

#[test]
fn a_pinned_file_without_a_component_is_named() {
    let r = repo();
    let mut l = List::load(&r, Platform::Linux).unwrap();
    l.components.retain(|c| c.id != "linux");
    let e = check::pins(&r, Platform::Linux, &l);
    assert!(
        e.iter()
            .any(|m| m.contains("guest/bzImage") && m.contains("add \"guest/bzImage\"")),
        "{e:?}"
    );
}

#[test]
fn a_copyleft_component_needs_its_source_archive() {
    let r = repo();
    let mut l = List::load(&r, Platform::Linux).unwrap();
    let c = l
        .components
        .iter_mut()
        .find(|c| c.id == "busybox-1.37.0")
        .unwrap();
    c.sources.iter_mut().for_each(|s| {
        s.file = None;
        s.sha256 = None;
    });
    let e = check::components(&r, Platform::Linux, &l);
    assert!(
        e.iter()
            .any(|m| m.contains("is copyleft") && m.contains("busybox")),
        "{e:?}"
    );
    let c = l
        .components
        .iter_mut()
        .find(|c| c.id == "busybox-1.37.0")
        .unwrap();
    c.license.clear();
    c.texts = vec!["hydo/dist/licenses/no-such/COPYING".into()];
    let e = check::components(&r, Platform::Linux, &l);
    assert!(e.iter().any(|m| m.contains("\"license\"")), "{e:?}");
    assert!(
        e.iter()
            .any(|m| m.contains("no-such/COPYING does not exist")),
        "{e:?}"
    );
}

#[test]
fn a_new_destination_in_build_release_is_named() {
    let l = List::load(&repo(), Platform::Linux).unwrap();
    let ok = "cp \"$A/guest/x\" \"$REL/guest/bzImage\"\nmkdir -p \"$REL/bin\"\n";
    assert!(check::script_text(Platform::Linux, &l, ok).is_empty());
    let bad = "cp \"$TOP/x\" \"$REL/extras/newtool\"\ncp -R x \"$REL/newdir/\"\n";
    let e = check::script_text(Platform::Linux, &l, bad);
    assert_eq!(e.len(), 2, "{e:?}");
    assert!(e[0].contains("$REL/extras/newtool") || e[1].contains("$REL/extras/newtool"));
}

#[test]
fn a_crate_without_license_texts_is_named() {
    let r = repo();
    let l = List::load(&r, Platform::Linux).unwrap();
    let mut m = BTreeMap::new();
    m.insert(
        "nolicense 1.0.0".to_string(),
        Crate {
            name: "nolicense".into(),
            version: "1.0.0".into(),
            license: None,
            license_file: None,
            dir: r.join("no-such-dir"),
            source: "registry+https://github.com/rust-lang/crates.io-index".into(),
            checksum: Some("0".repeat(64)),
            used_by: BTreeSet::from(["bin/hydo".to_string()]),
            texts: vec![],
            texts_from_repo: false,
        },
    );
    let e = check::crates(&r, Platform::Linux, &l, &m);
    assert!(
        e.iter()
            .any(|x| x.contains("nolicense 1.0.0 has no license field")
                && x.contains("rust_overrides")),
        "{e:?}"
    );
    assert!(
        e.iter().any(|x| x.contains("ships no license text")
            && x.contains("hydo/dist/licenses/rust-crates/nolicense-1.0.0")),
        "{e:?}"
    );
}
