//! `guest.clock_after_cut` after a power cut at the k-th flush (`cf=`, Milestone 196) on the
//! scripted agent and engine.
//!
//! With `cf=` the engine turns the workload's end into a power cut, so the controller never takes
//! the power-cut path where it reads the guest's clock for a `C=` cut. Before Milestone 196 the
//! recovery boot then set its clock to `guest.clock_start` plus `clock_after_cut`, behind every
//! time the first boot had written (a false finding on YugabyteDB). Now, with `cf=` in the
//! schedule and `clock_after_cut` in the plan, the controller reads the clock at the workload's
//! end and carries it in the acknowledged line as a `C=` cut does; the recovery boot continues
//! from it. Without `cf=`, or without `clock_after_cut`, the run sends the same commands as
//! before.

use crate::support;

use hydo_agent_proto::msg::Command;
use hydo_control::{Ending, Plan};
use hydo_schedule::Schedule;
use support::Item;

const START: i64 = 1794096000;

fn plan(after: bool) -> Plan {
    let a = if after { "  clock_after_cut: 60\n" } else { "" };
    let text = format!(
        "name: clockcut
guest:
  clock_start: {START}
{a}services:
  - name: db
    cmd: db-server
    ready: db-ping
workload:
  steps: 4
  run: client
invariants:
  - id: all.done
    final: true
    cmd: final-check
"
    );
    let m = hydo_manifest::from_embedded(&text, None)
        .unwrap_or_else(|e| panic!("the manifest: {e}\n{text}"));
    Plan::from_manifest(&m).unwrap()
}

fn clock_sets(items: &[Item]) -> Vec<i64> {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::ClockSet(c),
                ..
            } => c.sec,
            _ => None,
        })
        .collect()
}

fn clock_gets(items: &[Item]) -> usize {
    items
        .iter()
        .filter(|i| {
            matches!(
                i,
                Item::Send {
                    cmd: Command::ClockGet(_),
                    ..
                }
            )
        })
        .count()
}

fn acked_line(items: &[Item]) -> String {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Report { line, .. } => line.strip_prefix("hydo-explore-acked:"),
            _ => None,
        })
        .next_back()
        .expect("an acknowledged line")
        .to_string()
}

const CF: &str = "disk=cut dseed=1 cf=3 | run";

#[test]
fn a_cut_at_a_flush_carries_the_first_boots_clock() {
    let p = plan(true);
    let b = support::boot(&p, "clockcut").unwrap();
    let (run, inputs, recovery) =
        support::run_cut_at_go(&b, &Schedule::parse(CF).unwrap()).unwrap();
    assert!(
        matches!(run.fin.ending, Some(Ending::PowerCut { .. })),
        "{:?}",
        run.fin.ending
    );
    let inputs = inputs.expect("the cut hands inputs over");
    let clock = inputs.clock.expect("the first boot's clock is carried");
    assert!(clock >= START, "{clock}");
    // the token is in the acknowledged line, not among the ids
    assert!(acked_line(&run.items).contains(&format!("clock={clock}")));
    assert!(!inputs.acked.iter().any(|a| a.starts_with("clock=")));
    if let Some(Ending::PowerCut { acked, .. }) = &run.fin.ending {
        assert!(!acked.iter().any(|a| a.starts_with("clock=")), "{acked:?}");
    }
    // the recovery boot continues from it: its clock is set to the first boot's plus 60 s
    let rec = recovery.expect("a recovery boot");
    let sets = clock_sets(&rec.items);
    assert!(sets.contains(&(clock + 60)), "{sets:?} (clock {clock})");
    assert!(!sets.contains(&(START + 60)), "{sets:?}");
}

#[test]
fn without_cf_or_without_the_key_nothing_changes() {
    // without the key: no clock read and no token, with cf= or without it
    let p = plan(false);
    let b = support::boot(&p, "clockcut").unwrap();
    for s in [CF, "disk=cut dseed=1 | run"] {
        let r = support::run_schedule(&b, &Schedule::parse(s).unwrap()).unwrap();
        assert_eq!(clock_gets(&r.run.items), 0, "{s}");
        assert!(!acked_line(&r.run.items).contains("clock="), "{s}");
    }
    // with the key but no cf= and no cut: no clock read and no token either
    let p = plan(true);
    let b = support::boot(&p, "clockcut").unwrap();
    let r = support::run_schedule(&b, &Schedule::parse("disk=cut dseed=1 | run").unwrap()).unwrap();
    assert_eq!(clock_gets(&r.run.items), 0);
    assert!(!acked_line(&r.run.items).contains("clock="));
    // a cf= run the engine does not cut (its k-th flush never came) ends as usual
    let r = support::run_schedule(&b, &Schedule::parse(CF).unwrap()).unwrap();
    assert_eq!(clock_gets(&r.run.items), 1);
    assert!(
        matches!(&r.run.fin.ending, Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE:")),
        "{:?}",
        r.run.fin.ending
    );
}
