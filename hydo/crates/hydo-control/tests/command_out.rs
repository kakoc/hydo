//! A failing workload command's output (Milestone 348): a composed command that ends with an
//! exit status other than 0 and 100 gives one bounded `HYDO-CMD-OUT` note with the last lines of
//! its output, which otherwise stays in the guest (open items 157 and 170). A note is for people:
//! no control transcript's hash and no fingerprint covers it.

use hydo_control::schedule::{cmd_output_note, CHECK_OUT_LINE, CHECK_OUT_LINES, CMD_OUT_PREFIX};
use serde_json::{json, Value};

fn note(id: &str, entry: i64, rc: u32, out: &str) -> Value {
    let n = cmd_output_note(id, entry, rc, out).expect("a note");
    let (p, j) = n.split_once(' ').unwrap();
    assert_eq!(p, CMD_OUT_PREFIX);
    serde_json::from_str(j).unwrap()
}

#[test]
fn one_line_is_kept_and_no_line_gives_no_note() {
    assert_eq!(cmd_output_note("c", 3, 1, ""), None);
    assert_eq!(cmd_output_note("c", 3, 1, "\n  \nHYDO-ASSERT {}\n"), None);
    // a command's one line is in no other record, so it is kept
    assert_eq!(
        note(
            "parallel_driver_write",
            7,
            1,
            "exec: \"hydo-oci\": executable file not found in $PATH\n"
        ),
        json!({
            "id": "parallel_driver_write",
            "entry": 7,
            "rc": 1,
            "lines": ["exec: \"hydo-oci\": executable file not found in $PATH"],
            "omitted": 0
        })
    );
}

#[test]
fn a_long_output_keeps_its_last_lines_bounded() {
    let out: String = (0..100)
        .map(|i| format!("line {i} {}\n", "x".repeat(1000)))
        .collect();
    let v = note("c", 2, 143, &out);
    assert_eq!(v["rc"], json!(143));
    let ls = v["lines"].as_array().unwrap();
    assert!(ls.len() <= CHECK_OUT_LINES && !ls.is_empty());
    assert!(ls
        .iter()
        .all(|l| l.as_str().unwrap().len() <= CHECK_OUT_LINE));
    assert!(ls.last().unwrap().as_str().unwrap().starts_with("line 99 "));
    assert_eq!(v["omitted"], json!(100 - ls.len()));
}
