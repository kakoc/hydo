//! The fake outside world's controller (`src/schedule/fakes.rs`, docs/HISTORY.md M125) beyond
//! the identity corpus (which runs every schedule of the fakes manifests against the shell model,
//! tests/corpus.rs):
//!
//! - the plan reads the targets hydo-build writes (until M136 also: the model was written from
//!   the shell text hydo-build spliced);
//! - H= windows are switched where the shell switches them, and healed once;
//! - the world is carried across a power cut and the recovery's world starts from it;
//! - a carried world longer than one command is written in pieces;
//! - the recovery conversation's transcript replays to itself, as the run's does;
//! - the shell model catches deliberate mistakes in the controller's conversation;
//! - a few whole transcripts are kept readable (`tests/golden/fakes/`).

mod support;

use hydo_agent_proto::msg::{Command, Reply};
use hydo_control::plan::PlanWorld;
use hydo_control::{Ending, Plan, ReplayController, ScheduleController, Start};
use hydo_schedule::Schedule;
use support::fake::{Fake, Msg};
use support::model::{check, Conv};
use support::{boot, converse, oracle_of, plan_of, run_schedule, top, Booted, Item, Ran};

const PROXY: &str = "hydo/onboard/examples/redis/hydo-proxy.yaml";
const GRPC: &str = "hydo/onboard/examples/stack-grpc/hydo.yaml";

// The model (tests/support/model/fakes.rs) transliterates hydo-init-fakes.sh as it was when the
// shell init was deleted (M136; sha256 4869e5d4…, `git show d9202e7:hydo/onboard/hydo-init-fakes.sh`).
// The model is now the frozen description of what the shell images did.

fn booted(m: &str) -> Booted {
    let plan = plan_of(m).unwrap_or_else(|e| panic!("{m}: {e}"));
    for i in 0..20 {
        if let Ok(b) = boot(&plan, &format!("{m} fakes {i}")) {
            return b;
        }
    }
    panic!("{m}: no boot reached S1");
}

/// A schedule's run (and recovery), checked against the shell model.
fn ran(b: &Booted, text: &str) -> Ran {
    let s = Schedule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
    let r = run_schedule(b, &s).unwrap_or_else(|e| panic!("{text}: {e}"));
    check(
        &b.plan,
        &r.run.items,
        Conv::Run,
        &s.guest_text(),
        (&[], &[]),
        Some(&b.s1.2),
    )
    .unwrap_or_else(|e| panic!("{text}: the run differs from the shell:\n{e}"));
    if let Some(rc) = &r.recovery {
        let (cmd, a, f) = oracle_of(&r.run.items);
        check(
            &b.plan,
            &rc.items,
            Conv::Recovery,
            &cmd,
            (&a, &f),
            Some(&b.s0.2),
        )
        .unwrap_or_else(|e| panic!("{text}: the recovery differs from the shell:\n{e}"));
    }
    if std::env::var_os("HYDO_SHOW").is_some() {
        eprintln!("--- {text}\n{}", r.run.fin.transcript.to_jsonl());
        if let Some(rc) = &r.recovery {
            eprintln!("--- recovery\n{}", rc.fin.transcript.to_jsonl());
        }
    }
    r
}

/// The verdict line, or "cut".
fn verdict(r: &Ran) -> String {
    match &r.run.fin.ending {
        Some(Ending::Verdict(v)) => v.clone(),
        Some(Ending::PowerCut { .. }) => "cut".into(),
        e => format!("{e:?}"),
    }
}

/// One line per main command and report, for reading a conversation's order.
fn trace(items: &[Item]) -> Vec<String> {
    let mut v = Vec::new();
    for it in items {
        match it {
            Item::Send {
                by,
                cmd: Command::Run(r),
                ..
            } if by == "main" && r.argv[0] == "/bin/hydo-world" => {
                v.push(format!("ctl {}", r.argv[2..].join(" ")))
            }
            Item::Send {
                by,
                cmd: Command::Spawn(s),
                ..
            } if by == "main" => {
                let from = s
                    .env
                    .iter()
                    .flatten()
                    .find_map(|e| e.strip_prefix("HYDO_FROM="))
                    .map(|f| format!(" from={f}"))
                    .unwrap_or_default();
                v.push(format!("spawn {}{from}", s.h));
            }
            Item::Send {
                by,
                cmd: Command::Write(w),
                ..
            } if by == "main" && w.path.contains("world") => {
                v.push(format!("write {} {:?}", w.path, w.data))
            }
            Item::Report { by, line, .. } if by == "main" => v.push(format!("report {line}")),
            _ => {}
        }
    }
    v
}

/// `want` is a subsequence of `got`.
fn in_order(got: &[String], want: &[&str]) -> bool {
    let mut i = 0;
    for g in got {
        if i < want.len() && g.starts_with(want[i]) {
            i += 1;
        }
    }
    i == want.len()
}

#[test]
fn the_plan_reads_hydo_builds_targets() {
    let ctx = hydo_build::Ctx {
        onboard: top().join("hydo/onboard"),
        release: hydo_build::Release::None,
        inputs: Default::default(),
    };
    let run = |m: &str, flag: &str| -> String {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut io = hydo_build::Io {
            out: &mut out,
            err: &mut err,
        };
        let path = top().join(m).to_string_lossy().into_owned();
        let rc = hydo_build::main_with(&ctx, &[path, flag.to_string()], &mut io);
        assert_eq!(rc, 0, "{m} {flag}: {}", String::from_utf8_lossy(&err));
        String::from_utf8(out).unwrap()
    };
    // every manifest with a fake world, also those another seam still refuses
    let mut n = 0;
    for m in support::manifests() {
        let (data, _) = hydo_manifest::for_explorer(&top().join(&m)).unwrap();
        let Some(w) = PlanWorld::of(&data) else {
            continue;
        };
        n += 1;
        let shell = run(&m, "--print-shell");
        let line = |k: &str| -> String {
            let l = shell
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("{m}: no {k} in manifest.sh"));
            l.trim_matches('\'').to_string()
        };
        assert_eq!(line("HYDO_WORLD"), w.bin, "{m}");
        assert_eq!(line("HYDO_WORLD_TARGETS"), w.targets.join(" "), "{m}");
        assert!(run(&m, "--print-world").contains("\"sock\""), "{m}");
        if let Ok(p) = plan_of(&m) {
            assert_eq!(p.world.as_ref(), Some(&w), "{m}");
        }
    }
    assert!(n >= 5, "only {n} manifests with a fake world");
    // a manifest without one has none
    let p = plan_of("hydo/onboard/examples/redis/hydo.yaml").unwrap();
    assert!(p.world.is_none());
}

#[test]
fn protocol_faults_are_switched_at_their_windows_and_healed_once() {
    let b = booted(PROXY);
    let mut seen = 0;
    for d in 1..40 {
        let text = format!("disk=none dseed={d} | run H=redisp@2-5:drop H=nosuch@1-2:reset");
        let r = ran(&b, &text);
        if verdict(&r).contains("protocol fault") || verdict(&r).contains("ERROR") {
            continue; // the scripted world refused the rule or the guest failed: other paths
        }
        let t = trace(&r.run.items);
        let want = [
            "report HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.protocol\"",
            "spawn workload from=0",
            "ctl on 1 redisp@2-5:drop",
            "report HYDO-ASSERT {\"t\":\"reachable\",\"id\":\"fault.protocol\",\"ok\":true",
            "spawn workload from=2",
            "ctl off 1",
            "spawn workload from=5",
            "ctl heal",
            "report hydo-explore-acked:",
            "report hydo-explore-workload-done",
            "spawn fakes:summary",
            "report HYDO-ORACLE:",
        ];
        assert!(in_order(&t, &want), "{text}:\n{}", t.join("\n"));
        assert_eq!(t.iter().filter(|l| *l == "ctl heal").count(), 1, "{text}");
        assert_eq!(
            t.iter().filter(|l| l.starts_with("spawn workload")).count(),
            3,
            "{text}: the steps are cut at 2 and 5 only (nosuch is ignored)"
        );
        let notes: Vec<&str> = r
            .run
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Note { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert!(notes.contains(&"hydo-ob: H: no fake or proxy nosuch, ignored"));
        assert!(notes.contains(&"hydo-ob: PROTOCOL FAULT 1 on before step 2: H=redisp@2-5:drop"));
        assert!(notes.contains(&"hydo-ob: protocol fault 1 off before step 5"));
        seen += 1;
        if seen == 3 {
            return;
        }
    }
    panic!("only {seen} clean runs");
}

#[test]
fn the_world_is_carried_across_a_power_cut() {
    for m in [PROXY, GRPC] {
        let b = booted(m);
        let mut seen = 0;
        for d in 1..60 {
            let text = format!(
                "disk=cut dseed={d} | run H={}@1-6:drop C=4",
                b.plan.world.as_ref().unwrap().targets[0]
            );
            let r = ran(&b, &text);
            let Some(Ending::PowerCut { acked, world, .. }) = r.run.fin.ending.clone() else {
                continue;
            };
            let Some(rc) = &r.recovery else {
                unreachable!()
            };
            // the run: the dump's records end the acked line, and are the hand-over's world
            let (_, line, _) = oracle_of(&r.run.items);
            assert_eq!(
                line,
                acked.iter().chain(&world).cloned().collect::<Vec<_>>()
            );
            assert!(acked.iter().all(|a| !a.starts_with("w:")));
            assert!(world.iter().all(|a| a.starts_with("w:")), "{world:?}");
            let t = trace(&r.run.items);
            assert!(
                in_order(
                    &t,
                    &[
                        "ctl on 1",
                        "spawn workload from=1",
                        "spawn fakes:dump",
                        "report hydo-explore-acked:"
                    ]
                ),
                "{text}:\n{}",
                t.join("\n")
            );
            assert!(!t.iter().any(|l| l == "ctl heal"), "a cut heals nothing");
            // the recovery: world.carry holds exactly the carried records, the world starts
            // from it, and the id files never see a record
            let rt = trace(&rc.items);
            let carry: String = world.iter().map(|w| format!("{w}\n")).collect();
            let wanted = format!("write /tmp/hydo/world.carry {carry:?}");
            assert!(rt.contains(&wanted), "{text}:\n{}", rt.join("\n"));
            let started = rc.items.iter().any(|i| {
                matches!(i, Item::Send { cmd: Command::Spawn(s), .. }
                if s.h == "fakes:world" && s.argv[3].ends_with("--carry /tmp/hydo/world.carry"))
            });
            assert!(started, "{text}");
            for i in &rc.items {
                if let Item::Send {
                    cmd: Command::Write(w),
                    ..
                } = i
                {
                    if w.path.ends_with("/acked") || w.path.ends_with("/failed") {
                        assert!(!w.data.contains("w:"), "{text}: {w:?}");
                    }
                }
            }
            // the scripted world loaded them: its summary in the recovery lists each one
            let summary: Vec<&str> = rc
                .items
                .iter()
                .filter_map(|i| match i {
                    Item::Report { line, .. } => line.strip_prefix("hydo-world: "),
                    _ => None,
                })
                .collect();
            let recovered = matches!(&rc.fin.ending, Some(Ending::Verdict(v)) if !v.contains("did not start") && !v.contains("unmountable") && !v.contains("cannot recover"));
            if recovered && world.first().is_some_and(|w| w != "w:trunc") {
                for w in &world {
                    let seq = w.trim_start_matches("w:").split(',').next().unwrap();
                    assert!(
                        summary.iter().any(|l| l.starts_with(&format!("{seq} "))),
                        "{text}: {w} is not in the recovery's world: {summary:?}"
                    );
                }
                seen += 1;
            }
            if seen == 3 {
                break;
            }
        }
        assert_eq!(seen, 3, "{m}: too few recoveries that carried a world");
    }
}

/// The extension's rare paths, each reached on the scripted world and checked against the model.
#[test]
fn the_rare_paths_are_reached() {
    let mut hit: std::collections::BTreeMap<&str, usize> = Default::default();
    let notes = |items: &[Item]| -> Vec<String> {
        items
            .iter()
            .filter_map(|i| match i {
                Item::Note { line, .. } => Some(line.clone()),
                _ => None,
            })
            .collect()
    };
    // boots whose world does not start
    let plan = plan_of(PROXY).unwrap();
    for i in 0..120 {
        let salt = format!("rare boot {i}");
        let mut ctl = ScheduleController::boot(plan.clone()).unwrap();
        let c = converse(
            Fake::boot(&salt, 16),
            &mut ctl,
            Start::Boot,
            hydo_control::Header {
                controller: String::new(),
                conversation: "boot".into(),
                schedule: None,
                cadence: "default".into(),
                start: "boot".into(),
            },
        );
        check(&plan, &c.items, Conv::Boot, "provision", (&[], &[]), None)
            .unwrap_or_else(|e| panic!("{salt}: {e}"));
        if matches!(&c.fin.ending, Some(Ending::Verdict(v)) if v.contains("did not start")) {
            *hit.entry("boot: the world does not start").or_default() += 1;
            if notes(&c.items)
                .iter()
                .any(|n| n.starts_with("hydo-world: "))
            {
                *hit.entry("boot: tail of world.out").or_default() += 1;
            }
        }
    }
    for m in [PROXY, GRPC] {
        let b = booted(m);
        let t = b.plan.world.as_ref().unwrap().targets[0].clone();
        for d in 0..80 {
            for text in [
                format!("disk=none dseed={d} | run H={t}@1-3:drop H={t}@2-6:reset"),
                format!("disk=cut dseed={d} | run H={t}@1-3:drop C=5"),
            ] {
                let r = ran(&b, &text);
                let v = verdict(&r);
                let ns = notes(&r.run.items);
                if v.contains("ERROR protocol fault H=") {
                    *hit.entry("run: hydo-world refuses a rule").or_default() += 1;
                }
                if ns
                    .iter()
                    .any(|n| n.contains("world dump failed: hydo-world ctl: dial"))
                {
                    *hit.entry("cut: the dump fails").or_default() += 1;
                }
                let acked = oracle_of(&r.run.items).1;
                if acked.iter().any(|a| a == "w:trunc") {
                    *hit.entry("cut: w:trunc carried").or_default() += 1;
                }
                let healed = ns.iter().any(|n| n == "hydo-ob: protocol faults healed");
                let tried = trace(&r.run.items).iter().any(|l| l == "ctl heal");
                if tried && !healed {
                    *hit.entry("heal: ctl heal fails").or_default() += 1;
                }
                if tried && healed {
                    *hit.entry("heal: healed").or_default() += 1;
                }
                if let Some(rc) = &r.recovery {
                    if matches!(&rc.fin.ending, Some(Ending::Verdict(v)) if v.contains("did not start"))
                    {
                        *hit.entry("recovery: the world does not start").or_default() += 1;
                    }
                    if rc.items.iter().any(|i| matches!(i, Item::Report { line, .. } if line.starts_with("hydo-world: "))) {
                        *hit.entry("recovery: summary").or_default() += 1;
                    }
                }
            }
        }
    }
    eprintln!("{hit:?}");
    for k in [
        "boot: the world does not start",
        "boot: tail of world.out",
        "run: hydo-world refuses a rule",
        "cut: the dump fails",
        "cut: w:trunc carried",
        "heal: ctl heal fails",
        "heal: healed",
        "recovery: the world does not start",
        "recovery: summary",
    ] {
        assert!(
            hit.get(k).copied().unwrap_or(0) > 0,
            "never reached: {k} ({hit:?})"
        );
    }
}

#[test]
fn a_long_carried_world_is_written_in_pieces() {
    let b = booted(PROXY);
    let world: Vec<String> = (0..300)
        .map(|i| format!("w:{i},e,payments,charge,o{i},key-{i:06}"))
        .collect();
    let mut rc = ScheduleController::recovery(
        b.plan.clone(),
        vec!["3".into(), "w:999,v,-,cancel,o3,".into()],
        vec![],
        &b.s0.2,
    )
    .unwrap()
    .with_world(&world);
    let c = converse(
        Fake::resume(&b.s0.0, "long world", 16),
        &mut rc,
        Start::Resume(b.s0.1.clone()),
        hydo_control::Header {
            controller: String::new(),
            conversation: "recovery".into(),
            schedule: None,
            cadence: "default".into(),
            start: "resume S0".into(),
        },
    );
    let writes: Vec<&hydo_agent_proto::msg::Write> = c
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Write(w),
                ..
            } if w.path == "/tmp/hydo/world.carry" => Some(w),
            _ => None,
        })
        .collect();
    assert!(writes.len() >= 3, "{}", writes.len());
    assert_eq!(writes[0].append, None);
    assert!(writes[1..].iter().all(|w| w.append == Some(true)));
    assert!(writes.iter().all(|w| w.data.len() <= 3000));
    // the records among the acknowledged ids come first, as the shell's grep finds them there
    let all: String = writes.iter().map(|w| w.data.as_str()).collect();
    let want: String = std::iter::once("w:999,v,-,cancel,o3,".to_string())
        .chain(world.iter().cloned())
        .map(|w| format!("{w}\n"))
        .collect();
    assert_eq!(all, want);
    let acked: Vec<&str> = c
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Send {
                cmd: Command::Write(w),
                ..
            } if w.path == "/tmp/hydo/acked" => Some(w.data.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(acked, ["3\n"]);
    assert!(c.items.iter().any(|i| matches!(i, Item::Note { line, .. }
        if line == "hydo-ob: recovery boot: the fake world starts from 301 carried records")));
}

#[test]
fn the_recovery_transcript_replays_to_itself() {
    let b = booted(PROXY);
    let mut n = 0;
    for d in 1..30 {
        let text = format!("disk=cut dseed={d} | run H=redisp@0-3:reset C=2");
        let s = Schedule::parse(&text).unwrap();
        let r = ran(&b, &text);
        let (Some(rc), Some(Ending::PowerCut { world, .. })) = (&r.recovery, &r.run.fin.ending)
        else {
            continue;
        };
        let salt = format!("{s} recovery");
        let mut rep = ReplayController::new(&rc.fin.transcript);
        let again = converse(
            Fake::resume(&b.s0.0, &salt, support::cadence(Some(&s))),
            &mut rep,
            Start::Resume(b.s0.1.clone()),
            rc.fin.transcript.header.clone(),
        );
        let mut want = rc.fin.transcript.clone();
        want.header.controller = "replay".into();
        assert_eq!(again.fin.transcript.to_jsonl(), want.to_jsonl(), "{text}");
        // and the transcript file keeps the hand-over's world
        let back = hydo_control::Transcript::parse(&r.run.fin.transcript.to_jsonl()).unwrap();
        assert_eq!(back.ending(), r.run.fin.ending.as_ref());
        if !world.is_empty() {
            assert!(r.run.fin.transcript.to_jsonl().contains("\"world\":[\"w:"));
        }
        n += 1;
    }
    assert!(n >= 10, "{n}");
}

/// A conversation changed as a controller with one mistake would have it; the model must say
/// where it differs from the shell.
fn mutant(items: &[Item], f: impl Fn(&mut Vec<Item>) -> bool) -> Option<Vec<Item>> {
    let mut v = items.to_vec();
    f(&mut v).then_some(v)
}

fn pos(items: &[Item], f: impl Fn(&Item) -> bool) -> Option<usize> {
    items.iter().position(f)
}

fn is_ctl(i: &Item, what: &str) -> bool {
    matches!(i, Item::Send { cmd: Command::Run(r), .. } if r.argv[0] == "/bin/hydo-world" && r.argv[2] == what)
}

fn is_spawn(i: &Item, h: &str) -> bool {
    matches!(i, Item::Send { cmd: Command::Spawn(s), .. } if s.h == h)
}

#[test]
fn the_shell_model_catches_deliberate_mistakes() {
    let b = booted(PROXY);
    // a clean run with a window, and a cut with its recovery
    let (mut plain, mut cut) = (None, None);
    for d in 1..60 {
        if plain.is_none() {
            let t = format!("disk=none dseed={d} | run H=redisp@2-5:drop");
            let r = ran(&b, &t);
            if verdict(&r).starts_with("HYDO-ORACLE: OK")
                || verdict(&r).starts_with("HYDO-ORACLE: VIOLATION")
            {
                plain = Some((t, r));
            }
        }
        if cut.is_none() {
            let t = format!("disk=cut dseed={d} | run H=redisp@1-6:drop C=4");
            let r = ran(&b, &t);
            if let (Some(rc), Some(Ending::PowerCut { world, .. })) =
                (&r.recovery, &r.run.fin.ending)
            {
                if world.len() > 1
                    && matches!(&rc.fin.ending, Some(Ending::Verdict(v)) if !v.contains("did not start"))
                {
                    cut = Some((t, r));
                }
            }
        }
    }
    let (pt, p) = plain.expect("a clean run");
    let (ct, c) = cut.expect("a cut that carried a world");
    let plan = &b.plan;
    let run_ok =
        |items: &[Item], cmd: &str| check(plan, items, Conv::Run, cmd, (&[], &[]), Some(&b.s1.2));
    let (ocmd, oa, of) = oracle_of(&c.run.items);
    let rec_ok = |items: &[Item]| {
        check(
            plan,
            items,
            Conv::Recovery,
            &ocmd,
            (&oa, &of),
            Some(&b.s0.2),
        )
    };
    let pcmd = Schedule::parse(&pt).unwrap().guest_text();
    let ccmd = Schedule::parse(&ct).unwrap().guest_text();
    assert!(run_ok(&p.run.items, &pcmd).is_ok());
    assert!(run_ok(&c.run.items, &ccmd).is_ok());
    let rc = c.recovery.as_ref().unwrap();
    assert!(rec_ok(&rc.items).is_ok());

    type Edit = Box<dyn Fn(&mut Vec<Item>) -> bool>;
    type Mistake = (&'static str, bool, Edit);
    let mistakes: Vec<Mistake> = vec![
        (
            "the rule switched on after the step it guards started",
            false,
            Box::new(|v: &mut Vec<Item>| {
                let Some(i) = pos(v, |x| is_ctl(x, "on")) else {
                    return false;
                };
                let Some(j) = (i..v.len()).find(|k| is_spawn(&v[*k], "workload")) else {
                    return false;
                };
                let it = v.remove(i);
                v.insert(j, it);
                true
            }),
        ),
        (
            "the token text changed on its way to hydo-world",
            false,
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::Send {
                        cmd: Command::Run(r),
                        ..
                    } = x
                    {
                        if r.argv.get(2).is_some_and(|a| a == "on") {
                            r.argv[4] = r.argv[4].replace("drop", "reset");
                            return true;
                        }
                    }
                }
                false
            }),
        ),
        (
            "the rule never switched off at the end of its window",
            false,
            Box::new(|v: &mut Vec<Item>| {
                let Some(i) = pos(v, |x| is_ctl(x, "off")) else {
                    return false;
                };
                v.remove(i);
                true
            }),
        ),
        (
            "no heal at the end of fault injection",
            false,
            Box::new(|v: &mut Vec<Item>| {
                let Some(i) = pos(v, |x| is_ctl(x, "heal")) else {
                    return false;
                };
                v.remove(i);
                true
            }),
        ),
        (
            "the step range not cut at the window's end",
            false,
            Box::new(|v: &mut Vec<Item>| {
                // the workload piece [2,5) and [5,20) as one call [2,20)
                for x in v.iter_mut() {
                    if let Item::Send {
                        cmd: Command::Spawn(s),
                        ..
                    } = x
                    {
                        if let Some(e) = s.env.as_mut() {
                            if e.iter().any(|x| x == "HYDO_FROM=2") {
                                for y in e.iter_mut() {
                                    if y == "HYDO_TO=5" {
                                        *y = "HYDO_TO=20".into();
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
                false
            }),
        ),
        (
            "the summary left out before the verdict",
            false,
            Box::new(|v: &mut Vec<Item>| {
                let n = v.len();
                v.retain(
                    |x| !matches!(x, Item::Report { line, .. } if line.starts_with("hydo-world: ")),
                );
                n != v.len()
            }),
        ),
        (
            "the world's summary not waited for",
            false,
            Box::new(|v: &mut Vec<Item>| {
                let Some(i) = pos(v, |x| is_spawn(x, "fakes:summary")) else {
                    return false;
                };
                let Item::Send { id, .. } = v[i].clone() else {
                    return false;
                };
                let Some(e) = pos(
                    v,
                    |x| matches!(x, Item::Got { msg: Msg::Event(hydo_agent_proto::msg::Event::Exit(e)), .. } if e.id == id),
                ) else {
                    return false;
                };
                let ex = v.remove(e);
                v.push(ex);
                true
            }),
        ),
        (
            "the dump not carried on the acked line",
            true,
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::Report { line, .. } = x {
                        if line.starts_with("hydo-explore-acked:") && line.contains("w:") {
                            *line = line.split(" w:").next().unwrap().to_string() + " ";
                            return true;
                        }
                    }
                }
                false
            }),
        ),
        (
            "the hand-over without the carried world",
            true,
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::End {
                        end: Ending::PowerCut { world, .. },
                        ..
                    } = x
                    {
                        if world.is_empty() {
                            return false;
                        }
                        world.clear();
                        return true;
                    }
                }
                false
            }),
        ),
        (
            "the dump taken before the failed ids were written",
            true,
            Box::new(|v: &mut Vec<Item>| {
                let Some(d) = pos(v, |x| is_spawn(x, "fakes:dump")) else {
                    return false;
                };
                let Some(w) = (0..d).rev().find(|k| matches!(&v[*k], Item::Send { cmd: Command::Write(w), .. } if w.path.ends_with("/failed"))) else { return false };
                let it = v.remove(d);
                v.insert(w, it);
                true
            }),
        ),
    ];
    let mut caught = 0;
    for (what, on_cut, f) in &mistakes {
        let (items, cmd) = if *on_cut {
            (&c.run.items, &ccmd)
        } else {
            (&p.run.items, &pcmd)
        };
        let Some(bad) = mutant(items, f) else {
            panic!("{what}: the runs give no place for this mistake");
        };
        let e = run_ok(&bad, cmd).expect_err(what);
        eprintln!("caught: {what}: {}", e.lines().next().unwrap_or(""));
        caught += 1;
    }
    // the boot: the world started after the first service
    let mut bt = b.boot.items.clone();
    let w = pos(&bt, |x| is_spawn(x, "fakes:world")).unwrap();
    let first = pos(&bt, |x| is_spawn(x, &plan.services[0].name)).unwrap();
    assert!(w < first);
    let it = bt.remove(w);
    bt.insert(first, it);
    let e = check(plan, &bt, Conv::Boot, "provision", (&[], &[]), None).expect_err("boot order");
    eprintln!(
        "caught: the world started after the services: {}",
        e.lines().next().unwrap_or("")
    );
    caught += 1;
    // the recovery: a record left in the acked file, a record lost from the carry, the world
    // started without its carry
    let rmistakes: Vec<(&str, Edit)> = vec![
        (
            "a carried record written into the acked ids",
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::Send {
                        cmd: Command::Write(w),
                        ..
                    } = x
                    {
                        if w.path == "/tmp/hydo/acked" {
                            w.data.push_str("w:1,e,pay,charge,o1,\n");
                            return true;
                        }
                    }
                }
                false
            }),
        ),
        (
            "a carried record lost",
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::Send {
                        cmd: Command::Write(w),
                        ..
                    } = x
                    {
                        if w.path == "/tmp/hydo/world.carry" && w.data.contains('\n') {
                            let mut ls: Vec<&str> = w.data.lines().collect();
                            ls.pop();
                            w.data = ls.iter().map(|l| format!("{l}\n")).collect();
                            return true;
                        }
                    }
                }
                false
            }),
        ),
        (
            "the recovery's world started without its carry",
            Box::new(|v: &mut Vec<Item>| {
                for x in v.iter_mut() {
                    if let Item::Send {
                        cmd: Command::Spawn(s),
                        ..
                    } = x
                    {
                        if s.h == "fakes:world" {
                            s.argv[3] = s.argv[3].replace("--carry /tmp/hydo/world.carry", "");
                            return true;
                        }
                    }
                }
                false
            }),
        ),
    ];
    for (what, f) in &rmistakes {
        let bad = mutant(&rc.items, f).unwrap_or_else(|| panic!("{what}: no place for it"));
        let e = rec_ok(&bad).expect_err(what);
        eprintln!("caught: {what}: {}", e.lines().next().unwrap_or(""));
        caught += 1;
    }
    assert_eq!(caught, mistakes.len() + 1 + rmistakes.len());
    // the read of world.dump is the controller's: a Read reply, kept
    assert!(c.run.items.iter().any(|i| matches!(
        i,
        Item::Got {
            msg: Msg::Reply(_, Reply::Read(_)),
            ..
        }
    )));
}

/// Plans of the fakes manifests that another seam still refuses, with that feature taken out, so
/// that the controller also meets the shop's fakes, webhooks and routes. Not a claim about those
/// manifests: only the fakes part is checked here.
fn without_liveness(m: &str) -> Plan {
    let (mut data, meta) = hydo_manifest::for_explorer(&top().join(m)).unwrap();
    assert!(meta.is_none());
    if let Some(a) = data.get_mut("invariants").and_then(|x| x.as_array_mut()) {
        a.retain(|i| i.get("type").and_then(|t| t.as_str()) != Some("eventually"));
    }
    Plan::from_manifest(&hydo_manifest::Manifest::from_validated(data)).unwrap()
}

#[test]
fn the_shop_without_its_liveness_checks() {
    let plan = without_liveness("hydo/onboard/examples/shop/hydo.yaml");
    let w = plan.world.clone().unwrap();
    assert!(
        w.targets.iter().any(|t| t == "payments.charge"),
        "{:?}",
        w.targets
    );
    let b = (0..20)
        .find_map(|i| boot(&plan, &format!("shop {i}")).ok())
        .expect("a boot");
    let mut n = 0;
    for (i, t) in w.targets.iter().enumerate() {
        for text in [
            format!("disk=none dseed={i} | run H={t}@0-1:drop"),
            format!("disk=none dseed={i} | run H={t}@1-3:status=500,effect=yes H={t}@2-4:dup"),
            format!("disk=cut dseed={i} | run H={t}@0-2:late=5000 C=1"),
            format!("disk=cut dseed={i} | run H={t}@6-9:reset,p=50 C=7"),
        ] {
            ran(&b, &text);
            n += 1;
        }
    }
    assert!(n >= 16, "{n}");
}

#[test]
fn readable_transcripts() {
    let b = booted(PROXY);
    let dir = support::golden_dir().join("fakes");
    let mut index = String::new();
    for (i, text) in [
        "disk=none | run H=redisp@2-5:drop",
        "disk=cut dseed=3 | run H=redisp@1-6:drop C=4",
        "disk=none | run H=redisp@3-3:drop H=nosuch@1-2:reset K=redis@4+2",
    ]
    .iter()
    .enumerate()
    {
        let r = ran(&b, text);
        let mut body = r.run.fin.transcript.to_jsonl();
        if let Some(rc) = &r.recovery {
            body.push_str(&rc.fin.transcript.to_jsonl());
        }
        let f = format!("redis-proxy-{i}.jsonl");
        index.push_str(&format!("{f}\t{text}\n"));
        let p = dir.join(&f);
        if support::bless() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&p, &body).unwrap();
        } else {
            let want = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("{}: {e} (HYDO_BLESS=1 records it)", p.display()));
            assert_eq!(
                want,
                body,
                "{} differs (HYDO_BLESS=1 re-records it)",
                p.display()
            );
        }
    }
    let ip = dir.join("INDEX");
    if support::bless() {
        std::fs::write(&ip, &index).unwrap();
    } else {
        assert_eq!(std::fs::read_to_string(&ip).unwrap_or_default(), index);
    }
}

/// A manifest with both `fakes:`/`proxies:` and `errors:` (no example has both).
const PROXY_ERRORS: &str = "hydo/crates/hydo-control/tests/manifests/redis-proxy-errors.yaml";

#[test]
fn error_logs_with_fakes_print_in_the_shells_order() {
    // The order the shell images had (hydo-build spliced hydo-init-errlog.sh after the fakes, so
    // the console got the error records, then the fakes' summary lines, then the final
    // invariants' assertion records and the verdict line; M127 fix 2, checked against
    // `hydo-build --print-init` until M136): the controller's report lines in that order, every
    // run checked against the shell model
    let plan = plan_of(PROXY_ERRORS).unwrap_or_else(|e| panic!("{e}"));
    assert!(plan.errors.is_some() && plan.world.is_some());
    let b = (0..20)
        .find_map(|i| boot(&plan, &format!("proxy errors {i}")).ok())
        .expect("a boot that reaches S1");
    let mut ordered = 0;
    for text in [
        "disk=none | run",
        "disk=none | run H=redisp@2-5:drop",
        "disk=none | run H=redisp@1-3:reset K=redis@4+2",
        "disk=none | run P=redis@3+2 H=redisp@6-9:delay=300",
        "disk=cut | run H=redisp@1-6:drop C=8",
    ] {
        let r = ran(&b, text);
        let lines: Vec<&str> = r
            .run
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Report { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        if verdict(&r) == "cut" {
            continue;
        }
        let sum = lines
            .iter()
            .position(|l| l.starts_with("HYDO-ERRLOG-SUM "))
            .unwrap_or_else(|| panic!("{text}: no error-log summary: {lines:?}"));
        let world: Vec<usize> = (0..lines.len())
            .filter(|&i| lines[i].starts_with("hydo-world: "))
            .collect();
        let finals: Vec<usize> = (0..lines.len())
            .filter(|&i| {
                lines[i].starts_with("HYDO-ASSERT {")
                    && plan
                        .invariants
                        .iter()
                        .filter(|v| v.final_)
                        .any(|v| lines[i].contains(&format!("\"id\":\"{}\",", v.id)))
            })
            .collect();
        // (the scripted world's summary can be empty)
        assert!(
            world.iter().all(|&w| w > sum),
            "{text}: a summary line before the error records: {lines:?}"
        );
        assert!(
            finals
                .iter()
                .all(|&f| f > sum && world.iter().all(|&w| f > w)),
            "{text}: an assertion record before the error records or the summary: {lines:?}"
        );
        if !finals.is_empty() && !world.is_empty() {
            ordered += 1;
        }
    }
    assert!(ordered >= 3, "only {ordered} runs with final records");
}
