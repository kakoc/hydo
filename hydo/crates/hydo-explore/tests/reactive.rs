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

use crate::support;

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

/// Whether `--kill-now` takes the schedule's fault reaction (Milestone 309), and the service it
/// kills: the draw of [`hydo_explore::gen::NOW_SALT`]'s stream, as `add_reactive` makes it.
fn now_taken(e: &Ex, seed: u64, has_cf: bool) -> Option<String> {
    use hydo_explore::gen::{NOW_SALT, NOW_TAKE_PCT};
    let mut n = hydo_explore::rng::SplitMix::new(seed ^ NOW_SALT);
    if n.below(100) >= NOW_TAKE_PCT {
        return None;
    }
    let r = n.next();
    let all: Vec<&String> = e
        .pause_svcs
        .iter()
        .filter(|v| !has_cf || *v == "front")
        .collect();
    (!all.is_empty()).then(|| all[(r % all.len() as u64) as usize].clone())
}

/// Milestones 305 and 309: `--kill-now` changes only the drawn kills. A schedule with it is the
/// schedule without it, with two kinds of change:
///
/// - A schedule whose fault reaction `--kill-now` does not take: each `kill-<svc>` reaction is
///   `kill-now-<svc>` with a delay from `REACT_NOW_DELAYS`; every other reaction, and every other
///   field, is the same.
/// - A schedule whose fault reaction it takes: the fault reaction, with the same event and step,
///   is `kill-now-<v>` of the drawn service `v`. A `K=` or `P=` on `v` is left out, and with the
///   pause its heal; every other field is the same.
///
/// Its schedules are canonical, and no shrink candidate turns the kill at the event's moment
/// into a plain kill.
#[test]
fn kill_now_changes_only_the_drawn_kills() {
    use hydo_explore::gen::REACT_NOW_DELAYS;
    let mut now = 0;
    let mut taken = 0;
    for st in ["random", "process"] {
        for more in [false, true] {
            let e = ex(true, true, more);
            let k = Ex {
                react_kill_now: true,
                ..e.clone()
            };
            for seed in 1..=800u64 {
                let a = e.compose_seed(st, seed, false);
                let b = k.compose_seed(st, seed, false);
                let fault_a = a.a.iter().find(|r| r.kind() != "cont");
                let fault_b = b.a.iter().find(|r| r.kind() != "cont");
                match (fault_b, now_taken(&k, seed, a.cf.is_some())) {
                    (Some(y), Some(v)) => {
                        assert_eq!(y.action, format!("kill-now-{v}"), "{st} {seed}");
                        assert!(REACT_NOW_DELAYS.contains(&y.delay), "{st} {seed}");
                        if let Some(x) = fault_a {
                            assert_eq!((&y.event, y.at), (&x.event, x.at), "{st} {seed}");
                        }
                        let mut want = a.clone();
                        if want.k.as_ref().is_some_and(|x| x.0 == v) {
                            want.k = None;
                        }
                        if want.p.as_ref().is_some_and(|p| p.svc() == v) {
                            want.p = None;
                            want.a.retain(|r| r.action != format!("cont-{v}"));
                        }
                        assert!(b.k.as_ref().is_none_or(|x| x.0 != v), "{st} {seed}");
                        assert!(b.p.as_ref().is_none_or(|p| p.svc() != v), "{st} {seed}");
                        want.a.retain(|r| r.kind() == "cont");
                        let mut got = b.clone();
                        got.a.retain(|r| r.kind() == "cont");
                        // a reaction's power cut made the disk `cut`; a taken one does not
                        if fault_a.is_some_and(|x| x.action == "cut") {
                            want.disk = got.disk.clone();
                        }
                        assert_eq!(got, want, "{st} {seed}");
                        taken += 1;
                    }
                    _ => {
                        assert_eq!(a.a.len(), b.a.len(), "{st} {seed}");
                        for (x, y) in a.a.iter().zip(&b.a) {
                            if x.action.starts_with("kill-") {
                                assert_eq!(y.action, format!("kill-now-{}", x.svc().unwrap()));
                                assert_eq!(y.svc(), x.svc());
                                assert!(REACT_NOW_DELAYS.contains(&y.delay), "{st} {seed}");
                                assert_eq!((&y.event, y.at), (&x.event, x.at));
                                now += 1;
                            } else {
                                assert_eq!(x, y, "{st} {seed}");
                            }
                        }
                        let mut ra = a.clone();
                        let mut rb = b.clone();
                        ra.a.clear();
                        rb.a.clear();
                        assert_eq!(ra, rb, "{st} {seed}");
                    }
                }
                assert!(
                    b.a.iter()
                        .filter(|r| r.action.starts_with("kill-now-"))
                        .count()
                        <= 1
                );
                let text = k.render(&b);
                assert!(Schedule::is_canonical(&text), "{text}");
                if b.a.iter().any(|r| r.action.starts_with("kill-now-")) && seed % 10 == 0 {
                    for c in k.candidates(&b, None) {
                        let t = k.render(&c);
                        assert!(Schedule::is_canonical(&t), "{t}");
                        assert!(
                            !c.a.iter().any(|r| r.action.starts_with("kill-")
                                && !r.action.starts_with("kill-now-")),
                            "{t}: a shrink turned the kill into a plain kill"
                        );
                    }
                }
            }
        }
    }
    assert!(now > 100, "{now} kills at the event's moment");
    assert!(taken > 300, "{taken} reactions taken");
}

/// Milestone 309: on a manifest with one service, `--kill-now` still draws kills at the event's
/// moment. Without the take, the process strategy's `K=` and `P=` on the only service left a
/// reaction nothing to kill, so 24 seeds of the Meilisearch example drew none (Milestone 305).
/// Now about 30% of the schedules carry one (60% have a fault reaction, half of them taken),
/// and none of them also has a `K=` or `P=` on the service.
#[test]
fn kill_now_reaches_the_only_service() {
    let one = vec!["meilisearch".to_string()];
    let e = Ex {
        react_fault: true,
        react_kill_now: true,
        svcs: one.clone(),
        net_svcs: one.clone(),
        pause_svcs: one,
        react_events: vec!["meili.swap_acked".into(), "meili.add_acked".into()],
        ..Ex::default()
    };
    let old = Ex {
        react_kill_now: false,
        ..e.clone()
    };
    for st in ["random", "swarm", "process"] {
        let n = 1000u64;
        let mut with = 0;
        let mut before = 0;
        for seed in 1..=n {
            let s = e.compose_seed(st, seed, false);
            if s.a.iter().any(|r| r.action == "kill-now-meilisearch") {
                with += 1;
                assert!(
                    s.k.as_ref().is_none_or(|k| k.0 != "meilisearch"),
                    "{st} {seed}"
                );
                assert!(
                    s.p.as_ref().is_none_or(|p| p.svc() != "meilisearch"),
                    "{st} {seed}"
                );
            }
            let o = old.compose_seed(st, seed, false);
            if o.a.iter().any(|r| r.kind() == "kill") {
                before += 1;
            }
        }
        eprintln!("{st}: {with} of {n} with a kill at the event's moment, {before} kills before");
        assert!(with * 100 >= n * 25, "{st}: {with} of {n}");
        assert!(with > before, "{st}: {with} against {before}");
    }
}
