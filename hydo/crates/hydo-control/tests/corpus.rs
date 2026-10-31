//! The core controller on the explorer's identity corpus (docs/HISTORY.md M120).
//!
//! For every example manifest the explorer's identity test reads (hydo-explore
//! `tests/identity.rs`), the manifest is read into the controller's plan; a manifest that needs an
//! init extension is refused with its seam. For each core manifest:
//!
//! 1. it boots on the scripted agent and engine (`tests/support/fake.rs`) to S1, and the boot is
//!    checked against the shell model (`tests/support/model.rs`, hydo-init.sh written as blocking
//!    code); a boot the scripted world makes fail is checked too, and the next salt is tried;
//! 2. the schedules are those of the identity corpus: every schedule the explorer composes for
//!    the manifest (random, swarm, process, and swarm-pos for 15+ steps; the option streams
//!    switched per seed exactly as identity.rs does; hvx, and hvm with and without a data disk
//!    and tc), plus a few written here for the paths the explorer never draws for a manifest
//!    (X=, an unknown service, a repeated letter, a knob at its default, an unknown knob), kept
//!    when the core controller drives them (the others name their seam), one per guest part;
//! 3. each runs from S1 (and, after a power cut, its recovery boot from S0); both conversations
//!    are checked against the shell model, the controller's thread by thread;
//! 4. the run's transcript is typed again by the ReplayController on the same scripted guest, and
//!    the replay's transcript must equal the policy run's;
//! 5. the transcripts are recorded as goldens (`tests/golden/corpus-*.txt`: chunked SHA-256 of
//!    every command, report line and end, per manifest, with counts), and a few whole transcripts
//!    are kept readable in `tests/golden/examples/`.
//!
//! `HYDO_BLESS=1` re-records the goldens; `HYDO_CONTROL_FULL=1` uses the identity test's full
//! 300 seeds instead of 40.

mod support;

use hydo_control::{Ending, Plan, Transcript};
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use std::collections::{BTreeMap, BTreeSet};
use support::model::{check, Conv};
use support::{boot, manifests, plan_of, run_schedule, top, Booted};

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

/// identity.rs `flags_for`: all off, all on, then a hash of the seed.
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

/// The identity corpus's schedule strings for one manifest, in its order.
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

/// Schedules for the paths the explorer does not draw for a manifest.
fn written_here(p: &Plan) -> Vec<String> {
    let none = "nosuch".to_string();
    let a = p.services.first().map(|s| &s.name).unwrap_or(&none);
    let b = p.services.last().map(|s| &s.name).unwrap_or(&none);
    let n = p.steps;
    let mid = (n / 2).max(1);
    let mut v = vec![
        "disk=none | run".to_string(),
        format!("disk=none | run X={a}@{mid}~250"),
        format!("disk=none | run X={b}@1~40 K={a}@{mid}+2"),
        format!("disk=none | run K={a}@1+1 K={b}@{mid}+0"),
        format!("disk=none | run K=nosuch@2+2 P=nosuch@1+1 X=nosuch@1~10"),
        format!("disk=none | run P={a}@{mid}~120:300"),
        format!("disk=cut | run P={a}@1~10:4000 C={mid}"),
        format!("disk=cut | run X={a}@1~30 C={}", mid + 1),
        format!("disk=none | run P={b}@0+{n}"),
        format!("disk=none | run K={b}@{n}+3"),
        format!("disk=none | run Q=4 Z=1"),
        format!("disk=cut | run C=0"),
        format!("disk=cut | run C={n}"),
        "rb=1 | run".to_string(),
    ];
    if let Some(k) = p.knobs.first() {
        v.push(format!("disk=none | run {}={}", k.token, k.default));
        v.push(format!("disk=cut | run {}=zz C={mid}", k.token));
    }
    v
}

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
    // the hashed stream: the whole transcript except the notes (as its hash) and the header
    t.to_jsonl()
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"t\":\"note\""))
        .map(str::to_string)
        .collect()
}

fn boot_until_ok(plan: &Plan, path: &str, report: &mut Vec<String>) -> Option<Booted> {
    for i in 0..20 {
        let salt = format!("{path}#{i}");
        let b = boot(plan, &salt).ok();
        // every boot, failed or not, is checked against the shell model
        let mut ctl = hydo_control::ScheduleController::boot(plan.clone()).unwrap();
        let c = support::converse(
            support::fake::Fake::boot(&salt, 16),
            &mut ctl,
            hydo_control::Start::Boot,
            hydo_control::Header {
                controller: String::new(),
                conversation: "boot".into(),
                schedule: None,
                cadence: "default".into(),
                start: "boot".into(),
            },
        );
        if let Err(e) = check(plan, &c.items, Conv::Boot, "provision", (&[], &[]), None) {
            panic!("{path}: boot {salt}: the controller differs from the shell:\n{e}");
        }
        report.push(format!(
            "boot\t{path}\t{salt}\t{}\t{}",
            class(&c.fin.transcript),
            c.fin.transcript.hash()
        ));
        if let Some(b) = b {
            return Some(b);
        }
    }
    None
}

#[test]
fn the_core_controller_does_what_the_shell_init_does() {
    let seeds = if std::env::var_os("HYDO_CONTROL_FULL").is_some() {
        300
    } else {
        40
    };
    let t0 = std::time::Instant::now();
    let mut golden = Vec::new();
    let mut inputs = Vec::new();
    let mut total: BTreeMap<&str, usize> = BTreeMap::new();
    let mut readable: Vec<(String, String)> = Vec::new();
    let mut seams: BTreeMap<String, usize> = BTreeMap::new();
    for m in manifests() {
        inputs.push(m.clone());
        let plan = match plan_of(&m) {
            Ok(p) => p,
            Err(e) => {
                golden.push(format!("manifest\t{m}\tnot core: {e}"));
                continue;
            }
        };
        golden.push(format!("manifest\t{m}\tcore"));
        let Some(b) = boot_until_ok(&plan, &m, &mut golden) else {
            golden.push(format!("noboot\t{m}"));
            continue;
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stream = Vec::new();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut texts = identity_schedules(&m, seeds);
        let n_identity = texts.len();
        texts.extend(written_here(&plan));
        for (k, text) in texts.iter().enumerate() {
            let s = Schedule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            if !seen.insert(s.guest_text()) {
                continue;
            }
            let r = match run_schedule(&b, &s) {
                Ok(r) => r,
                Err(e) => {
                    let name = e
                        .seam
                        .map(|x| x.name().to_string())
                        .unwrap_or("other".into());
                    *seams.entry(name).or_default() += 1;
                    *counts.entry("refused".into()).or_default() += 1;
                    continue;
                }
            };
            let from = if k < n_identity {
                "identity"
            } else {
                "written"
            };
            *counts.entry(from.into()).or_default() += 1;
            let ctx = |what: &str| format!("{m}: {text}: {what}");
            if let Err(e) = check(
                &plan,
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
                ctx("the replay of the transcript differs from the policy run")
            );
            let cl = class(&r.run.fin.transcript);
            *counts.entry(format!("run:{cl}")).or_default() += 1;
            *total.entry("runs").or_default() += 1;
            stream.push(format!("S\t{text}"));
            stream.extend(stream_of(&r.run.fin.transcript));
            if let Some(rc) = &r.recovery {
                let Some(Ending::PowerCut { acked, failed }) = r.run.fin.ending.clone() else {
                    unreachable!()
                };
                let cmd = support::oracle_cmd(&plan, r.inputs.as_ref().unwrap());
                if let Err(e) = check(
                    &plan,
                    &rc.items,
                    Conv::Recovery,
                    &cmd,
                    (&acked, &failed),
                    Some(&b.s0.2),
                ) {
                    panic!(
                        "{}\n{e}\n--- transcript:\n{}",
                        ctx("the recovery differs from the shell"),
                        rc.fin.transcript.to_jsonl()
                    );
                }
                *counts
                    .entry(format!("recovery:{}", class(&rc.fin.transcript)))
                    .or_default() += 1;
                stream.push("R".into());
                stream.extend(stream_of(&rc.fin.transcript));
            }
            for t in &s.guest {
                *counts.entry(format!("token:{}", t.letter())).or_default() += 1;
            }
            if readable.len() < 6 && m.ends_with("examples/redis/hydo.yaml") && from == "written" {
                let mut body = r.run.fin.transcript.to_jsonl();
                if let Some(rc) = &r.recovery {
                    body.push_str(&rc.fin.transcript.to_jsonl());
                }
                readable.push((text.clone(), body));
            }
        }
        golden.push(format!(
            "counts\t{m}\t{}",
            counts
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        golden.push(format!(
            "boot-transcript\t{m}\t{}",
            b.boot.fin.transcript.hash()
        ));
        golden.extend(support::chunked(&m, &stream, 500));
    }
    golden.push(format!(
        "refused\t{}",
        seams
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    eprintln!(
        "corpus: {} runs over the core manifests in {:.1} s",
        total.get("runs").copied().unwrap_or(0),
        t0.elapsed().as_secs_f64()
    );
    assert!(
        total.get("runs").copied().unwrap_or(0) > 500,
        "too few runs: {total:?}"
    );
    let name = if seeds == 300 {
        "corpus-full.txt"
    } else {
        "corpus-quick.txt"
    };
    support::check_golden(
        name,
        &[
            "The core controller's transcripts on the identity corpus of the core manifests",
            "(hydo/crates/hydo-control/tests/corpus.rs). Re-record: HYDO_BLESS=1 cargo test -p hydo-control --test corpus",
        ],
        &inputs,
        &golden,
    );
    // a few whole transcripts, readable
    let dir = support::golden_dir().join("examples");
    let mut index = String::new();
    for (i, (text, body)) in readable.iter().enumerate() {
        let f = format!("redis-{i}.jsonl");
        index.push_str(&format!("{f}\t{text}\n"));
        let p = dir.join(&f);
        if support::bless() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&p, body).unwrap();
        } else {
            let want =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            assert_eq!(
                &want,
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
