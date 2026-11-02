//! The other conversations and formats: the shell's solo mode against the shell model, the
//! transcript file's round trip and its summary for the results document, determinism of two
//! runs, and the lock-step adapter over a scripted hydo_engine::lockstep stream.

mod support;

use hydo_agent_proto::frame::frame;
use hydo_agent_proto::line::AgentLine;
use hydo_agent_proto::msg::*;
use hydo_control::lockstep_link::LockstepLink;
use hydo_control::{
    run, Controller, Ending, Header, Out, ScheduleController, Seen, Start, StopInfo, Transcript,
};
use hydo_engine::lockstep::{escape, Lockstep, TextLink};
use support::fake::Fake;
use support::model::{check, Conv};
use support::{converse, manifests, plan_of};

fn header(conv: &str) -> Header {
    Header {
        controller: String::new(),
        conversation: conv.into(),
        schedule: None,
        cadence: "default".into(),
        start: "boot".into(),
    }
}

#[test]
fn solo_mode_does_what_the_shell_does() {
    let mut n = 0;
    for m in manifests() {
        let Ok(plan) = plan_of(&m) else { continue };
        for i in 0..4 {
            let salt = format!("{m} solo {i}");
            let mut ctl = ScheduleController::solo(plan.clone()).unwrap();
            let c = converse(Fake::boot(&salt, 16), &mut ctl, Start::Boot, header("solo"));
            check(&plan, &c.items, Conv::Solo, "", (&[], &[]), None)
                .unwrap_or_else(|e| panic!("{m} ({salt}): solo differs from the shell:\n{e}"));
            assert!(matches!(c.fin.ending, Some(Ending::Verdict(_))));
            n += 1;
        }
    }
    assert!(n > 50, "{n}");
}

#[test]
fn two_runs_give_the_same_transcript_and_the_file_reads_back() {
    let plan = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    let b = support::boot(&plan, "redis twice").unwrap();
    for text in [
        "disk=none | run K=redis@3+2 R=everysec",
        "disk=cut dseed=5 | run P=redis@2~100:400 C=12",
        "rb=1 | run X=redis@4~120",
    ] {
        let s: hydo_schedule::Schedule = text.parse().unwrap();
        let a = support::run_schedule(&b, &s).unwrap();
        let c = support::run_schedule(&b, &s).unwrap();
        assert_eq!(a.run.fin.transcript, c.run.fin.transcript, "{text}");
        let t = &a.run.fin.transcript;
        let back = Transcript::parse(&t.to_jsonl()).unwrap();
        assert_eq!(&back, t);
        assert_eq!(back.hash(), t.hash());
        assert_eq!(t.hash().len(), 16);
        // the notes are not in the hash
        let mut no_notes = t.clone();
        no_notes
            .records
            .retain(|r| !matches!(r, hydo_control::Record::Note { .. }));
        assert_eq!(no_notes.hash(), t.hash());
        let r = t.to_results(Some("control.jsonl"));
        assert_eq!(r.conversation, "run");
        assert_eq!(r.controller, "schedule");
        assert_eq!(r.hash, t.hash());
        assert_eq!(r.sends, t.sends().count() as u64);
        assert_eq!(
            r.cadence,
            if text.starts_with("rb=1") {
                "rb=1"
            } else {
                "default"
            }
        );
        let doc = serde_json::to_value(hydo_results::Run {
            control: vec![r],
            ..hydo_results::Run::new(text, hydo_results::Verdict::default(), "0")
        })
        .unwrap();
        assert_eq!(doc["control"][0]["transcript"], "control.jsonl");
        // a reader of the released results schema ignores the new field
        let run: hydo_results::Run = serde_json::from_value(doc).unwrap();
        assert_eq!(run.control.len(), 1);
    }
    assert!(Transcript::parse("{\"control\":2}\n")
        .unwrap_err()
        .contains("format 2"));
}

/// A controller for the adapter test: on the hello, sync the file systems; on the reply, end.
struct Tiny;

impl Controller for Tiny {
    fn name(&self) -> &str {
        "tiny"
    }
    fn step(&mut self, _stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        for s in seen {
            match s {
                Seen::Hello(_) => {
                    out.send("main", Command::Syncfs(Syncfs {}));
                }
                Seen::Reply { .. } => {
                    out.report("main", "HYDO-ORACLE: OK tiny");
                    out.end(Ending::Verdict("HYDO-ORACLE: OK tiny".into()));
                }
                _ => {}
            }
        }
    }
}

#[test]
fn the_lockstep_link_speaks_the_engine_protocol() {
    let hello = frame(
        &AgentLine::Event {
            v: 1,
            seq: 0,
            ev: Event::Hello(Hello {
                proto: 1,
                agent: "0.1.0".into(),
                ops: Command::NAMES.iter().map(|s| s.to_string()).collect(),
                features: vec![],
                pid1: true,
            }),
        }
        .encode(),
    );
    let reply = frame(
        &AgentLine::Reply {
            v: 1,
            seq: 1,
            re: 1,
            body: Reply::Syncfs(SyncfsOk {}),
        }
        .encode(),
    );
    let engine = format!(
        "EVENT a at=10 n=1 {}\n\
         EVENT a at=10 n=1 [    1.0] kernel text\n\
         STOP a at=10 n=1 phase=workload why=start\n\
         OK send id=1 sent=10\n\
         EVENT a at=20 n=2 {}\n\
         STOP a at=20 n=2 phase=workload why=cadence\n\
         OK ack 1\n\
         RESULT a fp=0000000000000001 wfp=0000000000000002 secs=1.0 stops=2 typed=1 inh=00000000000000aa verdict=HYDO-ORACLE: OK tiny\n",
        escape(hello.trim_end().as_bytes()),
        escape(reply.trim_end().as_bytes())
    );
    let mut tl = TextLink::new(std::io::Cursor::new(engine.into_bytes()), Vec::new());
    {
        let mut ls = Lockstep::new(&mut tl);
        let mut link = LockstepLink::new(&mut ls);
        let fin = run(&mut link, &mut Tiny, Start::Boot, header("boot")).unwrap();
        assert_eq!(
            fin.ending,
            Some(Ending::Verdict("HYDO-ORACLE: OK tiny".into()))
        );
        assert!(fin.result.contains("fp=0000000000000001"), "{}", fin.result);
        assert_eq!(fin.transcript.header.controller, "tiny");
    }
    assert_eq!(
        String::from_utf8(tl.writer().clone()).unwrap(),
        "SEND at=10 {\"id\":1,\"op\":\"syncfs\",\"v\":1}\\n\nCONT\nACK 1\nEND HYDO-ORACLE: OK tiny\n"
    );
}

#[test]
fn a_periodic_check_keeps_the_environment_of_its_start() {
    // miniflux: postgres, then front (the last service started, so HYDO_SVC=front when
    // start_periodic forks its checker). A K= restart of postgres exports HYDO_SVC=postgres in
    // the init; the shell's checker subshell keeps HYDO_SVC=front (HANDOFF.md, M122 decision 20).
    let m = "hydo/onboard/examples/miniflux/hydo.yaml";
    let plan = plan_of(m).unwrap();
    assert!(plan.invariants.iter().any(|i| !i.every.is_empty()));
    // the scripted guest fails some boots on purpose: the first salt that reaches S1
    let b = (0..20)
        .find_map(|i| support::boot(&plan, &format!("miniflux periodic env {i}")).ok())
        .expect("a boot that reaches S1");
    let has = |c: &Command, kv: &str| match c {
        Command::Run(r) => r.env.as_ref().is_some_and(|e| e.iter().any(|x| x == kv)),
        Command::Spawn(s) => s.env.as_ref().is_some_and(|e| e.iter().any(|x| x == kv)),
        _ => false,
    };
    let mut after_restart = 0;
    for text in [
        "disk=none | run K=postgres@2+3",
        "disk=none | run K=postgres@1+10",
        "disk=none | run K=postgres@5+20",
    ] {
        let s: hydo_schedule::Schedule = text.parse().unwrap();
        let r = support::run_schedule(&b, &s).unwrap();
        check(
            &plan,
            &r.run.items,
            Conv::Run,
            &s.guest_text(),
            (&[], &[]),
            Some(&b.s1.2),
        )
        .unwrap_or_else(|e| panic!("{text}: the run differs from the shell:\n{e}"));
        let mut restarted = false;
        for it in &r.run.items {
            let support::Item::Send { by, cmd, .. } = it else {
                continue;
            };
            if by == "main" && has(cmd, "HYDO_SVC=postgres") {
                restarted = true;
            }
            if by.starts_with("every:") {
                assert!(
                    has(cmd, "HYDO_SVC=front") && has(cmd, "HYDO_LOG=/tmp/hydo/front.log"),
                    "{text}: a periodic command without the environment of its start: {cmd:?}"
                );
                if restarted {
                    after_restart += 1;
                }
            }
        }
        assert!(restarted, "{text}: postgres was not restarted");
    }
    assert!(
        after_restart > 0,
        "no periodic command was sent after a restart of postgres"
    );
}
