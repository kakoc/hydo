//! No silent passes (2026-11-07): a schedule that names what its image does not have is refused
//! ([`hydo_control::names`]), every recorded schedule is still accepted, and with
//! `guest.outcomes_after_cut: true` the recovery boot after a power cut gets the first boot's
//! last outcomes in `$HYDO_OUTCOMES` (without it nothing changes: the recorded transcripts of
//! the other tests are the proof).

mod support;

use hydo_agent_proto::msg::Command;
use hydo_control::{Ending, Plan, ScheduleController};
use hydo_schedule::Schedule;
use support::Item;

fn plan(rel: &str) -> Plan {
    support::plan_of(rel).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn refused(p: &Plan, s: &str) -> String {
    let sc = Schedule::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
    match hydo_control::names::check(p, &sc) {
        Ok(()) => panic!("{s} was accepted"),
        Err(e) => {
            // the controller refuses it too, before any command
            let g = hydo_control::GuestState::default();
            let c = ScheduleController::run(p.clone(), &sc, &g);
            assert!(c.is_err(), "{s}: the controller drives it");
            e
        }
    }
}

fn accepted(p: &Plan, s: &str) {
    let sc = Schedule::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
    if let Err(e) = hydo_control::names::check(p, &sc) {
        panic!("{s} was refused: {e}");
    }
}

#[test]
fn a_token_for_what_the_image_lacks_is_refused_naming_what_it_has() {
    let p = plan("hydo/onboard/examples/redis/hydo.yaml");
    let e = refused(&p, "disk=none | run K=kafka@4+2");
    assert_eq!(
        e,
        "K=kafka@4+2 names the service kafka, which this image does not have (its services: redis)"
    );
    for s in [
        "disk=none | run P=front@8+5",
        "disk=none | run X=primry@5~200",
        "disk=none | run P=front@8~100:500",
        "disk=none | run A=redis.acked+10:kill-kafka",
    ] {
        assert!(
            refused(&p, s).contains("which this image does not have"),
            "{s}"
        );
    }
    // a network fault on an image whose faults: have no network
    let e = refused(&p, "disk=none | run N=redis@5-10:loss,60%");
    assert!(e.contains("neither network nor oneway"), "{e}");
    // a protocol fault without fakes:, a composition without a plan, BUGGIFY without
    // buggify: true, a freeze without freeze, the stack's Q=, a knob the manifest lacks
    assert!(refused(&p, "disk=none | run H=payments@0-1:drop").contains("no fakes:"));
    assert!(refused(&p, "disk=none | run W=0000002a,p4,q2,r100").contains("test composer"));
    assert!(refused(&p, "disk=none | run B=1234").contains("buggify: true"));
    assert!(refused(&p, "disk=none | run F=redis<workload@5#1:100").contains("freeze"));
    assert!(refused(&p, "disk=none | run Q=2d").contains("Q="));
    let e = refused(&p, "disk=none | run M=0");
    assert_eq!(
        e,
        "M=0 sets the knob M, which this image's manifest does not have (its knobs: R)"
    );
    // what the image has is accepted
    for s in [
        "disk=none | run",
        "disk=none | run K=redis@5+2",
        "disk=cut | run C=12 R=everysec",
        "disk=none | run T=10:3600 P=redis@5+2 X=redis@3~100",
        "disk=none | run A=redis.acked+10:kill-redis",
    ] {
        accepted(&p, s);
    }
}

#[test]
fn storage_faults_name_a_service_with_storage() {
    // G= needs storage:, L= a storage: with its own file system (M183); before this check the
    // controller logged "has no storage, ignored" and the run passed as the clean run
    let p = plan("hydo/onboard/examples/redis/hydo.yaml");
    let e = refused(&p, "disk=none | run G=redis@10:zero,len=40");
    assert!(e.contains("which has no storage: in the manifest"), "{e}");
    let e = refused(&p, "disk=none | run L=redis@10+2");
    assert!(e.contains("has no file system of its own"), "{e}");
    let s = plan("hydo/onboard/examples/redis/storage-faults.yaml");
    accepted(&s, "disk=none dseed=1 | run L=redis@10+2");
    accepted(
        &s,
        "disk=none dseed=1 | run G=redis@10+2:trunc,len=40,file=.incr.aof",
    );
    let e = refused(&s, "disk=none | run L=nosuch@10+2");
    assert!(e.contains("which this image does not have"), "{e}");
}

#[test]
fn links_ports_fakes_and_freezes_are_checked_against_the_manifest() {
    let p = plan("hydo/onboard/examples/redis-bridge/hydo.yaml");
    let names: Vec<String> = p.services.iter().map(|s| s.name.clone()).collect();
    let (a, b) = (&names[0], &names[1]);
    accepted(&p, &format!("disk=none | run N={a}-{b}@5-10:loss,60%"));
    accepted(&p, &format!("disk=none | run O={a}-*@5-8:resp:loss,100%"));
    accepted(&p, &format!("disk=none | run F={b}<{a}@5#3:2000"));
    accepted(&p, &format!("disk=none | run F={a}<workload@5#2:1000"));
    let e = refused(&p, &format!("disk=none | run N={a}-nosuch@5-10:loss,60%"));
    assert!(e.contains("which this image does not have"), "{e}");
    let e = refused(&p, &format!("disk=none | run F={a}<nosuch@5#2:1000"));
    assert!(
        e.contains("neither a service of this image nor workload"),
        "{e}"
    );
    let w = p.world.as_ref().expect("redis-bridge has a proxy");
    accepted(&p, &format!("disk=none | run H={}@3-6:drop", w.targets[0]));
    let e = refused(&p, "disk=none | run H=nosuch@3-6:drop");
    assert!(
        e.contains("not a fake, a proxy or one of their routes"),
        "{e}"
    );
    // the shop's routes
    let shop = plan("hydo/onboard/examples/shop/hydo.yaml");
    accepted(&shop, "disk=none | run H=payments.charge@0-1:drop");
    let e = refused(&shop, "disk=none | run H=payments.chrage@0-1:drop");
    assert!(e.contains("payments.charge"), "{e}");
}

#[test]
fn a_knob_of_a_reserved_letter_is_a_knob() {
    // the bug benchmark's manifest declares Q (the order service's concurrency) as a knob
    let p = plan("hydo/bench/manifest/hydo.yaml");
    accepted(&p, "disk=none | run Q=2d");
    accepted(
        &p,
        "disk=none | run K=redis@10+1 P=kafka@14+16 M=0 R=everysec",
    );
}

/// The tree's manifest of a recorded image (`hydo/onboard/recorded.json`): the release's
/// examples are the tree's (`examples/` on Linux, `examples-mac/` on the Mac).
fn manifest_of(platform: &str, dir: &str, manifest: &str) -> String {
    let dir = match dir.strip_prefix("release/examples/") {
        Some(x) if platform == "hvm" => format!("hydo/onboard/examples-mac/{x}"),
        Some(x) => format!("hydo/onboard/examples/{x}"),
        // two staged copies (hydo/onboard/rebaseline/stage.sh): the racy plan differs in an
        // env value, the generated-driver stack in its staged files; the names are the tree's
        None => dir
            .replace("/redis-composed-racy/", "/redis-composed/")
            .replace("/examples/stackgen", "/examples/stack"),
    };
    format!("{dir}/{manifest}")
}

#[test]
fn every_recorded_schedule_names_only_what_its_image_has() {
    let top = support::top();
    let text = std::fs::read_to_string(top.join("hydo/onboard/recorded.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut n = 0;
    for img in doc["images"].as_array().unwrap() {
        let b = &img["build"];
        let rel = manifest_of(
            img["platform"].as_str().unwrap(),
            b["dir"].as_str().unwrap(),
            b["manifest"].as_str().unwrap(),
        );
        let p = plan(&rel);
        for r in img["runs"].as_array().unwrap() {
            let s = r["schedule"].as_str().unwrap();
            let sc = Schedule::parse(s).unwrap();
            if let Err(e) = hydo_control::names::check(&p, &sc) {
                panic!("{} ({rel}): {s}: {e}", img["id"]);
            }
            n += 1;
        }
    }
    assert!(n > 200, "only {n} recorded schedules");
    // the bug benchmark's hand-written plans and the pinned reproducers
    let bench = plan("hydo/bench/manifest/hydo.yaml");
    let mut files: Vec<_> = std::fs::read_dir(top.join("hydo/bench/c2-plans"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for f in files {
        for l in std::fs::read_to_string(&f).unwrap().lines() {
            let l = l.trim();
            if !l.contains(" | run") || l.starts_with('#') {
                continue;
            }
            let sc = Schedule::parse(l).unwrap_or_else(|e| panic!("{}: {l}: {e}", f.display()));
            if let Err(e) = hydo_control::names::check(&bench, &sc) {
                panic!("{}: {l}: {e}", f.display());
            }
        }
    }
}

fn writes_to(items: &[Item], path: &str) -> String {
    items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Write(w),
                ..
            } if w.path == path => Some(w.data.to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn outcomes_after_cut_carries_the_last_outcomes_into_the_recovery_boot() {
    let mut p = plan("hydo/onboard/examples/redis/hydo.yaml");
    let outcomes = p.layout.outcomes();
    let sched = Schedule::parse("disk=cut | run C=12").unwrap();
    // without the key: nothing is carried, and the recovery boot writes no outcome
    let b = support::boot(&p, "silent").unwrap();
    let off = support::run_schedule(&b, &sched).unwrap();
    let Some(Ending::PowerCut { outcomes: o, .. }) = &off.run.fin.ending else {
        panic!("no power cut")
    };
    assert!(o.is_empty());
    let rc = off.recovery.as_ref().expect("a recovery boot");
    assert_eq!(writes_to(&rc.items, &outcomes), "");
    // with it: the first boot's last outcome per id, in the ending, the transcript and the
    // recovery boot's $HYDO_OUTCOMES
    p.outcomes_after_cut = true;
    let b = support::boot(&p, "silent").unwrap();
    let on = support::run_schedule(&b, &sched).unwrap();
    let file = on.run.fake.world_file(&outcomes);
    let want = hydo_control::schedule::last_outcomes(&file);
    assert!(!want.is_empty(), "the workload wrote no outcome");
    let Some(Ending::PowerCut { outcomes: o, .. }) = &on.run.fin.ending else {
        panic!("no power cut")
    };
    assert_eq!(o, &want);
    assert_eq!(on.inputs.as_ref().unwrap().outcomes, want);
    let line = format!(
        "{} {}",
        hydo_control::schedule::OUTCOMES_PREFIX,
        want.iter()
            .map(|l| l.replacen(' ', "=", 1))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert!(
        on.run
            .fin
            .transcript
            .report_lines()
            .contains(&line.as_str()),
        "no {line}"
    );
    // the transcript keeps them (a replay hands them over again)
    let t = hydo_control::Transcript::parse(&on.run.fin.transcript.to_jsonl()).unwrap();
    assert_eq!(t.ending(), on.run.fin.ending.as_ref());
    let rc = on.recovery.as_ref().expect("a recovery boot");
    let wrote = writes_to(&rc.items, &outcomes);
    let lines: Vec<String> = wrote.lines().map(str::to_string).collect();
    assert_eq!(lines, want);
    // the words of the report line give the lines back
    let words: Vec<String> = line
        .strip_prefix(hydo_control::schedule::OUTCOMES_PREFIX)
        .unwrap()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(hydo_control::schedule::outcomes_of_words(&words), want);
}

#[test]
fn last_outcomes_keeps_the_last_line_per_id_in_numeric_order() {
    let got = hydo_control::schedule::last_outcomes("10 unknown\n2 failed\n10 ack\n2 unknown\n1\n");
    assert_eq!(got, vec!["1", "2 unknown", "10 ack"]);
}

#[test]
fn a_fault_after_the_last_step_is_warned_about() {
    let p = plan("hydo/onboard/examples/redis/hydo.yaml");
    assert_eq!(p.steps, 20);
    let w = |s: &str| hydo_control::names::step_warnings(&p, &Schedule::parse(s).unwrap());
    assert_eq!(
        w("disk=none | run K=redis@40+2"),
        vec![
            "K=redis@40+2 starts at step 40, after the workload's last step (20): it does \
              nothing, and the run is the run without it"
                .to_string()
        ]
    );
    assert_eq!(w("disk=cut | run C=21 T=30:60 P=redis@25+1").len(), 3);
    // at the last step: after the last request, before the heal (a power cut there is a test)
    assert!(w("disk=cut | run C=20 K=redis@20+3 T=19:60").is_empty());
}

/// Every recorded schedule's faults start within its workload.
#[test]
fn no_recorded_fault_starts_after_the_last_step() {
    let top = support::top();
    let text = std::fs::read_to_string(top.join("hydo/onboard/recorded.json")).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
    for img in doc["images"].as_array().unwrap() {
        let b = &img["build"];
        let rel = manifest_of(
            img["platform"].as_str().unwrap(),
            b["dir"].as_str().unwrap(),
            b["manifest"].as_str().unwrap(),
        );
        let p = plan(&rel);
        for r in img["runs"].as_array().unwrap() {
            let s = r["schedule"].as_str().unwrap();
            let w = hydo_control::names::step_warnings(&p, &Schedule::parse(s).unwrap());
            assert!(w.is_empty(), "{} ({rel}): {s}: {w:?}", img["id"]);
        }
    }
}
