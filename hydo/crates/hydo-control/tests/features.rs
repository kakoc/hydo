//! The feature controllers of M122 (liveness, clock, BUGGIFY, the recovery inputs) on the
//! explorer's identity corpus (docs/HISTORY.md M122).
//!
//! `tests/corpus.rs` already runs every core manifest, and since M122 that includes the
//! manifests with `eventually` invariants (redis/hydo-live.yaml, compose-voting/hydo-loopback),
//! `T=` on every manifest and `B=` on the `buggify: true` ones (node-kv, rust-kv, python-kv).
//! This test runs the same streams on the manifests that use these features, plus variants the
//! examples do not have (no example sets `guest.clock_after_cut`; none has two eventually
//! invariants, a `start` anchor or zero steps), and schedules written here for the paths the
//! explorer does not draw (large and small clock steps, a step at the last step, two B= tokens).
//! Every run and recovery boot is checked against the shell model, every run's and every
//! recovery's transcript is typed again by the ReplayController and must equal the policy's,
//! every feature path must be reached, and the transcripts are recorded as a golden
//! (`tests/golden/features-quick.txt`).
//!
//! The second test shows that the shell model catches mistakes in these features: it takes
//! conversations of the first test, makes one deliberate mistake at a time in what the
//! controller did, and checks that the model refuses each.

mod support;

use hydo_agent_proto::msg::{ClockSet, Command, Run};
use hydo_control::plan::{PlanInvariant, PlanLive};
use hydo_control::{Ending, GuestState, Plan, ReplayController, Start, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use std::collections::{BTreeMap, BTreeSet};
use support::fake::{Fake, Msg};
use support::model::{check, Conv};
use support::{boot, plan_of, run_schedule, top, Booted, Item};

// ------------------------------------------------------------------ the identity streams
// (the same as tests/corpus.rs, which follows hydo-explore's tests/identity.rs)

const FLAGS: [&str; 14] = [
    "TIMING",
    "NEW_FAULTS",
    "DURABLE",
    "MORE_FAULTS",
    "PREEMPT",
    "TURNS",
    "BURSTS",
    "BUGGIFY",
    "CONCURRENCY",
    "FREEZE_CALL",
    "PROTOCOL_FAULTS",
    "GRPC",
    "COMPOSE",
    "KNOB_ANY",
];

fn crc32(b: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(b);
    c.sum()
}

fn salt_of(text: &str) -> u64 {
    u64::from_str_radix(&support::sha256_hex(text.as_bytes())[..15], 16).unwrap()
}

fn flags_for(sd: u64, salt: u64) -> Vec<(&'static str, bool)> {
    let h = hydo_explore::rng::mix64(sd.wrapping_mul(0x1000193) ^ salt);
    FLAGS
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let on = match sd {
                1 => false,
                2 => true,
                _ => (h >> i) & 1 == 1,
            };
            (*f, on)
        })
        .collect()
}

fn set_flags(ex: &mut Ex, fl: &[(&str, bool)]) {
    let b = |k: &str| fl.iter().any(|(f, on)| *f == k && *on);
    ex.timing = b("TIMING");
    ex.new_faults = b("NEW_FAULTS");
    ex.more_faults = b("MORE_FAULTS");
    ex.preempt = b("PREEMPT");
    ex.turns = b("TURNS");
    ex.bursts = b("BURSTS");
    ex.buggify = b("BUGGIFY");
    ex.concurrency = b("CONCURRENCY");
    ex.freeze_call = b("FREEZE_CALL");
    ex.protocol_faults = b("PROTOCOL_FAULTS");
    ex.compose = b("COMPOSE");
    ex.grpc_on = b("GRPC");
    ex.knob_rule_any = b("KNOB_ANY");
}

fn identity_schedules(m: &str, seeds: u64) -> Vec<String> {
    let mut out = Vec::new();
    for (backend, disk, tc) in [
        ("hvx", false, false),
        ("hvm", true, true),
        ("hvm", false, false),
    ] {
        let label = if backend == "hvx" {
            format!("{m}/hvx")
        } else {
            format!(
                "{m}/hvm/{}{}",
                if disk { "True" } else { "False" },
                if tc { "True" } else { "False" }
            )
        };
        let mut ex = Ex::default();
        let sp = Space::from_path(&top().join(m)).expect("the explorer reads the manifest");
        ex.grpc_targets = grpc_targets(Some(&sp.m));
        sp.apply(&mut ex);
        ex.backend = if backend == "hvm" {
            Backend::Hvm
        } else {
            Backend::Hvx
        };
        ex.hvm_disk = disk;
        ex.hvm_tc = tc;
        ex.cf_range = Some((1, 300));
        let strats: &[&str] = if ex.norders >= 15 {
            &["random", "swarm", "swarm-pos", "process"]
        } else {
            &["random", "swarm", "process"]
        };
        let salt = salt_of(&label);
        for strat in strats {
            for sd in 1..=seeds {
                let fl = flags_for(sd, salt ^ u64::from(crc32(strat.as_bytes())));
                set_flags(&mut ex, &fl);
                let durable = fl.iter().any(|(f, on)| *f == "DURABLE" && *on);
                out.push(ex.render(&ex.compose_seed(strat, sd, durable)));
            }
        }
    }
    out
}

// ------------------------------------------------------------------ the variants

/// One plan to run: a label, the manifest whose streams it uses, the plan.
struct Variant {
    label: String,
    manifest: String,
    plan: Plan,
}

fn second_eventually(p: &mut Plan, after: &str, within: i64, poll: i64) {
    let k = p.invariants.len();
    p.invariants.push(PlanInvariant {
        key: format!("i{k}"),
        id: format!("extra.settled_{after}"),
        kind: "eventually".into(),
        msg: format!("a second \"eventually\" check, anchored at {after}"),
        cmd: format!("echo checking {after}; test -s $HYDO_ACKED"),
        every: String::new(),
        final_: false,
        live: Some(PlanLive {
            within,
            after: after.into(),
            poll,
        }),
    });
}

fn variants() -> Vec<Variant> {
    let mut v = Vec::new();
    let mut add = |label: &str, manifest: &str, f: &dyn Fn(&mut Plan)| {
        let mut plan = plan_of(manifest).unwrap_or_else(|e| panic!("{manifest}: {e}"));
        f(&mut plan);
        v.push(Variant {
            label: label.into(),
            manifest: manifest.into(),
            plan,
        });
    };
    let live = "hydo/onboard/examples/redis/hydo-live.yaml";
    let redis = "hydo/onboard/examples/redis/hydo.yaml";
    add("redis-live", live, &|_| {});
    add(
        "compose-voting-loopback",
        "hydo/onboard/examples/compose-voting/hydo-loopback.yaml",
        &|_| {},
    );
    add("rust-kv", "hydo/onboard/examples/rust-kv/hydo.yaml", &|_| {});
    add("node-kv", "hydo/onboard/examples/node-kv/hydo.yaml", &|_| {});
    add(
        "python-kv-mac",
        "hydo/onboard/examples/python-kv/hydo-mac.yaml",
        &|_| {},
    );
    add("redis+clock_after_cut", redis, &|p| {
        p.clock_after_cut = Some(3600)
    });
    add("redis-live+clock_after_cut+start", live, &|p| {
        p.clock_after_cut = Some(86400);
        second_eventually(p, "start", 3, 2);
    });
    add("redis-live+heal+heal", live, &|p| {
        second_eventually(p, "heal", 2, 1);
    });
    add("redis-live+steps0", live, &|p| {
        p.steps = 0;
        second_eventually(p, "start", 4, 1);
    });
    add("rust-kv+clock_after_cut", "hydo/onboard/examples/rust-kv/hydo.yaml", &|p| {
        p.clock_after_cut = Some(1)
    });
    v
}

/// Schedules for the paths of these features that the explorer does not draw.
fn written_here(p: &Plan) -> Vec<String> {
    let n = p.steps;
    let mid = (n / 2).max(1);
    let a = p
        .services
        .first()
        .map(|s| s.name.clone())
        .unwrap_or_default();
    let mut v = vec![
        "disk=none | run".to_string(),
        format!("disk=none | run T={mid}:-3600"),
        format!("disk=none | run T=0:86400 K={a}@{mid}+2"),
        format!("disk=none | run T={n}:-30"),
        format!("disk=none | run T=1:3 T=2:-5"),
        format!("disk=none | run T=1:6 P={a}@1~50:2000"),
        format!("disk=none | run T={}:-2000000000", mid + 1),
        format!("disk=cut | run T=1:7200 C={mid}"),
        format!("disk=cut | run C={n}"),
        format!("disk=cut | run P={a}@1~10:4000 C={mid}"),
        format!("disk=none | run P={a}@{}~0:20000", (n - 2).max(0)),
        format!("disk=none | run K={a}@{}+3", (n - 1).max(0)),
        "disk=none | run B=2a".to_string(),
        format!("disk=none | run B=1 B=deadbeef,500,20 K={a}@{mid}+1"),
        format!("disk=cut | run B=ff,1000,1000 C={mid}"),
        "rb=1 | run T=1:60".to_string(),
    ];
    v.dedup();
    v
}

// ------------------------------------------------------------------ the conversations

fn class(t: &Transcript) -> &'static str {
    match t.ending() {
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: OK") => "ok",
        Some(Ending::Verdict(v)) if v.starts_with("HYDO-ORACLE: VIOLATION") => "violation",
        Some(Ending::Verdict(_)) => "error",
        Some(Ending::PowerCut { .. }) => "cut",
        Some(Ending::AtSnapshot(_)) => "snapshot",
        None => "none",
    }
}

fn stream_of(t: &Transcript) -> Vec<String> {
    t.to_jsonl()
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"t\":\"note\""))
        .map(str::to_string)
        .collect()
}

/// One checked conversation, kept for the mistake test.
#[derive(Clone)]
struct Kept {
    what: String,
    plan: Plan,
    items: Vec<Item>,
    conv: Conv,
    cmd: String,
    ids: (Vec<String>, Vec<String>),
    at: Option<GuestState>,
}

impl Kept {
    fn check(&self, items: &[Item]) -> Result<(), String> {
        check(
            &self.plan,
            items,
            self.conv,
            &self.cmd,
            (&self.ids.0, &self.ids.1),
            self.at.as_ref(),
        )
    }
}

fn boot_until_ok(v: &Variant, golden: &mut Vec<String>) -> Option<Booted> {
    for i in 0..20 {
        let salt = format!("{}#{i}", v.label);
        let mut ctl = hydo_control::ScheduleController::boot(v.plan.clone()).unwrap();
        let c = support::converse(
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
        if let Err(e) = check(&v.plan, &c.items, Conv::Boot, "provision", (&[], &[]), None) {
            panic!("{}: boot {salt}: the controller differs from the shell:\n{e}", v.label);
        }
        golden.push(format!(
            "boot\t{}\t{salt}\t{}\t{}",
            v.label,
            class(&c.fin.transcript),
            c.fin.transcript.hash()
        ));
        if let Ok(b) = boot(&v.plan, &salt) {
            return Some(b);
        }
    }
    None
}

/// What the runs reached, by feature path.
fn paths_of(items: &[Item], conv: &str, cov: &mut BTreeMap<String, usize>) {
    let mut hit = |k: &str| *cov.entry(format!("{conv}:{k}")).or_default() += 1;
    for it in items {
        match it {
            Item::Report { line, .. } => {
                if line.starts_with("HYDO-ASSERT {\"t\":\"eventually\"") {
                    hit(if line.contains("\"ok\":true") {
                        "eventually-ok"
                    } else {
                        "eventually-not-ok"
                    });
                }
                if line.contains("\"id\":\"fault.clock_back\",\"ok\"") {
                    hit("T-back");
                }
                if line.contains("\"id\":\"fault.clock_fwd\",\"ok\"") {
                    hit("T-fwd");
                }
                if line.starts_with("hydo-explore-acked:") && line.contains("clock=") {
                    hit("cut-clock");
                }
                if line.starts_with("HYDO-ORACLE: ERROR clock jump not applied") {
                    hit("T-not-applied");
                }
            }
            Item::Note { line, .. } => {
                for (pat, k) in [
                    (": ok after ", "live-ok"),
                    (": late after ", "live-late"),
                    (": expired after ", "live-expired"),
                    ("date -s failed", "T-set-refused"),
                    ("recovery boot: realtime clock set to", "recovery-clock"),
                    ("BUGGIFY ", "buggify"),
                    ("liveness anchor 'heal'", "anchor-heal"),
                    ("liveness anchor 'start'", "anchor-start"),
                ] {
                    if line.starts_with("hydo-ob: ") && line.contains(pat) {
                        hit(k);
                    }
                }
            }
            Item::Send { by, cmd, .. } if by.starts_with("live:") => {
                if let Command::Run(Run { argv, .. }) = cmd {
                    if argv.get(1).is_some_and(|a| a == "timeout") {
                        hit("live-attempt");
                    }
                }
            }
            Item::Got {
                msg: Msg::Event(hydo_agent_proto::msg::Event::Done(d)),
                ..
            } if d.status == 143 => hit("status-143"),
            Item::Got {
                msg: Msg::Refused(_, op, _, _),
                ..
            } if op == "clock_set" => hit("clock-set-refused"),
            _ => {}
        }
    }
}

#[test]
fn the_feature_controllers_do_what_the_shell_extensions_do() {
    let seeds = if std::env::var_os("HYDO_CONTROL_FULL").is_some() {
        300
    } else {
        40
    };
    let t0 = std::time::Instant::now();
    let mut golden = Vec::new();
    let mut inputs: BTreeSet<String> = BTreeSet::new();
    let mut cov: BTreeMap<String, usize> = BTreeMap::new();
    let mut runs = 0usize;
    for v in variants() {
        inputs.insert(v.manifest.clone());
        let Some(b) = boot_until_ok(&v, &mut golden) else {
            panic!("{}: no boot reached S1", v.label);
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stream = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut texts = identity_schedules(&v.manifest, seeds);
        texts.extend(written_here(&v.plan));
        for text in &texts {
            let s = Schedule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            if !seen.insert(s.guest_text()) {
                continue;
            }
            let r = match run_schedule(&b, &s) {
                Ok(r) => r,
                Err(e) => {
                    let seam = e.seam.map(|x| x.name().to_string()).unwrap_or("other".into());
                    *counts.entry(format!("refused:{seam}")).or_default() += 1;
                    continue;
                }
            };
            let ctx = |what: &str| format!("{}: {text}: {what}", v.label);
            if let Err(e) = check(
                &v.plan,
                &r.run.items,
                Conv::Run,
                &s.guest_text(),
                (&[], &[]),
                Some(&b.s1.2),
            ) {
                panic!(
                    "{}\n{e}\n--- transcript:\n{}",
                    ctx("the run differs from the shell"),
                    r.run.fin.transcript.to_jsonl()
                );
            }
            let mut reference = r.run.fin.transcript.clone();
            reference.header.controller = "replay".into();
            assert_eq!(
                r.replay.to_jsonl(),
                reference.to_jsonl(),
                "{}",
                ctx("the replay of the run's transcript differs from the policy run")
            );
            runs += 1;
            *counts
                .entry(format!("run:{}", class(&r.run.fin.transcript)))
                .or_default() += 1;
            paths_of(&r.run.items, "run", &mut cov);
            stream.push(format!("S\t{text}"));
            stream.extend(stream_of(&r.run.fin.transcript));
            if let Some(rc) = &r.recovery {
                let inputs = r.inputs.clone().unwrap();
                let cmd = support::oracle_cmd(&v.plan, &inputs);
                if let Err(e) = check(
                    &v.plan,
                    &rc.items,
                    Conv::Recovery,
                    &cmd,
                    (&inputs.acked, &inputs.failed),
                    Some(&b.s0.2),
                ) {
                    panic!(
                        "{}\n{e}\n--- transcript:\n{}",
                        ctx("the recovery differs from the shell"),
                        rc.fin.transcript.to_jsonl()
                    );
                }
                // the recovery's transcript typed again on the same guest
                let cad = support::cadence(Some(&s));
                let mut rep = ReplayController::new(&rc.fin.transcript);
                let again = support::converse(
                    Fake::resume(&b.s0.0, &format!("{s} recovery"), cad),
                    &mut rep,
                    Start::Resume(b.s0.1.clone()),
                    rc.fin.transcript.header.clone(),
                )
                .fin
                .transcript;
                let mut reference = rc.fin.transcript.clone();
                reference.header.controller = "replay".into();
                assert_eq!(
                    again.to_jsonl(),
                    reference.to_jsonl(),
                    "{}",
                    ctx("the replay of the recovery's transcript differs")
                );
                if v.plan.clock_after_cut.is_some() {
                    assert!(
                        inputs.clock.is_some(),
                        "{}",
                        ctx("the cut handed no clock over")
                    );
                }
                *counts
                    .entry(format!("recovery:{}", class(&rc.fin.transcript)))
                    .or_default() += 1;
                paths_of(&rc.items, "recovery", &mut cov);
                stream.push("R".into());
                stream.extend(stream_of(&rc.fin.transcript));
            }
            for t in &s.guest {
                *counts.entry(format!("token:{}", t.letter())).or_default() += 1;
            }
        }
        golden.push(format!(
            "counts\t{}\t{}",
            v.label,
            counts
                .iter()
                .map(|(k, n)| format!("{k}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        golden.push(format!(
            "boot-transcript\t{}\t{}",
            v.label,
            b.boot.fin.transcript.hash()
        ));
        golden.extend(support::chunked(&v.label, &stream, 500));
    }
    golden.push(format!(
        "paths\t{}",
        cov.iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    eprintln!(
        "features: {runs} runs in {:.1} s; paths: {cov:?}",
        t0.elapsed().as_secs_f64()
    );
    // every path of the features was reached
    for k in [
        "run:eventually-ok",
        "run:eventually-not-ok",
        "run:live-ok",
        "run:live-late",
        "run:live-expired",
        "run:live-attempt",
        "run:anchor-heal",
        "run:anchor-start",
        "run:status-143",
        "run:T-back",
        "run:T-fwd",
        "run:T-set-refused",
        "run:T-not-applied",
        "run:clock-set-refused",
        "run:cut-clock",
        "run:buggify",
        "recovery:eventually-ok",
        "recovery:eventually-not-ok",
        "recovery:anchor-heal",
        "recovery:anchor-start",
        "recovery:recovery-clock",
        "recovery:clock-set-refused",
    ] {
        assert!(cov.get(k).copied().unwrap_or(0) > 0, "never reached: {k}");
    }
    assert!(runs > 1500, "too few runs: {runs}");
    let name = if seeds == 300 {
        "features-full.txt"
    } else {
        "features-quick.txt"
    };
    let inputs: Vec<String> = inputs.into_iter().collect();
    support::check_golden(
        name,
        &[
            "The M122 feature controllers' transcripts on the identity corpus of the manifests that use them",
            "(hydo/crates/hydo-control/tests/features.rs). Re-record: HYDO_BLESS=1 cargo test -p hydo-control --test features",
        ],
        &inputs,
        &golden,
    );
}

// ------------------------------------------------------------------ deliberate mistakes

/// The conversations the mistakes are made in: the runs and recoveries of a few variants.
fn kept() -> Vec<Kept> {
    let mut out = Vec::new();
    for v in variants() {
        let mut g = Vec::new();
        let b = boot_until_ok(&v, &mut g).unwrap();
        for text in written_here(&v.plan)
            .into_iter()
            .chain(identity_schedules(&v.manifest, 6))
        {
            let s = Schedule::parse(&text).unwrap();
            let Ok(r) = run_schedule(&b, &s) else {
                continue;
            };
            out.push(Kept {
                what: format!("{}: {text}", v.label),
                plan: v.plan.clone(),
                items: r.run.items.clone(),
                conv: Conv::Run,
                cmd: s.guest_text(),
                ids: (Vec::new(), Vec::new()),
                at: Some(b.s1.2.clone()),
            });
            if let (Some(rc), Some(i)) = (&r.recovery, &r.inputs) {
                out.push(Kept {
                    what: format!("{}: {text} (recovery)", v.label),
                    plan: v.plan.clone(),
                    items: rc.items.clone(),
                    conv: Conv::Recovery,
                    cmd: support::oracle_cmd(&v.plan, i),
                    ids: (i.acked.clone(), i.failed.clone()),
                    at: Some(b.s0.2.clone()),
                });
            }
        }
    }
    out
}

type Mistake = fn(&Kept, &[Item]) -> Option<Vec<Item>>;

fn pos(items: &[Item], f: impl Fn(&Item) -> bool) -> Option<usize> {
    items.iter().position(f)
}

fn report_starts(it: &Item, p: &str) -> bool {
    matches!(it, Item::Report { line, .. } if line.starts_with(p))
}

fn replace_report(items: &[Item], i: usize, f: impl Fn(&str) -> String) -> Vec<Item> {
    let mut v = items.to_vec();
    if let Item::Report { line, .. } = &mut v[i] {
        *line = f(line);
    }
    v
}

/// One mistake per feature path; each returns the items with the mistake made, or None when the
/// conversation does not have the place.
fn mistakes() -> Vec<(&'static str, Mistake)> {
    vec![
        ("eventually: a wrong elapsed time in the record", |_, items| {
            let i = pos(items, |it| report_starts(it, "HYDO-ASSERT {\"t\":\"eventually\""))?;
            Some(replace_report(items, i, |l| {
                l.replacen("\"elapsed_s\":\"", "\"elapsed_s\":\"1", 1)
            }))
        }),
        ("eventually: late counted as satisfied", |_, items| {
            let late = items.iter().any(
                |it| matches!(it, Item::Note { line, .. } if line.contains(": late after ")),
            );
            if !late {
                return None;
            }
            let i = pos(items, |it| {
                report_starts(it, "HYDO-ASSERT {\"t\":\"eventually\"")
                    && matches!(it, Item::Report { line, .. } if line.contains("\"ok\":false"))
            })?;
            Some(replace_report(items, i, |l| {
                l.replacen("\"ok\":false", "\"ok\":true", 1)
            }))
        }),
        ("eventually: a monitor with the environment of a later moment", |_, items| {
            let i = pos(items, |it| {
                matches!(it, Item::Send { by, cmd: Command::Run(r), .. }
                    if by.starts_with("live:") && r.env.as_ref().is_some_and(|e| e.iter().any(|x| x.starts_with("HYDO_SVC="))))
            })?;
            let mut v = items.to_vec();
            if let Item::Send {
                cmd: Command::Run(r),
                ..
            } = &mut v[i]
            {
                for e in r.env.as_mut().unwrap() {
                    if e.starts_with("HYDO_SVC=") {
                        e.push_str("-later");
                    }
                }
            }
            Some(v)
        }),
        ("eventually: resolved before its monitor ended", |k, items| {
            // the monitor's last answer comes after the record that needed it
            let rec = pos(items, |it| report_starts(it, "HYDO-ASSERT {\"t\":\"eventually\""))?;
            let Item::Report { line, .. } = &items[rec] else {
                unreachable!()
            };
            let inv = k
                .plan
                .invariants
                .iter()
                .find(|v| line.contains(&format!("\"id\":\"{}\",", v.id)))?;
            let label = format!("live:{}", inv.key);
            let last_send = items[..rec].iter().rposition(
                |it| matches!(it, Item::Send { by, cmd: Command::ClockGet(_), .. } if *by == label),
            )?;
            let Item::Send { id, .. } = &items[last_send] else {
                unreachable!()
            };
            let ans = pos(items, |it| matches!(it, Item::Got { msg: Msg::Reply(re, _), .. } if re == id))?;
            if ans > rec {
                return None;
            }
            let mut v = items.to_vec();
            let a = v.remove(ans);
            v.insert(rec, a);
            Some(v)
        }),
        ("eventually: a monitor that does not read the uptime", |_, items| {
            let i = pos(items, |it| {
                matches!(it, Item::Send { by, cmd: Command::ClockGet(_), .. } if by.starts_with("live:"))
            })?;
            let mut v = items.to_vec();
            v.remove(i);
            Some(v)
        }),
        ("eventually: the recovery's anchors after its services started", |k, items| {
            if k.conv != Conv::Recovery {
                return None;
            }
            let a = pos(items, |it| {
                matches!(it, Item::Note { line, .. } if line.contains("liveness anchor 'heal'"))
            })?;
            let sp = pos(items, |it| matches!(it, Item::Send { cmd: Command::Spawn(_), .. }))?;
            if sp < a {
                return None;
            }
            let mut v = items.to_vec();
            let n = v.remove(a);
            v.insert(sp, n);
            Some(v)
        }),
        ("T=: the clock stepped by delta instead of set to now + delta", |k, items| {
            if k.conv != Conv::Run {
                return None;
            }
            let i = pos(items, |it| {
                matches!(it, Item::Send { by, cmd: Command::ClockSet(ClockSet { sec: Some(_), .. }), .. } if by == "main")
            })?;
            let mut v = items.to_vec();
            if let Item::Send {
                cmd: Command::ClockSet(c),
                ..
            } = &mut v[i]
            {
                *c = ClockSet {
                    sec: None,
                    step: Some(c.sec.unwrap() % 1000),
                };
            }
            Some(v)
        }),
        ("T=: the wrong direction's record", |_, items| {
            let i = pos(items, |it| {
                matches!(it, Item::Report { line, .. } if line.contains("\"id\":\"fault.clock_back\",\"ok\""))
            })?;
            Some(replace_report(items, i, |l| {
                l.replace("fault.clock_back", "fault.clock_fwd")
                    .replace("backwards", "forwards")
            }))
        }),
        ("T=: the 5 s check skipped", |_, items| {
            let i = pos(items, |it| {
                report_starts(it, "HYDO-ORACLE: ERROR clock jump not applied")
            })?;
            let mut v = items.to_vec();
            v.remove(i);
            Some(v)
        }),
        ("B=: the value written without its newline", |_, items| {
            let i = pos(items, |it| {
                matches!(it, Item::Send { cmd: Command::Write(w), .. } if w.path == "/tmp/hydo-buggify")
            })?;
            let mut v = items.to_vec();
            if let Item::Send {
                cmd: Command::Write(w),
                ..
            } = &mut v[i]
            {
                w.data = w.data.trim_end().to_string();
            }
            Some(v)
        }),
        ("B=: written after the fault declarations", |_, items| {
            let i = pos(items, |it| {
                matches!(it, Item::Send { cmd: Command::Write(w), .. } if w.path == "/tmp/hydo-buggify")
            })?;
            let d = pos(items, |it| report_starts(it, "HYDO-ASSERT-DECL {\"t\":\"reachable\",\"id\":\"fault.paused\""))?;
            let mut v = items.to_vec();
            let w = v.remove(i);
            v.insert(d, w);
            Some(v)
        }),
        ("clock_after_cut: the acked line without the clock", |_, items| {
            let i = pos(items, |it| {
                report_starts(it, "hydo-explore-acked:")
                    && matches!(it, Item::Report { line, .. } if line.contains("clock="))
            })?;
            Some(replace_report(items, i, |l| {
                l.split(' ')
                    .filter(|t| !t.starts_with("clock="))
                    .collect::<Vec<_>>()
                    .join(" ")
            }))
        }),
        ("clock_after_cut: the clock read before failed_ids", |k, items| {
            if k.plan.clock_after_cut.is_none() || k.conv != Conv::Run {
                return None;
            }
            let acked = pos(items, |it| report_starts(it, "hydo-explore-acked:"))?;
            let cg = items[..acked].iter().rposition(
                |it| matches!(it, Item::Send { by, cmd: Command::ClockGet(_), .. } if by == "main"),
            )?;
            let rd = items[..cg].iter().rposition(
                |it| matches!(it, Item::Send { by, cmd: Command::Read(_), .. } if by == "main"),
            )?;
            let mut v = items.to_vec();
            let c = v.remove(cg);
            v.insert(rd, c);
            Some(v)
        }),
        ("clock_after_cut: the recovery clock without the time off", |k, items| {
            let after = k.plan.clock_after_cut?;
            if k.conv != Conv::Recovery {
                return None;
            }
            let sets: Vec<usize> = items
                .iter()
                .enumerate()
                .filter(|(_, it)| matches!(it, Item::Send { cmd: Command::ClockSet(_), .. }))
                .map(|(i, _)| i)
                .collect();
            let i = *sets.get(1)?;
            let mut v = items.to_vec();
            if let Item::Send {
                cmd: Command::ClockSet(c),
                ..
            } = &mut v[i]
            {
                c.sec = c.sec.map(|s| s - after);
            }
            Some(v)
        }),
        ("clock_after_cut: the clock written among the recovered ids", |k, items| {
            k.plan.clock_after_cut?;
            if k.conv != Conv::Recovery {
                return None;
            }
            let i = pos(items, |it| {
                matches!(it, Item::Send { cmd: Command::Write(w), .. } if w.path == "/tmp/hydo/acked")
            })?;
            let mut v = items.to_vec();
            let Item::Send { n, by, cmd: Command::Write(w), .. } = v[i].clone() else {
                unreachable!()
            };
            let mut w2 = w.clone();
            w2.data = "clock=1767225600\n".into();
            v.insert(
                i + 1,
                Item::Send {
                    n,
                    id: 999_999,
                    by,
                    cmd: Command::Write(w2),
                },
            );
            Some(v)
        }),
    ]
}

#[test]
fn the_shell_model_catches_mistakes_in_the_features() {
    let ks = kept();
    let mut caught = Vec::new();
    for (what, f) in mistakes() {
        let mut n = 0;
        let mut first = None;
        for k in &ks {
            let Some(bad) = f(k, &k.items) else {
                continue;
            };
            assert!(
                k.check(&k.items).is_ok(),
                "{}: the unchanged conversation must pass",
                k.what
            );
            match k.check(&bad) {
                Ok(()) => panic!("the model did not catch \"{what}\" in {}", k.what),
                Err(e) => {
                    n += 1;
                    first.get_or_insert_with(|| (k.what.clone(), e));
                }
            }
        }
        let (at, e) = first.unwrap_or_else(|| panic!("no conversation has the place for \"{what}\""));
        caught.push(format!(
            "{what}: caught in {n} conversations (first: {at}: {})",
            e.lines().next().unwrap_or("")
        ));
    }
    eprintln!("{}", caught.join("\n"));
}
