//! `--reactive fault|heal|both` (M139): reactions (`A=`) drawn on top of every strategy's
//! schedules from their own stream.
//!
//! - Every other field of a seed's schedule stays as it was: a schedule with reactions is the
//!   schedule without them plus `A=` tokens, except that a healed pause lasts to the workload's
//!   end and a reaction's power cut makes the disk `cut` (both checked field by field).
//! - The rules: no reaction's power cut with `cf=`; a kill or stop never on the `K=` or `P=`
//!   service, and with `cf=` only on the order service; a heal resumes the paused service,
//!   armed at the pause's start.
//! - Every schedule is canonical for hydo-schedule (the controller's parser) and round-trips
//!   through the explorer's own parse/render; so does every shrink candidate.
//! - The stream is pinned: `tests/golden/reactive.txt` has the count, the SHA-256 and the first
//!   line of each configuration's schedules (`HYDO_BLESS=1` rewrites it).

mod support;

use hydo_explore::gen::Ex;
use hydo_explore::sched::{Pause, S};
use hydo_schedule::Schedule;

fn ex(fault: bool, heal: bool, more: bool) -> Ex {
    Ex {
        react_fault: fault,
        react_heal: heal,
        more_faults: more,
        cf_range: more.then_some((1, 309)),
        ..Ex::default()
    }
}

fn base_of(e: &Ex, strategy: &str, seed: u64) -> S {
    let b = Ex {
        react_fault: false,
        react_heal: false,
        ..e.clone()
    };
    b.compose_seed(strategy, seed, false)
}

#[test]
fn reactions_only_add_tokens_and_follow_the_rules() {
    let mut with_fault = 0;
    let mut with_heal = 0;
    let mut pauses = 0;
    let mut n = 0;
    let mut kinds = std::collections::BTreeMap::new();
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for (fault, heal) in [(true, false), (false, true), (true, true)] {
            for more in [false, true] {
                let e = ex(fault, heal, more);
                for seed in 1..=1500u64 {
                    n += 1;
                    let base = base_of(&e, st, seed);
                    let s = e.compose_seed(st, seed, false);
                    // the rest of the schedule is the strategy's
                    let mut rest = s.clone();
                    rest.a.clear();
                    let healed = s.a.iter().find(|a| a.kind() == "cont");
                    let cut = s.a.iter().any(|a| a.kind() == "cut");
                    if healed.is_some() {
                        rest.p = base.p.clone();
                    }
                    if cut {
                        assert!(base.disk == "none" || base.disk == "cut", "{st} {seed}");
                        assert_eq!(s.disk, "cut");
                        rest.disk = base.disk.clone();
                    }
                    assert_eq!(
                        rest, base,
                        "{st} {seed} fault={fault} heal={heal} more={more}"
                    );
                    // the rules
                    if matches!(base.p, Some(Pause::Plus(..))) && heal {
                        pauses += 1;
                    }
                    for a in &s.a {
                        *kinds.entry(a.kind().to_string()).or_insert(0) += 1;
                        match a.kind() {
                            "cont" => {
                                assert!(heal);
                                let Some(Pause::Plus(svc, at, k)) = &s.p else {
                                    panic!("{st} {seed}: a heal without a counted pause")
                                };
                                assert_eq!(a.svc(), Some(svc.as_str()));
                                assert_eq!(a.at, *at);
                                assert_eq!(at + k, e.norders.max(at + 1));
                                assert!(e.heal_events.contains(&a.event));
                                if svc == "front" {
                                    assert!(a.event.starts_with("client."));
                                }
                                with_heal += 1;
                            }
                            k => {
                                assert!(fault, "{st} {seed}");
                                assert!(e.react_events.contains(&a.event));
                                if k == "cut" {
                                    assert!(s.cf.is_none(), "{st} {seed}");
                                } else {
                                    let v = a.svc().unwrap();
                                    assert!(s.k.as_ref().is_none_or(|x| x.0 != v));
                                    assert!(s.p.as_ref().is_none_or(|p| p.svc() != v));
                                    if s.cf.is_some() {
                                        assert_eq!(v, "front");
                                    }
                                }
                                with_fault += 1;
                            }
                        }
                    }
                    // canonical for the controller, and the explorer's own round trip
                    let text = e.render(&s);
                    let p = Schedule::parse(&text).unwrap_or_else(|x| panic!("{text}: {x}"));
                    assert_eq!(p.to_string(), text);
                    assert_eq!(e.render(&e.parse(&text).unwrap()), text);
                    if !s.a.is_empty() && seed % 25 == 0 {
                        for c in e.candidates(&s, None) {
                            let t = e.render(&c);
                            assert!(Schedule::is_canonical(&t), "{t}");
                        }
                        assert!(e
                            .candidates(&s, None)
                            .iter()
                            .any(|c| c.a.len() + 1 == s.a.len()));
                    }
                }
            }
        }
    }
    eprintln!("{n} schedules, {with_fault} fault reactions, {with_heal} heals of {pauses} counted pauses, kinds {kinds:?}");
    // the draw rates: 60% of the schedules get a fault reaction (fewer where no action is
    // allowed), 70% of the counted pauses a heal
    assert!(with_fault * 100 > (n * 2 / 3) * 50, "{with_fault}");
    assert!(
        with_heal * 100 > pauses * 60 && with_heal * 100 < pauses * 80,
        "{with_heal}/{pauses}"
    );
    for k in ["cut", "kill", "stop", "cont"] {
        assert!(kinds.get(k).copied().unwrap_or(0) > 100, "{k}: {kinds:?}");
    }
}

#[test]
fn the_reactive_stream_is_pinned() {
    let mut lines = Vec::new();
    for st in ["random", "swarm", "swarm-pos", "process"] {
        for (mode, fault, heal) in [
            ("fault", true, false),
            ("heal", false, true),
            ("both", true, true),
        ] {
            for more in [false, true] {
                let e = ex(fault, heal, more);
                let all: Vec<String> = (1..=3)
                    .flat_map(|r| (1..=60).map(move |i| r * 1000 + i))
                    .map(|sd| e.render(&e.compose_seed(st, sd as u64, false)))
                    .collect();
                let blob = all.join("\n") + "\n";
                lines.push(format!(
                    "{st} {mode} mf={}|{}|{}|{}",
                    more as u8,
                    all.len(),
                    support::sha256_hex(blob.as_bytes()),
                    all[0]
                ));
            }
        }
    }
    let path = support::golden_dir().join("reactive.txt");
    let got = lines.join("\n") + "\n";
    if support::bless() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want =
        std::fs::read_to_string(&path).expect("tests/golden/reactive.txt (HYDO_BLESS=1 writes it)");
    assert_eq!(got, want);
}
