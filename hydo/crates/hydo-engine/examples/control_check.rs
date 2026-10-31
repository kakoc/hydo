//! The lock-step control sessions of hvmx (M119) on a real engine, as a permanent test of the M116
//! Mac experiment (docs/design/guest-control.md, "Feasibility experiments").
//!
//! ```text
//! control_check <hvmx> <Image> <image .cpio.gz> <outdir> experiment|run "<schedule>"...
//! ```
//!
//! The test drives today's shell-init image the way `RUN` does, but through a control session:
//! the host types the schedule's guest part at the session's start, `go` at the first stop where
//! `hydo-explore-workload-done` has appeared, and ends at `hydo-stack-marker: workload complete`,
//! with a fingerprint at each of the two points (`fp` = hvx_explore's combination of the two, as in
//! a RUN's RESULT). Every configuration runs three times: twice from the restored S1 in one serve
//! worker (`a`, `b`) and once as the first request of a `fresh` worker (no snapshot restore). It
//! prints `<name> <fp>` when the three agree, and fails otherwise.
//!
//! - `experiment`: the M116 table (`disk=none | run K=redis@5+2`, a line typed at exits +500,
//!   +1,500, +1,501, +3,000, other text, two lines, a reaction to `KILLED redis` at once and 200
//!   exits later), the same reaction with a cadence of 16, a snapshot refused while typed input
//!   waits, and mid-run branches: a snapshot at the reaction point, continued with the reaction
//!   re-sent by the host, re-typed by the engine from the snapshot's plan, or lost.
//! - `run`: each schedule as a plain RUN and as a control session in the same worker; the two
//!   fingerprints must be equal (and equal from the fresh worker).

use hydo_engine::control::{ControlRequest, ControlStart, LockStep, SnapAnswer, Stop};
use hydo_engine::{hvm, BootSpec, CpuLevel, Engine, EngineError, SnapshotStore, Worker};
use hydo_schedule::Schedule;
use std::path::PathBuf;

const DONE: &[u8] = b"hydo-explore-workload-done";
const FINAL: &[u8] = b"hydo-stack-marker: workload complete";

/// What the host does in one configuration.
#[derive(Clone, Default)]
struct Plan {
    /// The schedule: host tokens before `|`, the guest part typed at the start.
    schedule: String,
    /// Observation cadence in exits.
    every: u64,
    /// (exits after the start, bytes) typed at those times.
    at: Vec<(u64, Vec<u8>)>,
    /// When this text appears: type the bytes `delay` exits later.
    react: Option<(Vec<u8>, u64, Vec<u8>)>,
    /// Take a snapshot at the reaction's stop, before (or, with `plan`, after) the reaction is sent.
    snap: Option<(String, bool)>,
    /// At the session's start: a WAKE one exit later, where a SNAP must be refused.
    refuse_check: bool,
}

/// How a session continues after its start (from S1, or from a snapshot).
struct Run {
    fp: u64,
    verdict: String,
    snap_at: Option<u64>,
}

fn hex(s: &str) -> u64 {
    u64::from_str_radix(s, 16).unwrap_or_else(|_| panic!("not a fingerprint: {s}"))
}

fn line_of(schedule: &str) -> (Vec<String>, Vec<u8>) {
    let (host, guest) = schedule.split_once('|').expect("a schedule has a |");
    // hvmx types the guest part with the space after the bar (M102: not normalised)
    let mut g = guest.trim_end_matches([' ', '\n']).as_bytes().to_vec();
    g.push(b'\n');
    (host.split_whitespace().map(str::to_string).collect(), g)
}

/// Drive one session to its end: start stop already returned as `st`.
fn drive(
    ls: &mut dyn LockStep,
    mut st: Stop,
    plan: &Plan,
    wfp: &mut Option<u64>,
    reacted: &mut bool,
) -> Result<Run, EngineError> {
    let mut snap_at = None;
    loop {
        if let Some(e) = &st.end {
            panic!("the guest stopped at {} ({e}) before the end marker", st.at);
        }
        for m in &st.matches {
            if m.text == DONE {
                *wfp = Some(hex(&ls.fingerprint()?));
                ls.send(st.at, b"go\n")?;
            } else if m.text == FINAL {
                let w = match wfp {
                    Some(w) => *w,
                    None => hex(&ls.fingerprint()?),
                };
                let r = ls.end()?;
                return Ok(Run {
                    fp: hydo_engine::control::combine_fp(hex(&r.fp), w),
                    verdict: r.verdict,
                    snap_at,
                });
            } else if let Some((marker, delay, text)) = &plan.react {
                if m.text == *marker && !*reacted {
                    *reacted = true;
                    match &plan.snap {
                        Some((name, false)) => {
                            snap_at = Some(snap(ls, name)?);
                            ls.send(st.at + delay, text)?;
                        }
                        Some((name, true)) => {
                            ls.send(st.at + delay, text)?;
                            snap_at = Some(snap(ls, name)?);
                        }
                        None => ls.send(st.at + delay, text)?,
                    }
                }
            }
        }
        st = ls.cont()?;
    }
}

fn snap(ls: &mut dyn LockStep, name: &str) -> Result<u64, EngineError> {
    match ls.snap(name)? {
        SnapAnswer::Taken(at) => Ok(at),
        SnapAnswer::InputPending(n) => panic!("SNAP {name} refused: {n} bytes pending"),
    }
}

/// One session from S1 with `plan`.
fn session(w: &mut dyn Worker, tag: &str, plan: &Plan) -> Result<Run, EngineError> {
    let ls = w.lock_step().expect("this engine has control sessions");
    let (host, guest) = line_of(&plan.schedule);
    let host: Vec<&str> = host.iter().map(String::as_str).collect();
    let st = ls.begin(&ControlRequest::from_s1(tag, plan.every.max(1), &host))?;
    let x0 = st.at;
    ls.send(x0, &guest)?;
    for (d, t) in &plan.at {
        ls.send(x0 + d, t)?;
    }
    ls.watch(DONE)?;
    ls.watch(FINAL)?;
    if let Some((m, _, _)) = &plan.react {
        ls.watch(m)?;
    }
    let st = if plan.refuse_check {
        ls.wake(x0 + 1)?;
        let st = ls.cont()?;
        assert_eq!(st.at, x0 + 1, "the WAKE stop");
        match ls.snap("refused")? {
            SnapAnswer::InputPending(n) => assert!(n > 0),
            SnapAnswer::Taken(_) => panic!("SNAP must be refused while typed input waits"),
        }
        st
    } else {
        st
    };
    let (mut wfp, mut reacted) = (None, false);
    drive(ls, st, plan, &mut wfp, &mut reacted)
}

/// A continuation from snapshot `name`, taken at the reaction (the base's wfp is not reached yet).
fn continuation(
    w: &mut dyn Worker,
    tag: &str,
    name: &str,
    keep_plan: bool,
    resend: Option<(u64, &[u8])>,
    plan: &Plan,
) -> Result<Run, EngineError> {
    let ls = w.lock_step().expect("control sessions");
    let st = ls.begin(&ControlRequest {
        tag: tag.into(),
        start: ControlStart::Snapshot {
            name: name.into(),
            keep_plan,
        },
    })?;
    if !keep_plan {
        // the host's own state: the watches it had armed are its to re-arm
        ls.watch(DONE)?;
        ls.watch(FINAL)?;
    }
    if let Some((delay, text)) = resend {
        ls.send(st.at + delay, text)?;
    }
    let (mut wfp, mut reacted) = (None, true);
    drive(ls, st, plan, &mut wfp, &mut reacted)
}

struct Check {
    failed: bool,
}

impl Check {
    fn same(&mut self, name: &str, fps: &[(&str, u64)], verdict: &str) -> u64 {
        let first = fps[0].1;
        if fps.iter().all(|(_, f)| *f == first) {
            println!("{name} {first:016x} {}", short(verdict));
        } else {
            self.failed = true;
            let all: Vec<String> = fps.iter().map(|(k, f)| format!("{k}={f:016x}")).collect();
            println!("{name} DIFFER {}", all.join(" "));
        }
        first
    }
    fn expect(&mut self, what: &str, ok: bool) {
        if !ok {
            self.failed = true;
            println!("FAIL {what}");
        }
    }
}

fn short(v: &str) -> String {
    v.split(" (").next().unwrap_or(v).to_string()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("usage: control_check <hvmx> <Image> <image.cpio.gz> <outdir> experiment|run \"<schedule>\"...");
        std::process::exit(2);
    }
    let image = PathBuf::from(&a[3]).canonicalize().expect("image");
    let outdir = PathBuf::from(&a[4]);
    std::fs::create_dir_all(outdir.join("snap")).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let engine = hvm::Hvm {
        hvmx: PathBuf::from(&a[1]),
        env: vec![(
            "HVM_SNAP_DIR".into(),
            outdir.join("snap").display().to_string(),
        )],
    };
    let spec = |dir: &str| {
        std::fs::create_dir_all(outdir.join(dir)).expect("dir");
        BootSpec {
            image: image.clone(),
            ram_mib: 1024,
            vcpus: 1,
            cpu: CpuLevel::V2,
            kernel: PathBuf::from(&a[2]),
            initrd: None,
            outdir: outdir.join(dir),
            snapshots: SnapshotStore::Files,
        }
    };
    let mut ck = Check { failed: false };
    let mut serve = engine
        .start(&spec("serve"), &outdir.join("serve.err"))
        .unwrap_or_else(|e| panic!("start: {e}"));
    let mut fresh_n = 0;
    let mut fresh = |plan: &Plan| -> Run {
        fresh_n += 1;
        let d = format!("fresh{fresh_n}");
        let mut w = engine
            .start_fresh(&spec(&d), &outdir.join(format!("{d}.err")))
            .unwrap_or_else(|e| panic!("start_fresh: {e}"));
        let r = session(w.as_mut(), "f", plan).unwrap_or_else(|e| panic!("fresh: {e}"));
        w.close().expect("close");
        r
    };
    let mode = a[5].as_str();
    if mode == "run" {
        for (i, s) in a[6..].iter().enumerate() {
            let sched = Schedule::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            let r = serve.run(&format!("r{i}"), &sched).expect("RUN");
            let plan = Plan {
                schedule: s.clone(),
                every: 1,
                ..Plan::default()
            };
            let c = session(serve.as_mut(), &format!("c{i}"), &plan).expect("CTRL");
            let f = fresh(&plan);
            ck.same(
                &format!("run:{s}"),
                &[("run", hex(&r.fp)), ("ctrl", c.fp), ("fresh", f.fp)],
                &c.verdict,
            );
        }
        serve.close().expect("close");
        std::process::exit(if ck.failed { 1 } else { 0 });
    }
    assert_eq!(mode, "experiment", "mode: experiment or run");
    let sched = "disk=none | run K=redis@5+2".to_string();
    let base = Plan {
        schedule: sched.clone(),
        every: 1,
        ..Plan::default()
    };
    let ping = |n: u8| format!("hydo-agent-ping {}\n", n as char).into_bytes();
    let killed = b"KILLED redis".to_vec();
    let mut configs: Vec<(&str, Plan)> = vec![("base", base.clone())];
    for (name, d, t) in [
        ("at500", 500, ping(b'1')),
        ("at1500", 1500, ping(b'1')),
        ("at3000", 3000, ping(b'1')),
        ("at1500b", 1500, ping(b'2')),
        ("at1501", 1501, ping(b'1')),
    ] {
        configs.push((
            name,
            Plan {
                at: vec![(d, t)],
                ..base.clone()
            },
        ));
    }
    configs.push((
        "two",
        Plan {
            at: vec![(500, ping(b'1')), (3000, ping(b'2'))],
            ..base.clone()
        },
    ));
    configs.push((
        "react",
        Plan {
            react: Some((killed.clone(), 0, ping(b'1'))),
            ..base.clone()
        },
    ));
    let react200 = Plan {
        react: Some((killed.clone(), 200, ping(b'1'))),
        ..base.clone()
    };
    configs.push(("react200", react200.clone()));
    configs.push((
        "react200-every16",
        Plan {
            every: 16,
            ..react200.clone()
        },
    ));
    configs.push((
        "refused-snap",
        Plan {
            refuse_check: true,
            ..base.clone()
        },
    ));
    let mut fps = std::collections::BTreeMap::new();
    for (name, plan) in &configs {
        let ra = session(serve.as_mut(), &format!("{name}-a"), plan).expect("a");
        let rb = session(serve.as_mut(), &format!("{name}-b"), plan).expect("b");
        let rf = fresh(plan);
        let fp = ck.same(
            name,
            &[("a", ra.fp), ("b", rb.fp), ("fresh", rf.fp)],
            &ra.verdict,
        );
        fps.insert(name.to_string(), fp);
    }
    let fp = |n: &str| fps[n];
    ck.expect(
        "different times and texts give different fingerprints",
        [
            "base", "at500", "at1500", "at3000", "at1500b", "two", "react", "react200",
        ]
        .iter()
        .map(|n| fp(n))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
            == 8,
    );
    ck.expect(
        "stops alone change nothing (refused-snap = base)",
        fp("refused-snap") == fp("base"),
    );

    // mid-run branches at the reaction point
    let snap_first = Plan {
        snap: Some(("br".into(), false)),
        ..react200.clone()
    };
    let b = session(serve.as_mut(), "br-base", &snap_first).expect("branch base");
    let v1 = continuation(
        serve.as_mut(),
        "br-resent",
        "br",
        true,
        Some((200, &ping(b'1'))),
        &react200,
    )
    .expect("v1");
    let v2 = continuation(serve.as_mut(), "br-lost", "br", true, None, &react200).expect("v2");
    let plan_carried = Plan {
        snap: Some(("bp".into(), true)),
        ..react200.clone()
    };
    let p = session(serve.as_mut(), "bp-base", &plan_carried).expect("plan base");
    let v3 = continuation(serve.as_mut(), "bp-retyped", "bp", true, None, &react200).expect("v3");
    let v4 = continuation(serve.as_mut(), "bp-dropped", "bp", false, None, &react200).expect("v4");
    let v3b =
        continuation(serve.as_mut(), "bp-retyped2", "bp", true, None, &react200).expect("v3b");
    println!(
        "branch point at exit {} (the reaction's stop); the plan-carrying snapshot at {}",
        b.snap_at.unwrap_or(0),
        p.snap_at.unwrap_or(0)
    );
    ck.same(
        "branch-resent",
        &[
            ("base", b.fp),
            ("resent", v1.fp),
            ("straight", fp("react200")),
        ],
        &v1.verdict,
    );
    ck.same(
        "branch-retyped",
        &[
            ("base", p.fp),
            ("retyped", v3.fp),
            ("retyped2", v3b.fp),
            ("straight", fp("react200")),
        ],
        &v3.verdict,
    );
    ck.same(
        "branch-lost",
        &[("lost", v2.fp), ("dropped", v4.fp), ("base", fp("base"))],
        &v2.verdict,
    );
    serve.close().expect("close");
    std::process::exit(if ck.failed { 1 } else { 0 });
}
