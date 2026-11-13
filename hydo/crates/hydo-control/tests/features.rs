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
//! Every run's and every recovery's transcript is typed again by the ReplayController and must
//! equal the policy's, the recovery's inputs must be the ones the run's transcript hands over,
//! every feature path must be reached, and the transcripts are recorded as a golden
//! (`tests/golden/features-quick.txt`). Until Milestone 145 every run and recovery boot was also
//! checked against a shell model of the extensions, and a second test showed that the model
//! caught deliberate mistakes; both went with the shell init's images (`tests/support/mod.rs`).

use crate::support;

use hydo_agent_proto::msg::{Command, Run};
use hydo_control::plan::{PlanInvariant, PlanLive};
use hydo_control::{Ending, Plan, ReplayController, Start, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use std::collections::{BTreeMap, BTreeSet};
use support::fake::{Fake, Msg};
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
    add(
        "rust-kv",
        "hydo/onboard/examples/rust-kv/hydo.yaml",
        &|_| {},
    );
    add(
        "node-kv",
        "hydo/onboard/examples/node-kv/hydo.yaml",
        &|_| {},
    );
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
    add(
        "rust-kv+clock_after_cut",
        "hydo/onboard/examples/rust-kv/hydo.yaml",
        &|p| p.clock_after_cut = Some(1),
    );
    // liveness, the clock and the fake world together (M125 merged): at a cut the acked line is
    // the ids, then clock=, then the world's records
    add(
        "shop+clock_after_cut",
        "hydo/onboard/examples/shop/hydo.yaml",
        &|p| p.clock_after_cut = Some(600),
    );
    // with service networks and network faults too (M124 merged)
    add(
        "redis-bridge",
        "hydo/onboard/examples/redis-bridge/hydo.yaml",
        &|_| {},
    );
    add(
        "redis-bridge+clock_after_cut",
        "hydo/onboard/examples/redis-bridge/hydo.yaml",
        &|p| p.clock_after_cut = Some(7200),
    );
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
        // clock steps to a time (M225): a calendar boundary and a minute of every hour
        format!("disk=none | run T={mid}:@month+10m"),
        "disk=none | run T=0:@:10-2s".to_string(),
        format!("disk=cut | run T=1:@day C={mid}"),
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
                    // with a fake world: the ids, then clock=, then the records (all after it)
                    let words: Vec<&str> = line.split_whitespace().collect();
                    let c = words.iter().position(|w| w.starts_with("clock=")).unwrap();
                    if words.iter().any(|w| w.starts_with("w:")) {
                        assert!(
                            words.iter().position(|w| w.starts_with("w:")).unwrap() > c,
                            "a world record before the clock: {line}"
                        );
                        hit("cut-clock-world");
                    }
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
                    ("s to @", "T-to"),
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
            Item::Send {
                by,
                cmd: Command::Run(Run { argv, .. }),
                ..
            } if by.starts_with("live:") && argv.get(1).is_some_and(|a| a == "timeout") => {
                hit("live-attempt")
            }
            Item::Got {
                msg: Msg::Event(hydo_agent_proto::msg::Event::Done(d)),
                ..
            } if d.status == 143 => hit("status-143"),
            Item::Got {
                msg: Msg::Refused(_, op, _, _, _),
                ..
            } if op == "clock_set" => hit("clock-set-refused"),
            _ => {}
        }
    }
}

#[test]
fn the_feature_controllers_recorded_transcripts() {
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
                    let seam = e
                        .seam
                        .map(|x| x.name().to_string())
                        .unwrap_or("other".into());
                    *counts.entry(format!("refused:{seam}")).or_default() += 1;
                    continue;
                }
            };
            let ctx = |what: &str| format!("{}: {text}: {what}", v.label);
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
                // what drive (C1) hands over from the run's transcript is the controller's
                assert_eq!(
                    hydo_control::RecoveryInputs::from_transcript(&r.run.fin.transcript).as_ref(),
                    Some(&inputs),
                    "{}",
                    ctx("the transcript's hand-over differs from the controller's")
                );
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
        "run:T-to",
        "run:T-set-refused",
        "run:T-not-applied",
        "run:clock-set-refused",
        "run:cut-clock",
        "run:cut-clock-world",
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
