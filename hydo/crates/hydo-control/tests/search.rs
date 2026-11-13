//! The state search's links on the scripted agent and engine (docs/design/state-search.md): a run
//! that takes snapshots has the transcript of the same run without them; a branch from a
//! snapshot with new faults after the snapshot's step range has the transcript of the branch's
//! schedule run from S1; a branch that changes the prefix is refused by the offline check.

use crate::support;

use hydo_control::drive::{self, Policy, RunFrom};
use hydo_control::search::{BranchLink, RecLink, SnapPolicy};
use hydo_control::{Ending, EngineAction, Link, LinkError, LinkNext, Seen, StopInfo};
use hydo_schedule::Schedule;
use support::fake::{Fake, VmState};
use support::plan_of;

/// One scripted VM, its stops numbered from `n0` on (a real engine's snapshot keeps the stop
/// count; the scripted engine starts each conversation at 1).
struct Vm {
    f: Fake,
    n0: u64,
}

impl Link for Vm {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        Ok(match self.f.next_stop()? {
            LinkNext::Stop { stop, lines } => LinkNext::Stop {
                stop: StopInfo {
                    n: stop.n + self.n0,
                    ..stop
                },
                lines,
            },
            d => d,
        })
    }
    fn send(&mut self, at: u64, bytes: &[u8]) -> Result<u64, LinkError> {
        self.f.send(at, bytes)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.f.ack(send)
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        self.f.snap(name)
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.f.engine(a)
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.f.cont()
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.f.end(e)
    }
    fn before_step(&mut self, _stop: &StopInfo, _seen: &[Seen]) -> Result<(), LinkError> {
        Ok(())
    }
}

const SALT: &str = "state-search";

fn vm(st: &VmState, cad: u64, n0: u64) -> Vm {
    Vm {
        f: Fake::resume(st, SALT, cad),
        n0,
    }
}

#[test]
fn a_branch_equals_its_schedule_run_from_s1() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let sb = support::boot(&plan, "search").unwrap();
    let db = drive::Booted {
        plan: plan.clone(),
        s0: (sb.s0.1.clone(), sb.s0.2.clone()),
        s1: (sb.s1.1.clone(), sb.s1.2.clone()),
        boot: sb.boot.fin.clone(),
    };
    let a: Schedule = "disk=none | run K=redis@5+2".parse().unwrap();
    let cad = support::cadence(Some(&a));
    // the parent, with snapshots wherever a stop shows a new console line
    let mut m = vm(&sb.s1.0, cad, 0);
    let policy = SnapPolicy {
        names: (0..6).map(|i| format!("x{i}")).collect(),
        any_new: true,
        // the scripted guest writes no console lines in the workload: snapshots by clock
        spread: (1..=6).map(|i| i * 40).collect(),
        ..SnapPolicy::default()
    };
    let (ran_a, stops, taken) = {
        let mut rec = RecLink::new(&mut m, policy);
        let r = drive::run(&mut rec, &db, &a, RunFrom::Restored, &Policy::Schedule).unwrap();
        (r, rec.stops.clone(), rec.taken.clone())
    };
    let snaps = m.f.snaps.clone();
    // the snapshots changed nothing: the same run without them
    let mut plain = vm(&sb.s1.0, cad, 0);
    let ran_p = drive::run(&mut plain, &db, &a, RunFrom::Restored, &Policy::Schedule).unwrap();
    assert_eq!(ran_a.run.transcript, ran_p.run.transcript);
    assert!(!taken.is_empty(), "no snapshot taken");
    let ranges = hydo_control::search::ranges(&ran_a.run.transcript);
    assert!(!ranges.is_empty(), "no step range in the transcript");
    let mut checked = 0;
    for t in &taken {
        let b = ranges
            .iter()
            .find(|r| r.2 >= t.stop.n)
            .map(|r| r.1)
            .unwrap_or(plan.steps as u64);
        if b + 2 >= plan.steps as u64 {
            continue;
        }
        // a fault after the step range in progress: a valid branch
        let text = format!("disk=none | run K=redis@5+2 P=redis@{}+3", b + 1);
        let s: Schedule = text.parse().unwrap();
        let prefix = &stops[..=t.index];
        hydo_control::agent::check_branch(&db, &s, prefix)
            .unwrap_or_else(|e| panic!("{text} from {}: {e}", t.name));
        let mut live = vm(&snaps[&t.name], cad, t.stop.n - 1);
        let got = {
            let mut br = BranchLink::new(prefix, Some(&mut live));
            drive::run(&mut br, &db, &s, RunFrom::Restored, &Policy::Schedule).unwrap()
        };
        let mut fresh = vm(&sb.s1.0, cad, 0);
        let want = drive::run(&mut fresh, &db, &s, RunFrom::Restored, &Policy::Schedule).unwrap();
        // (the scripted engine numbers its sends from 1 in each conversation; a real engine's
        // snapshot keeps the count, so only the scripted transcripts differ in `send`)
        let norm = |t: &hydo_control::Transcript| -> Vec<hydo_control::Record> {
            t.records
                .iter()
                .map(|r| match r {
                    hydo_control::Record::Ack { n, id, .. } => hydo_control::Record::Ack {
                        n: *n,
                        id: *id,
                        send: 0,
                    },
                    r => r.clone(),
                })
                .collect()
        };
        assert_eq!(
            norm(&got.run.transcript),
            norm(&want.run.transcript),
            "{text} from {} (stop {})",
            t.name,
            t.stop.n
        );
        // a change before the snapshot's step range ends is refused
        if b > 1 {
            let bad: Schedule = format!("disk=none | run K=redis@{}+2", b.min(5) - 1)
                .parse()
                .unwrap();
            if bad != a {
                assert!(hydo_control::agent::check_branch(&db, &bad, prefix).is_err());
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "no snapshot before the last steps");
}
