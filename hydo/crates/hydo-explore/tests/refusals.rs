//! The explorer never draws what its engine refuses (Milestone 208). `--timing --preempt` on hvx
//! drew tick jitter (`tj=`, `tjmax=500000`) together with a shorter tick period (`pp=65536`),
//! which the module refuses (`HVX_SET_TJITTER`: a delay of a tick period or more); every such
//! run ended `ERROR harness tick jitter (needs the PMU clock)` and counted as a failure. Here
//! every schedule the explorer generates for each engine, with every option that engine takes,
//! is accepted by that engine's rule (`hydo_engine::refusal`, the module's rule for hvx and the
//! hvx-only host tokens for the Mac and KVM engines), and the combination that was refused is
//! still drawn (with its jitter below the shorter period), so the check is not vacuous.

use crate::support;

use hydo_engine::EngineKind;
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::Space;
use hydo_schedule::Schedule;
use support::top;

/// The example spaces checked: the built-in one and two manifests with services and knobs.
const SPACES: [Option<&str>; 3] = [
    None,
    Some("hydo/onboard/examples/redis/hydo.yaml"),
    Some("hydo/onboard/examples/stack/hydo.yaml"),
];

fn ex_for(space: Option<&str>, backend: Backend) -> Ex {
    let mut ex = Ex::default();
    if let Some(m) = space {
        let sp = Space::from_path(&top().join(m)).expect("an example manifest loads");
        sp.apply(&mut ex);
    }
    ex.backend = backend;
    ex.timing = true;
    ex.preempt = true;
    ex.new_faults = true;
    ex.more_faults = backend == Backend::Hvx;
    ex
}

#[test]
fn every_generated_schedule_is_one_its_engine_runs() {
    // the explorer's backend and the engines it drives (the KVM engine is explored as the Mac's)
    let engines = [
        (Backend::Hvx, EngineKind::Hvx),
        (Backend::Hvm, EngineKind::Hvm),
        (Backend::Hvm, EngineKind::Kvm),
    ];
    let mut n = 0;
    let mut tj_with_pp = 0;
    let mut tj_with_pct = 0;
    for space in SPACES {
        for (backend, kind) in engines {
            let ex = ex_for(space, backend);
            for strategy in ["random", "swarm", "process"] {
                for seed in 1..=400u64 {
                    let s = ex.compose_seed(strategy, seed, false);
                    let text = ex.render(&s);
                    let sc = Schedule::parse(&text)
                        .unwrap_or_else(|e| panic!("{space:?} {strategy} {seed}: {text}: {e}"));
                    if let Some(why) = hydo_engine::refusal(kind, &sc) {
                        panic!("{space:?} ({kind:?}, {strategy}, seed {seed}): {text}: {why}");
                    }
                    let has = |k: &str| sc.host_token(k).is_some();
                    tj_with_pp += (has("tj") && has("pp")) as u64;
                    tj_with_pct += (has("tj") && has("pct")) as u64;
                    n += 1;
                }
            }
        }
    }
    assert!(n >= 10_000, "{n} schedules");
    assert!(
        tj_with_pp > 100,
        "tick jitter with pp= drawn {tj_with_pp} times"
    );
    assert!(
        tj_with_pct > 100,
        "tick jitter with pct= drawn {tj_with_pct} times"
    );
}

#[test]
fn the_shorter_period_keeps_the_jitter_below_it() {
    let ex = ex_for(None, Backend::Hvx);
    let mut seen = 0;
    for seed in 1..=400u64 {
        let s = ex.compose_seed("random", seed, false);
        let text = ex.render(&s);
        let sc = Schedule::parse(&text).expect("parses");
        let num = |k: &str| match sc.host_token(k) {
            Some(hydo_schedule::HostToken::Pp(v) | hydo_schedule::HostToken::Tjmax(v)) => Some(*v),
            _ => None,
        };
        if let (Some(pp), Some(max)) = (num("pp"), num("tjmax")) {
            assert_eq!(max, pp / 2, "{text}");
            seen += 1;
        } else if let Some(max) = num("tjmax") {
            // without pp= the jitter is drawn as before (default explorations unchanged)
            assert_eq!(max, hydo_explore::gen::TJ_MAX as u64, "{text}");
        }
    }
    assert!(seen > 10, "{seen}");
}

#[test]
fn the_engines_refuse_what_they_cannot_arm() {
    let p = |s: &str| Schedule::parse(s).expect("parses");
    // the Qdrant exploration's schedules on hvx (hydo/onboard/examples/qdrant-flaky-10200)
    let both = p("disk=none dseed=1 tj=5 tjpm=10 tjmax=500000 pp=65536 | run");
    let why = hydo_engine::refusal(EngineKind::Hvx, &both).expect("hvx refuses it");
    assert!(
        why.contains("tjmax=500000") && why.ends_with("(on hvx)"),
        "{why}"
    );
    // the Mac and KVM engines cap their jitter, but have no pp=
    for k in [EngineKind::Hvm, EngineKind::Kvm] {
        let why = hydo_engine::refusal(k, &both).expect("pp= is hvx's");
        assert!(why.contains("pp=65536"), "{why}");
        let pct = p("disk=none tj=5 tjmax=1000 pct=ec699251106e8e0c pctn=40 pctk=3000 | run");
        assert!(hydo_engine::refusal(k, &pct).is_none());
    }
    assert!(hydo_engine::refusal(EngineKind::Hvx, &p("disk=none | run")).is_none());
    // a refused verdict is never a finding
    assert!(hydo_engine::is_refused(
        "ERROR refused: tick jitter tjmax=500000 (on hvx)"
    ));
    assert!(hydo_engine::is_refused(
        "UNSUPPORTED on kvm: host token pp=1"
    ));
    assert!(!hydo_engine::is_refused(
        "ERROR harness tick jitter (needs the PMU clock)"
    ));
}
