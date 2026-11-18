//! YAML anchors, aliases and `<<` merge keys in `hydo.yaml`, with Compose's top-level `x-`
//! extension fields that hold them (Milestone 316). A user's manifest with `x-common: &common`
//! and `<<: *common` in its services stopped with `hydo.yaml:<n>: unexpected indentation`: the
//! YAML subset had no anchors, and the validator knew no `x-` key.

use hydo_manifest::model::Project;
use hydo_manifest::{parse_text, validate_data};

const WITH_ANCHORS: &str = r#"format: 1
name: shop
x-common: &common
  image_ref: "app:1"
  cmd: /bin/app
  env:
    MODE: shared
x-ready: &ready "true"
services:
  - name: api
    <<: *common
    ready: *ready
  - name: worker
    <<: *common
    env:
      MODE: worker
    ready: *ready
workload:
  steps: 2
  run: "true"
invariants:
  - id: ok
    cmd: "true"
"#;

/// The same manifest written out by hand.
const EXPANDED: &str = r#"format: 1
name: shop
services:
  - image_ref: "app:1"
    cmd: /bin/app
    env:
      MODE: shared
    name: api
    ready: "true"
  - image_ref: "app:1"
    cmd: /bin/app
    env:
      MODE: worker
    name: worker
    ready: "true"
workload:
  steps: 2
  run: "true"
invariants:
  - id: ok
    cmd: "true"
"#;

#[test]
fn a_manifest_with_anchors_reads_as_the_one_written_out() {
    let a = parse_text(WITH_ANCHORS, "hydo.yaml", false).unwrap();
    let b = parse_text(EXPANDED, "hydo.yaml", false).unwrap();
    assert_eq!(a, b);
    assert!(
        a.get("x-common").is_none(),
        "the extension field is dropped"
    );
    let mut v = a.clone();
    validate_data(&mut v).unwrap();
    serde_json::from_value::<Project>(a).unwrap();
}

#[test]
fn an_alias_without_its_anchor_names_the_line() {
    let t = WITH_ANCHORS.replace("<<: *common\n    ready", "<<: *commn\n    ready");
    let e = parse_text(&t, "hydo.yaml", false).unwrap_err().to_string();
    assert_eq!(
        e,
        "hydo.yaml:11: alias *commn has no anchor &commn before it"
    );
}
