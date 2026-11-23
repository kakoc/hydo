//! Options the engine or the image cannot use, refused before any VM (Milestone 345).
//!
//! An option that draws a fault kind of its own is worth nothing where the engine refuses what it
//! draws, or where the image has nothing for it to act on: every such schedule was refused one by
//! one (Milestone 208 records a refused run, never a failure), or the option silently drew
//! nothing. So the explorer asks, before its first run, what each such option would draw: it
//! draws the first [`SEEDS`] schedules with only that option on and asks the engine's own rule
//! ([`hydo_engine::refusal_on`], the module's for hvx and the hvx-only host tokens for the Mac and
//! KVM engines) about each. A refused draw refuses the option, with the engine's reason. Two
//! options act on parts of the manifest: `--storage-faults` needs a service with `storage:` and a
//! `faults:` that allows `corrupt` or `powerloss`, and `--block-faults` a data disk. Every refusal
//! is one line that names the option, the reason, and what to use instead; the explorer prints
//! them all and stops with the refusal's exit status 5 (`crate::REFUSED`).

use crate::gen::{Backend, Ex};
use hydo_engine::EngineKind;

/// How many schedules of each option are asked about.
pub const SEEDS: u64 = 64;

/// The engine that runs the explorer's schedules.
pub fn kind_of(ex: &Ex, kvm: bool) -> EngineKind {
    match (ex.backend, kvm) {
        (Backend::Hvx, _) => EngineKind::Hvx,
        (Backend::Hvm, true) => EngineKind::Kvm,
        (Backend::Hvm, false) => EngineKind::Hvm,
    }
}

/// The options of an exploration that draw their own kinds, as `ex` has them on, with what each
/// changes on an [`Ex`] and what to use instead when the engine refuses it.
type Opt = (
    &'static str,
    fn(&Ex) -> bool,
    fn(&mut Ex, bool),
    &'static str,
);

const OPTS: [Opt; 7] = [
    (
        "--timing",
        |e| e.timing,
        |e, v| e.timing = v,
        "explore without it, or with one vCPU",
    ),
    (
        "--preempt",
        |e| e.preempt,
        |e, v| e.preempt = v,
        "explore without it",
    ),
    (
        "--more-faults",
        |e| e.more_faults,
        |e, v| e.more_faults = v,
        "explore without it, for example --strategy process",
    ),
    (
        "--turns",
        |e| e.turns,
        |e, v| e.turns = v,
        "explore without it, or --sweep turns",
    ),
    (
        "--pause-workload or --pause-thread",
        |e| e.workload_pauses,
        |e, v| e.workload_pauses = v,
        "rebuild the image with this hydo (hydo build), or explore on hvx",
    ),
    (
        "--block-faults",
        |e| e.block_faults,
        |e, v| e.block_faults = v,
        "use --backend kvm, or the Mac",
    ),
    (
        "--storage-faults",
        |e| e.storage_faults,
        |e, v| e.storage_faults = v,
        "explore without it",
    ),
];

/// `ex` with every option of [`OPTS`] off.
fn bare(ex: &Ex) -> Ex {
    let mut b = ex.clone();
    for (_, _, set, _) in OPTS {
        set(&mut b, false);
    }
    b.bursts = false;
    b
}

/// The rendered schedules of seeds 1 to [`SEEDS`].
fn draws(ex: &Ex, strategy: &str) -> Vec<String> {
    (1..=SEEDS)
        .map(|sd| ex.render(&ex.compose_seed(strategy, sd, false)))
        .collect()
}

/// Every refusal of `ex`'s options on engine `kind` with `vcpus` vCPUs for an image of the
/// fingerprint epoch `epoch`, one line each (empty: the exploration may start).
pub fn refusals(ex: &Ex, strategy: &str, kind: EngineKind, vcpus: u32, epoch: u64) -> Vec<String> {
    let mut out = Vec::new();
    let base = bare(ex);
    let plain = draws(&base, strategy);
    for (name, on, set, instead) in OPTS {
        if !on(ex) {
            continue;
        }
        // what the image has for the option to act on
        if let Some(why) = image_gap(name, ex) {
            out.push(format!("hydo explore: {name}: {why}"));
            continue;
        }
        let mut one = base.clone();
        set(&mut one, true);
        one.bursts = name == "--turns" && ex.bursts;
        let got = draws(&one, strategy);
        let refused = got.iter().find_map(|t| {
            let sc = hydo_schedule::Schedule::parse(t).ok()?;
            hydo_engine::refusal_on(kind, vcpus, epoch, &sc).map(|why| (t.clone(), why))
        });
        if let Some((t, why)) = refused {
            out.push(format!(
                "hydo explore: {name} draws schedules that the {} engine refuses ({why}; for \
                 example {t:?}); {instead}",
                kind.name()
            ));
            continue;
        }
        if name == "--storage-faults" && got == plain {
            out.push(format!(
                "hydo explore: {name}: this image's faults: allows neither corrupt nor \
                 powerloss, so it draws nothing; add one of them to faults:, or {instead}"
            ));
        }
    }
    out
}

/// What the image lacks for option `name`, in words with what to do (None: nothing).
fn image_gap(name: &str, ex: &Ex) -> Option<String> {
    match name {
        "--storage-faults" => {
            let has = ex
                .space
                .as_ref()
                .is_some_and(|sp| !sp.storage_svcs.is_empty());
            (!has).then(|| {
                "it damages the files and cuts the power of services with storage:, and no \
                 service of this image's manifest has storage:; give the service a file system \
                 of its own (storage: {dir:, size_mib:}, hydo guide faults), or explore \
                 without it"
                    .to_string()
            })
        }
        "--block-faults" => (ex.backend == Backend::Hvm && !ex.hvm_disk).then(|| {
            "it damages the data disk's blocks, and this image has no data disk; give the \
             manifest a data: disk and build the image again, or explore without it"
                .to_string()
        }),
        _ => None,
    }
}
