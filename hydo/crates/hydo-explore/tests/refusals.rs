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

/// Milestone 345: an option whose draws the engine refuses, or that has nothing to act on in the
/// image, is refused before any VM, one line each with the option, the reason and what to do;
/// the options each engine takes pass.
#[test]
fn options_the_engine_or_the_image_cannot_use_are_refused() {
    use hydo_explore::refuse::refusals;
    let epoch = hydo_image::EPOCH;
    // tick jitter on the KVM engine with several vCPUs (its turns are exits)
    let mut kvm = ex_for(Some("hydo/onboard/examples/redis/hydo.yaml"), Backend::Hvm);
    kvm.hvm_disk = true;
    kvm.preempt = false;
    kvm.new_faults = false;
    let lines = refusals(&kvm, "random", EngineKind::Kvm, 2, epoch);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with(
            "hydo explore: --timing draws schedules that the kvm engine refuses (host token tj="
        ) && lines[0].ends_with("explore without it, or with one vCPU"),
        "{}",
        lines[0]
    );
    assert!(refusals(&kvm, "random", EngineKind::Kvm, 1, epoch).is_empty());
    // storage faults on a manifest without storage:, and on one with it
    let mut redis = ex_for(Some("hydo/onboard/examples/redis/hydo.yaml"), Backend::Hvx);
    redis.timing = false;
    redis.preempt = false;
    redis.more_faults = false;
    redis.storage_faults = true;
    let lines = refusals(&redis, "process", EngineKind::Hvx, 1, epoch);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("hydo explore: --storage-faults: it damages the files"),
        "{}",
        lines[0]
    );
    let mut meili = ex_for(
        Some("hydo/onboard/examples/meilisearch/hydo/hydo.yaml"),
        Backend::Hvx,
    );
    meili.timing = false;
    meili.preempt = false;
    meili.more_faults = false;
    meili.storage_faults = true;
    assert!(refusals(&meili, "process", EngineKind::Hvx, 1, epoch).is_empty());
    // the block faults on the Mac engine need a data disk
    let mut mac = meili.clone();
    mac.backend = Backend::Hvm;
    mac.storage_faults = false;
    mac.block_faults = true;
    mac.hvm_disk = false;
    let lines = refusals(&mac, "process", EngineKind::Hvm, 1, epoch);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("--block-faults: it damages the data disk's blocks"));
    mac.hvm_disk = true;
    assert!(refusals(&mac, "process", EngineKind::Hvm, 1, epoch).is_empty());
    // every option the engines take, as the test above draws them, passes
    for space in SPACES {
        for (backend, kind) in [
            (Backend::Hvx, EngineKind::Hvx),
            (Backend::Hvm, EngineKind::Hvm),
            (Backend::Hvm, EngineKind::Kvm),
        ] {
            let ex = ex_for(space, backend);
            assert!(
                refusals(&ex, "random", kind, 1, epoch).is_empty(),
                "{space:?} {kind:?}"
            );
        }
    }
}

/// The examples' own command lines with the options they give (their hydo.yaml headers and
/// RESULTS.md pages), as (manifest, strategy, options): none draws nothing.
const EXAMPLE_LINES: &[(&str, &str, &[&str])] = &[
    (
        "hydo/onboard/examples/hatchet/hydo/hydo.yaml",
        "process",
        &["new", "more", "cluster"],
    ),
    (
        "hydo/onboard/examples/clickhouse/hydo/hydo.yaml",
        "process",
        &["new", "cluster"],
    ),
    (
        "hydo/onboard/examples/tikv/hydo/hydo.yaml",
        "process",
        &["new", "cluster", "storage"],
    ),
    (
        "hydo/onboard/examples/victoriametrics/hydo/hydo.yaml",
        "process",
        &["new", "storage", "cluster"],
    ),
    (
        "hydo/onboard/examples/dragonfly/hydo/hydo.yaml",
        "process",
        &["new", "cluster"],
    ),
    (
        "hydo/onboard/examples/river/hydo/hydo.yaml",
        "process",
        &["new", "more", "cluster"],
    ),
    (
        "hydo/onboard/examples/valkey/hydo/hydo.yaml",
        "process",
        &["new", "cluster"],
    ),
    (
        "hydo/onboard/examples/redpanda/hydo/hydo.yaml",
        "process",
        &["more", "new", "storage", "cluster"],
    ),
    (
        "hydo/onboard/examples/greptimedb/hydo/hydo.yaml",
        "process",
        &["new", "cluster", "storage", "protocol"],
    ),
    (
        "hydo/onboard/examples/redis-bridge/hydo.yaml",
        "process",
        &["new", "protocol"],
    ),
    (
        "hydo/onboard/examples/prometheus/hydo/hydo.yaml",
        "process",
        &["new", "more"],
    ),
    (
        "hydo/onboard/examples/etcd-upgrade/hydo.yaml",
        "random",
        &["upgrades"],
    ),
    (
        "hydo/onboard/examples/web-shop/hydo.yaml",
        "random",
        &["protocol"],
    ),
    ("hydo/onboard/examples/redis/hydo.yaml", "swarm", &["new"]),
];

/// Milestone 348: an option whose draws the explorer's own rendering drops entirely is refused,
/// one line each; `--new-faults` with `--strategy process` only warns, because that strategy
/// draws its faults itself and the examples' command lines give the pair.
#[test]
fn an_option_that_draws_nothing_is_refused() {
    use hydo_explore::refuse::nothing_drawn;
    let plain = |m: &str| {
        let mut ex = Ex::default();
        let sp = Space::from_path(&top().join(m)).expect("an example manifest loads");
        sp.apply(&mut ex);
        ex.backend = Backend::Hvx;
        ex
    };
    let redis = "hydo/onboard/examples/redis/hydo.yaml";
    // the pair of the examples' command lines: one warning, no refusal
    let mut ex = plain(redis);
    ex.new_faults = true;
    let (refused, warned) = nothing_drawn(&ex, "process");
    assert!(refused.is_empty(), "{refused:?}");
    assert_eq!(
        warned,
        [
            "hydo explore: warning: --new-faults adds nothing to --strategy process, which draws \
          pauses, clock steps and one-way losses itself"
        ]
    );
    // with the random strategy it draws
    assert_eq!(nothing_drawn(&ex, "random"), (vec![], vec![]));
    // the Redis example has no groups: and no service with versions:
    let mut ex = plain(redis);
    ex.cluster_faults = true;
    ex.upgrades = true;
    let (refused, warned) = nothing_drawn(&ex, "random");
    assert!(warned.is_empty());
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(
        refused[0].starts_with("hydo explore: --cluster-faults draws nothing in this exploration")
            && refused[0].ends_with("; explore without it"),
        "{}",
        refused[0]
    );
    assert!(refused[1].starts_with("hydo explore: --upgrades draws nothing"));
    // an exploration without faults draws no option's faults by design
    let mut none = ex.clone();
    none.no_faults = true;
    assert_eq!(nothing_drawn(&none, "random"), (vec![], vec![]));
    // the examples' own lines: nothing refused
    for (m, strategy, opts) in EXAMPLE_LINES {
        let mut ex = plain(m);
        for o in *opts {
            match *o {
                "new" => ex.new_faults = true,
                "more" => ex.more_faults = true,
                "cluster" => ex.cluster_faults = true,
                "storage" => ex.storage_faults = true,
                "protocol" => ex.protocol_faults = true,
                "upgrades" => ex.upgrades = true,
                other => panic!("{other}"),
            }
        }
        let (refused, _) = nothing_drawn(&ex, strategy);
        assert!(refused.is_empty(), "{m} {strategy} {opts:?}: {refused:?}");
    }
}
