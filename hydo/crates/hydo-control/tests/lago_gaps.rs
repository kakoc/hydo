//! The controller's part of the Lago onboarding's gaps (open items 87 to 89, Milestone 225), on
//! the scripted agent and engine, with the composed Redis example's plan:
//!
//! - `after: commands`: the `eventually` commands run before the eventually checks anchored at
//!   `commands`, that anchor is stamped after them, and final_verdict does not run them again;
//!   without such a check the order is the old one (the checks first).
//! - `test.first_records`: the run does not empty `$HYDO_ACKED` and `$HYDO_OUTCOMES`, so the
//!   lines the `first` commands wrote in provisioning (in the snapshot S1) stay.
//! - a failing check's output beyond its last line: one bounded `HYDO-CHECK-OUT` note.

use crate::support;

use hydo_agent_proto::msg::{Command, Write};
use hydo_control::schedule::{
    check_output_note, CHECK_OUT_LINE, CHECK_OUT_LINES, CHECK_OUT_PREFIX,
};
use hydo_control::Plan;
use hydo_schedule::Schedule;
use serde_json::{json, Value};
use support::{boot, data_of, run_schedule, Item};

const REL: &str = "hydo/onboard/examples/redis-composed/hydo/hydo.yaml";

fn plan_with(f: &dyn Fn(&mut Value)) -> Plan {
    let mut d = data_of(REL).unwrap();
    f(&mut d);
    Plan::from_manifest(&hydo_manifest::Manifest::from_validated(d)).unwrap()
}

/// A boot of the scripted guest that reached S1 (the scripted agent fails some boots, by salt).
fn boot_ok(p: &Plan, salt: &str) -> support::Booted {
    (0..20)
        .find_map(|i| boot(p, &format!("{salt}#{i}")).ok())
        .expect("a boot reached S1")
}

fn after_commands(d: &mut Value) {
    for v in d["invariants"].as_array_mut().unwrap() {
        if v.get("type").and_then(Value::as_str) == Some("eventually") {
            v["after"] = json!("commands");
        }
    }
}

/// The run's items as text lines: `R <report>` and `N <note>`, in order.
fn lines(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Report { line, .. } => Some(format!("R {line}")),
            Item::Note { line, .. } => Some(format!("N {line}")),
            _ => None,
        })
        .collect()
}

fn pos(ls: &[String], pat: &str) -> Vec<usize> {
    ls.iter()
        .enumerate()
        .filter(|(_, l)| l.contains(pat))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn eventually_checks_after_the_eventually_commands() {
    let s = Schedule::parse("disk=none | run").unwrap();
    // the old order: the eventually check is resolved before the eventually command runs
    let old = plan_with(&|_| {});
    let b = boot_ok(&old, "lago-gaps-old");
    let ls = lines(&run_schedule(&b, &s).unwrap().run.items);
    let cmd = pos(&ls, "kind=eventually cmd=eventually_serves");
    let rec = pos(&ls, "HYDO-ASSERT {\"t\":\"eventually\",\"id\":\"answers\"");
    assert_eq!(cmd.len(), 1, "{ls:#?}");
    assert_eq!(rec.len(), 1, "{ls:#?}");
    assert!(rec[0] < cmd[0], "the old order changed: {ls:#?}");
    assert!(pos(&ls, "liveness anchor 'commands'").is_empty());
    // after: commands
    let new = plan_with(&after_commands);
    let b = boot_ok(&new, "lago-gaps-new");
    let ls = lines(&run_schedule(&b, &s).unwrap().run.items);
    let cmd = pos(&ls, "kind=eventually cmd=eventually_serves");
    let end = pos(&ls, "HYDO-CMD end i=e");
    let anchor = pos(&ls, "liveness anchor 'commands'");
    let rec = pos(&ls, "HYDO-ASSERT {\"t\":\"eventually\",\"id\":\"answers\"");
    let fin = pos(&ls, "kind=finally");
    assert_eq!(cmd.len(), 1, "the eventually command ran again: {ls:#?}");
    assert_eq!((anchor.len(), rec.len()), (1, 1), "{ls:#?}");
    assert!(
        cmd[0] < end[0] && end[0] < anchor[0] && anchor[0] < rec[0],
        "{ls:#?}"
    );
    assert!(ls[rec[0]].contains("\"after\":\"commands\""), "{ls:#?}");
    // the finally command stays at the end, after the checks
    if let Some(f) = fin.first() {
        assert!(*f > rec[0], "{ls:#?}");
    }
    // the eventually command's result is asserted once, from its one run
    let held = pos(&ls, "\"id\":\"eventually_serves\",\"ok\":true");
    assert_eq!(held.len(), 1, "{ls:#?}");
    assert!(held[0] > end[0], "{ls:#?}");
    assert!(ls.iter().any(|l| l.starts_with("R HYDO-ORACLE:")));
}

/// The writes that empty the run's record files at its start.
fn truncations(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd:
                    Command::Write(Write {
                        path, data, append, ..
                    }),
                ..
            } if data.is_empty() && append != &Some(true) => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn first_records_stay_in_the_run() {
    let s = Schedule::parse("disk=none | run").unwrap();
    let old = plan_with(&|_| {});
    let b = boot_ok(&old, "lago-gaps-fr-old");
    let t = truncations(&run_schedule(&b, &s).unwrap().run.items);
    assert!(t.iter().any(|p| p == "/tmp/hydo/acked"), "{t:?}");
    assert!(t.iter().any(|p| p == "/tmp/hydo/outcomes"), "{t:?}");
    let new = plan_with(&|d| d["_compose"]["first_records"] = json!(true));
    assert!(new.compose.as_ref().unwrap().first_records);
    let b = boot_ok(&new, "lago-gaps-fr-new");
    let t = truncations(&run_schedule(&b, &s).unwrap().run.items);
    assert!(!t.iter().any(|p| p == "/tmp/hydo/acked"), "{t:?}");
    assert!(!t.iter().any(|p| p == "/tmp/hydo/outcomes"), "{t:?}");
    assert!(t.iter().any(|p| p == "/tmp/hydo/workload.log"), "{t:?}");
}

#[test]
fn a_failing_checks_output_is_kept_bounded() {
    // one line: the verdict has it all
    assert_eq!(check_output_note("c", "lost 3\n"), None);
    assert_eq!(check_output_note("c", "\n  \nlost 3\n"), None);
    let n = check_output_note(
        "c",
        "k1: short 2\nHYDO-ASSERT {}\nk2: short 1\n2 totals short\n",
    )
    .unwrap();
    let (p, j) = n.split_once(' ').unwrap();
    assert_eq!(p, CHECK_OUT_PREFIX);
    let v: Value = serde_json::from_str(j).unwrap();
    assert_eq!(
        v,
        json!({"id": "c", "lines": ["k1: short 2", "k2: short 1", "2 totals short"], "omitted": 0})
    );
    // many long lines: the last ones, each cut, at most 2,000 bytes
    let out: String = (0..100)
        .map(|i| format!("line {i} {}\n", "x".repeat(1000)))
        .collect();
    let v: Value = serde_json::from_str(
        check_output_note("c", &out)
            .unwrap()
            .split_once(' ')
            .unwrap()
            .1,
    )
    .unwrap();
    let ls = v["lines"].as_array().unwrap();
    assert!(ls.len() <= CHECK_OUT_LINES && !ls.is_empty());
    assert!(ls
        .iter()
        .all(|l| l.as_str().unwrap().len() <= CHECK_OUT_LINE));
    assert!(ls.last().unwrap().as_str().unwrap().starts_with("line 99 "));
    assert_eq!(v["omitted"], json!(100 - ls.len()));
    // quotes, tabs and other characters survive
    let v: Value = serde_json::from_str(
        check_output_note("c\"d", "a\t\"b\"\\\nlast\n")
            .unwrap()
            .split_once(' ')
            .unwrap()
            .1,
    )
    .unwrap();
    assert_eq!(v["id"], json!("c\"d"));
    assert_eq!(v["lines"][0], json!("a\t\"b\"\\"));
}
