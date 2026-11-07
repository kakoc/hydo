//! Reactions (M139, the schedule token `A=`, `src/features/react.rs`) on the scripted agent and
//! engine. The fake writes the SDK's assertion records of each workload call on the console when
//! the schedule has `A=` (`Fake::sdk_records`), so a reaction has events to see.
//!
//! Checked for every schedule: two runs give the same transcript (a pure function of the
//! schedule and the console), the transcript typed again by the ReplayController gives the same
//! records (`hydo repro --transcript`), and over one link (`drive::run`, the recovery VM after a
//! reaction's power cut) policy and replay agree. Semantically: a reaction acts at the first stop
//! where its event's record is seen after it was armed (an `always` that holds is no event, one
//! that fails is), not at all when the event never comes, and a schedule whose reaction never
//! fires runs the same commands as the schedule without it (only the declaration is added).

use crate::support;

use hydo_agent_proto::msg::{Command, Sig};
use hydo_control::drive::{self, Policy, RunFrom};
use hydo_control::transcript::{Header, Record};
use hydo_control::{Ending, EngineAction, Link, LinkError, LinkNext, ScheduleController, Start};
use hydo_schedule::{GuestToken, ReactAction, Schedule};
use support::fake::{Delivered, Fake, Msg, VmState};
use support::{boot, cadence, converse, plan_of, run_schedule, Item};

/// The reaction's event record: (id, whether the record is the event).
fn record(line: &str) -> Option<(String, bool)> {
    let j: serde_json::Value = serde_json::from_str(line.strip_prefix("HYDO-ASSERT ")?).ok()?;
    let t = j["t"].as_str()?;
    let ok = j["ok"].as_bool()?;
    let ev = if t == "sometimes" || t == "reachable" {
        ok
    } else {
        !ok
    };
    Some((j["id"].as_str()?.to_string(), ev))
}

/// The stop of the workload call that starts at step `from`.
fn spawn_stop(items: &[Item], from: u64) -> Option<u64> {
    items.iter().find_map(|i| match i {
        Item::Send {
            n,
            cmd: Command::Spawn(s),
            ..
        } if s.h == "workload"
            && s.env
                .as_ref()
                .is_some_and(|e| e.contains(&format!("HYDO_FROM={from}"))) =>
        {
            Some(*n)
        }
        _ => None,
    })
}

/// The stop at which the reaction's event is first seen after its arming, if it is.
fn event_stop(items: &[Item], got: &[Delivered], event: &str, at: u64) -> Option<u64> {
    let armed = spawn_stop(items, at)?;
    got.iter().find_map(|d| match &d.msg {
        Msg::Kernel(l) if d.n >= armed && record(l).is_some_and(|(id, ev)| ev && id == event) => {
            Some(d.n)
        }
        _ => None,
    })
}

fn by<'a>(items: &'a [Item], who: &str) -> Vec<&'a Item> {
    items.iter().filter(|i| i.by() == Some(who)).collect()
}

const SCHEDULES: [&str; 12] = [
    "disk=none | run A=client.place_failed:kill-redis",
    "disk=none | run A=client.place_failed+200:stop-redis-500",
    "disk=none | run A=server.place_acked@4:stop-redis-300",
    "disk=none | run P=redis@3+27 A=client.outcome_unknown@3+100:cont-redis",
    "disk=cut | run A=client.place_failed:cut",
    "disk=cut | run A=server.store_error+50:cut R=everysec",
    "disk=cut | run C=20 A=client.outcome_unknown:cut",
    "disk=cut | run C=3 A=client.place_failed:kill-redis",
    "disk=none | run A=order.fine:kill-redis",
    "disk=none | run A=order.broken:kill-redis",
    "disk=none | run A=no.such.event:cut K=redis@5+2",
    "disk=none | run A=client.place_failed:kill-nosuch",
];

#[test]
fn a_reaction_acts_where_its_event_is_seen_and_replays() {
    let mut fired = 0;
    let mut cuts = 0;
    // the Redis example and the bug benchmark's manifest (four services; the bench's own
    // reactions act on its order service too)
    let bench: Vec<String> = SCHEDULES
        .iter()
        .map(|s| s.replace("redis", "front"))
        .chain([
            "disk=none | run O=kafka@5-10:resp:loss,100% A=client.outcome_unknown+20:kill-front"
                .to_string(),
            "disk=cut | run K=mysql@3+2 A=client.place_failed@3:cut".to_string(),
        ])
        .collect();
    let redis: Vec<String> = SCHEDULES.iter().map(|s| s.to_string()).collect();
    for (rel, list) in [
        ("hydo/onboard/examples/redis/hydo.yaml", &redis),
        ("hydo/bench/manifest/hydo.yaml", &bench),
    ] {
        let plan = plan_of(rel).unwrap();
        // the first three salts whose boot reaches S1 (the fake's world fails some boots)
        let boots: Vec<(String, support::Booted)> = (1..20)
            .map(|i| format!("react-{i}"))
            .filter_map(|salt| boot(&plan, &salt).ok().map(|b| (salt, b)))
            .take(3)
            .collect();
        assert_eq!(boots.len(), 3, "{rel}");
        for (salt, b) in &boots {
            for text in list {
                let text = text.as_str();
                let s: Schedule = text.parse().unwrap();
                let ran = run_schedule(b, &s).unwrap();
                // the transcript typed again gives the same records
                assert_eq!(
                    ran.replay.records, ran.run.fin.transcript.records,
                    "{salt} {text}: replay"
                );
                // a second run decides the same
                let again = run_schedule(b, &s).unwrap();
                assert_eq!(
                    again.run.fin.transcript, ran.run.fin.transcript,
                    "{salt} {text}: two runs"
                );
                let items = &ran.run.items;
                let GuestToken::React(r) = s.guest.iter().find(|t| t.letter() == 'A').unwrap()
                else {
                    unreachable!()
                };
                let mine = by(items, "react:a0");
                if r.action == ReactAction::Kill("nosuch".into()) {
                    assert!(mine.is_empty(), "{text}");
                    assert!(
                    items.iter().any(|i| matches!(i, Item::Note { line, .. } if line.contains("A: no service nosuch, ignored"))),
                    "{text}"
                );
                    continue;
                }
                let has_c = s.guest.iter().any(|t| t.letter() == 'C');
                let ev = event_stop(items, &ran.run.fake.delivered, &r.event, r.at);
                let first = mine.first().map(|i| i.stop());
                match ev {
                    None => assert!(mine.is_empty(), "{salt} {text}: acted without its event"),
                    // a C= cut can come first and cancel the reaction; else it acts at once
                    Some(n) if has_c => assert!(first.is_none_or(|f| f == n), "{salt} {text}"),
                    Some(n) => assert_eq!(first, Some(n), "{salt} {text}: the first action"),
                }
                fired += first.is_some() as usize;
                if let Some(n) = first {
                    match &mine[0] {
                        Item::Note { line, .. } => assert!(line.contains("REACT A="), "{line}"),
                        i => panic!("{text}: the reaction starts with {i:?}"),
                    }
                    let cmds: Vec<&Command> = mine
                        .iter()
                        .filter_map(|i| match i {
                            Item::Send { cmd, .. } => Some(cmd),
                            _ => None,
                        })
                        .collect();
                    let sleep = |c: &Command| matches!(c, Command::Run(r) if r.argv.get(1).is_some_and(|a| a == "sleep"));
                    let sig = |c: &Command, h: &str, sg: Sig| matches!(c, Command::Signal(x) if x.h == h && x.sig == sg);
                    let mut it = cmds.iter();
                    if r.delay_ms > 0 {
                        assert!(sleep(it.next().unwrap()), "{text}: the delay");
                    }
                    match &r.action {
                        ReactAction::Kill(v) => {
                            assert!(sig(it.next().unwrap(), v, Sig::Kill), "{text}")
                        }
                        ReactAction::Stop(v, _) => {
                            assert!(sig(it.next().unwrap(), v, Sig::Stop), "{text}");
                            assert!(sleep(it.next().unwrap()), "{text}");
                            assert!(sig(it.next().unwrap(), v, Sig::Cont), "{text}");
                        }
                        ReactAction::Cont(v) => {
                            assert!(sig(it.next().unwrap(), v, Sig::Cont), "{text}")
                        }
                        ReactAction::Cut => {
                            // the main sequence freezes the workload and every service at once
                            let ending = ran.run.fin.ending.as_ref().unwrap();
                            if matches!(ending, Ending::PowerCut { .. }) {
                                let main: Vec<&Item> = items
                                    .iter()
                                    .filter(|i| i.by() == Some("main") && i.stop() >= n)
                                    .collect();
                                let froze = main.iter().position(|i| matches!(i, Item::Send { cmd: Command::Signal(x), .. } if x.h == "workload" && x.sig == Sig::Stop));
                                let read = main.iter().position(|i| {
                                    matches!(
                                        i,
                                        Item::Send {
                                            cmd: Command::Read(_),
                                            ..
                                        }
                                    )
                                });
                                if items.iter().any(|i| matches!(i, Item::Note { line, .. } if line.contains("POWER CUT at the reaction"))) {
                                assert!(froze.unwrap() < read.unwrap(), "{text}: frozen before the reads");
                                cuts += 1;
                                assert!(ran.recovery.is_some(), "{text}: the recovery boot");
                            }
                            }
                        }
                    }
                    assert!(
                    mine.iter().any(|i| matches!(i, Item::Report { line, .. } if line.contains("\"fault.reacted\""))) || r.action == ReactAction::Cut,
                    "{text}: the record"
                );
                    let _ = n;
                }
            }
        }
    }
    eprintln!("{fired} reactions acted, {cuts} of them power cuts");
    assert!(fired >= 30, "{fired} reactions acted");
    assert!(cuts >= 6, "{cuts} reaction power cuts");
}

/// A reaction whose event never comes changes nothing but its declaration: the same commands at
/// the same stops as the schedule without it (the fake's world from the same salt).
#[test]
fn a_reaction_that_never_fires_is_inert() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let b = boot(&plan, "inert").unwrap();
    for (with, without) in [
        (
            "disk=none | run K=redis@5+2 A=no.such.event:cut",
            "disk=none | run K=redis@5+2",
        ),
        (
            "disk=cut | run P=redis@3+4 C=12 A=order.fine:kill-redis",
            "disk=cut | run P=redis@3+4 C=12",
        ),
        (
            "disk=none | run A=no.such.event+300:stop-redis-1000",
            "disk=none | run",
        ),
    ] {
        let (sw, so): (Schedule, Schedule) = (with.parse().unwrap(), without.parse().unwrap());
        let salt = with.to_string();
        let conv = |s: &Schedule| {
            let mut ctl = ScheduleController::run(plan.clone(), s, &b.s1.2).unwrap();
            converse(
                Fake::resume(&b.s1.0, &salt, cadence(Some(s))),
                &mut ctl,
                Start::Resume(b.s1.1.clone()),
                Header {
                    controller: String::new(),
                    conversation: "run".into(),
                    schedule: Some(s.to_string()),
                    cadence: hydo_control::runner::cadence_of(Some(s)),
                    start: "resume S1".into(),
                },
            )
        };
        let (a, o) = (conv(&sw), conv(&so));
        let keep = |r: &&Record| {
            !matches!(r, Record::Report { line, .. } if line.contains("\"fault.reacted\""))
                && !matches!(r, Record::Note { line, .. } if line.starts_with("hydo-ob: command:"))
        };
        let ra: Vec<&Record> = a.fin.transcript.records.iter().filter(keep).collect();
        let ro: Vec<&Record> = o.fin.transcript.records.iter().filter(keep).collect();
        assert_eq!(ra, ro, "{with}");
        assert_eq!(a.fin.ending, o.fin.ending, "{with}");
        assert_eq!(
            a.fin.transcript.records.len(),
            o.fin.transcript.records.len() + 1,
            "{with}: only the declaration is added"
        );
    }
}

/// One engine run over one link with the recovery VM of a power cut (as tests/drive.rs).
struct Machine {
    cur: Fake,
    s0: VmState,
    salt: String,
    cad: u64,
}

impl Link for Machine {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        self.cur.next_stop()
    }
    fn send(&mut self, at: u64, bytes: &[u8]) -> Result<u64, LinkError> {
        self.cur.send(at, bytes)
    }
    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        self.cur.ack(send)
    }
    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        self.cur.snap(name)
    }
    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.cur.engine(a)
    }
    fn cont(&mut self) -> Result<(), LinkError> {
        self.cur.cont()
    }
    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.cur.end(e)
    }
    fn cut(&mut self, _e: &Ending) -> Result<String, LinkError> {
        self.cur = Fake::resume(&self.s0, &format!("{} recovery", self.salt), self.cad);
        Ok(String::new())
    }
}

/// `drive::run` (what `hydo run`, `hydo repro` and the explorer's workers use): a reaction's
/// power cut takes the run into its recovery boot on the same link, and the recorded transcripts
/// typed again (`hydo repro --transcript`) give the same records and the same `ctl=`.
#[test]
fn over_one_link_a_reactions_cut_reaches_the_recovery_and_replays() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let sb = boot(&plan, "react-drive").unwrap();
    let db = drive::Booted {
        plan: plan.clone(),
        s0: (sb.s0.1.clone(), sb.s0.2.clone()),
        s1: (sb.s1.1.clone(), sb.s1.2.clone()),
        boot: sb.boot.fin.clone(),
    };
    let mut recoveries = 0;
    for text in SCHEDULES {
        let s: Schedule = text.parse().unwrap();
        let cad = cadence(Some(&s));
        let machine = || Machine {
            cur: Fake::resume(&sb.s1.0, text, cad),
            s0: sb.s0.0.clone(),
            salt: text.into(),
            cad,
        };
        let got = drive::run(
            &mut machine(),
            &db,
            &s,
            RunFrom::Restored,
            &Policy::Schedule,
        )
        .unwrap();
        let want = run_schedule(&sb, &s).unwrap();
        assert_eq!(got.run.transcript, want.run.fin.transcript, "{text}");
        assert_eq!(
            got.recovery.as_ref().map(|r| &r.transcript),
            want.recovery.as_ref().map(|r| &r.fin.transcript),
            "{text}: recovery"
        );
        recoveries += got.recovery.is_some() as usize;
        let policy = Policy::Replay {
            run: Box::new(got.run.transcript.clone()),
            recovery: got
                .recovery
                .as_ref()
                .map(|r| Box::new(r.transcript.clone())),
        };
        let rep = drive::run(&mut machine(), &db, &s, RunFrom::Restored, &policy).unwrap();
        assert_eq!(
            rep.run.transcript.records, got.run.transcript.records,
            "{text}: replay"
        );
        assert_eq!(
            rep.recovery.as_ref().map(|r| &r.transcript.records),
            got.recovery.as_ref().map(|r| &r.transcript.records),
            "{text}: replayed recovery"
        );
        assert_eq!(rep.result_line(), got.result_line(), "{text}");
    }
    assert!(recoveries >= 3, "{recoveries}");
}
