//! hydo-build's refusals of `errors:` (MANIFEST.md "Error logs as findings"). The compiled
//! HYDO_ERR_* lines are covered by the build goldens (`--print-shell`, tests/equivalence.rs).
//!
//! Until M136 this file also ran the guest side of the shell-init images (hydo-init-errlog.sh's
//! el_mark / el_report under the real busybox) and checked that hydo-build spliced it; both went
//! with the shell init. The host controller computes the same scan on agent images
//! (hydo-control `schedule/errlog.rs`, checked by its tests); `git show
//! d9202e7:hydo/crates/hydo-build/tests/errlog.rs` restores the old test.

use serde_json::{json, Value};

#[test]
fn errors_refusals() {
    for (errors, svc_errors, why) in [
        (json!({"bogus": 1}), None, "unknown field"),
        (json!({"max": 0}), None, "max"),
        (
            Value::Null,
            Some(json!({"format": "redis"})),
            "services[0].errors needs errors:",
        ),
        (json!(true), Some(json!({"match": "("})), "bad regex"),
        (
            json!(true),
            Some(json!({"files": ["relative.log"]})),
            "relative file",
        ),
    ] {
        let mut m = json!({"name": "t", "services": [{"name": "a", "cmd": "x", "ready": "x"}],
                           "workload": {"steps": 2, "run": "x"},
                           "invariants": [{"id": "i", "cmd": "true"}]});
        if !errors.is_null() {
            m["errors"] = errors;
        }
        if let Some(se) = svc_errors {
            m["services"][0]["errors"] = se;
        }
        assert!(
            hydo_manifest::validate_data(&mut m).is_err(),
            "refused: {why}"
        );
    }
}
