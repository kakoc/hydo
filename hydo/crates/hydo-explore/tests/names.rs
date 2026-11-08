//! The explorer draws only what an image has (no silent passes, 2026-11-07): every schedule it
//! generates from an example manifest's fault space, with every option that space allows (the
//! options a manifest cannot take, `--concurrency` without a knob `Q` and `--buggify` without
//! `buggify: true`, are refused by `hydo explore`), names only services, links, fakes, proxies
//! and knobs of that manifest (`hydo_control::names`, which refuses the others before a run).

use crate::support;

use hydo_control::Plan;
use hydo_explore::gen::{Backend, Ex};
use hydo_explore::space::{grpc_targets, Space};
use hydo_schedule::Schedule;
use support::{manifests, top};

fn plan_of(rel: &str) -> Plan {
    let (mut data, meta) = hydo_manifest::for_explorer(&top().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {}", e.message));
    if let Some(meta) = meta {
        data["_compose"] = meta;
    }
    Plan::from_manifest(&hydo_manifest::Manifest::from_validated(data))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn every_generated_schedule_names_only_what_the_manifest_has() {
    let mut n = 0;
    for m in manifests() {
        let plan = plan_of(&m);
        for backend in [Backend::Hvx, Backend::Hvm] {
            let mut ex = Ex::default();
            let sp = Space::from_path(&top().join(&m)).expect("an example manifest loads");
            ex.grpc_targets = grpc_targets(Some(&sp.m));
            sp.apply(&mut ex);
            ex.backend = backend;
            ex.timing = true;
            ex.new_faults = true;
            ex.more_faults = backend == Backend::Hvx;
            ex.preempt = true;
            ex.freeze_call = !ex.fc_pairs.is_empty();
            ex.protocol_faults = !ex.pf_targets.is_empty();
            ex.grpc_on = !ex.grpc_targets.is_empty();
            ex.buggify = plan.buggify;
            ex.concurrency = plan.knob("Q").is_some();
            // swarm-pos needs a workload of 15 steps or more (as the corpus and the explorer)
            let strategies: &[&str] = if ex.norders >= 15 {
                &["random", "swarm", "swarm-pos", "process"]
            } else {
                &["random", "swarm", "process"]
            };
            for strategy in strategies.iter().copied() {
                for seed in 1..=40u64 {
                    let mut s = ex.compose_seed(strategy, seed, false);
                    s = ex.add_timing(s, seed);
                    s = ex.add_preempt(s, seed);
                    s = ex.add_buggify(s, seed);
                    s = ex.add_concurrency(s, seed);
                    s = ex.add_compose(s, seed);
                    if backend == Backend::Hvm {
                        s = ex.hvm_supported(&s);
                    }
                    let text = ex.render(&s);
                    let sc = Schedule::parse(&text).unwrap_or_else(|e| panic!("{m}: {text}: {e}"));
                    if let Err(e) = hydo_control::names::check(&plan, &sc) {
                        panic!("{m} ({backend:?}, {strategy}, seed {seed}): {text}: {e}");
                    }
                    let w = hydo_control::names::step_warnings(&plan, &sc);
                    assert!(
                        w.is_empty(),
                        "{m} ({backend:?}, {strategy}, seed {seed}): {w:?}"
                    );
                    n += 1;
                }
            }
        }
    }
    assert!(n > 1000, "{n} schedules");
}
