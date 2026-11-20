//! A run that the engine ends by itself (Milestone 328, open item 112). A user's run ended
//! `ERROR check: exit budget exhausted`: the engine's limit of VM exits for one phase ran out while
//! an `eventually` check started one client per stuck query at every try. The verdict named
//! neither the check nor the queries. The controller now names what was running, and a check
//! whose tries all failed until then gives its violation. The scripted guest of
//! `support::fake` stands in for the engine and the agent; its engine ends the run at a chosen
//! stop with the engine's words.

use crate::support;

use hydo_control::plan::{PlanInvariant, PlanLive};
use hydo_control::{Header, ScheduleController, Start};
use hydo_schedule::Schedule;
use support::fake::Fake;
use support::{boot, converse, plan_of};

fn header(s: &Schedule) -> Header {
    Header {
        controller: String::new(),
        conversation: "run".into(),
        schedule: Some(s.to_string()),
        cadence: "default".into(),
        start: "resume S1".into(),
    }
}

/// The verdict a run of `s` from `b` gives when the engine ends it at stop `end` (None: the
/// run's own end), and the stop the run ended at.
fn run_to(b: &support::Booted, s: &Schedule, end: Option<u64>) -> (Option<String>, u64) {
    let mut ctl = ScheduleController::run_unchecked(b.plan.clone(), s, &b.s1.2).unwrap();
    let mut fake = Fake::resume(&b.s1.0, &s.to_string(), support::cadence(Some(s)));
    fake.end_at = end;
    let c = converse(fake, &mut ctl, Start::Resume(b.s1.1.clone()), header(s));
    let n = c.fin.last_stop.map(|x| x.n).unwrap_or(0);
    (c.fin.engine_verdict.clone(), n)
}

#[test]
fn a_run_the_engine_ends_names_what_ran_or_the_failing_check() {
    let mut plan = plan_of("hydo/onboard/examples/redis/hydo-live.yaml").unwrap();
    let k = plan.invariants.len();
    plan.invariants.push(PlanInvariant {
        key: format!("i{k}"),
        id: "queries.settle".into(),
        kind: "eventually".into(),
        msg: "every accepted query ends".into(),
        cmd: "HYDO_FAKE_NEVER=1; echo 3 queries still running".into(),
        every: String::new(),
        final_: false,
        live: Some(PlanLive {
            within: 30,
            after: "heal".into(),
            poll: 1,
        }),
    });
    let b = (0..20)
        .find_map(|i| boot(&plan, &format!("engine end #{i}")).ok())
        .expect("a boot");
    let s: Schedule = "disk=none | run".parse().unwrap();
    // without an end by the engine nothing changes
    let (v, last) = run_to(&b, &s, None);
    assert_eq!(v, None);
    assert!(last > 40, "{last}");
    // the engine ends the run while the check keeps failing: its violation, with its output
    let (v, _) = run_to(&b, &s, Some(last - 3));
    let v = v.expect("a verdict in place of the engine's");
    assert!(
        v.starts_with("HYDO-ORACLE: VIOLATION [queries.settle] not satisfied "),
        "{v}"
    );
    assert!(
        v.contains("the engine's exit budget for the phase ran out after "),
        "{v}"
    );
    assert!(
        v.contains(" failed tries of the check (last check rc="),
        "{v}"
    );
    // earlier, the engine's words with what ran (the workload's steps at some stop)
    let mut named = Vec::new();
    for end in (2..last - 3).step_by(3) {
        if let (Some(v), _) = run_to(&b, &s, Some(end)) {
            if v.starts_with("ERROR check: exit budget exhausted while ") {
                named.push(v);
            } else {
                // the example's own eventually check fails a few tries after the heal too
                assert!(
                    v.starts_with("HYDO-ORACLE: VIOLATION [")
                        && v.contains("the engine's exit budget for the phase ran out after "),
                    "{v}"
                );
            }
        }
    }
    assert!(
        named
            .iter()
            .any(|v| v.contains(" while the workload's step")),
        "{named:?}"
    );
    assert!(
        named
            .iter()
            .all(|v| v.ends_with("(the engine's limit of VM exits for one phase of a run; hydo guide checks, \"The exit budget\")")),
        "{named:?}"
    );
}
