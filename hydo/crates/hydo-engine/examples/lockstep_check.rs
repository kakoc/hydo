//! Lock-step control on a real hvx engine (milestone 118): the feasibility experiments of
//! docs/design/guest-control.md as a permanent check, plus the cost of stopping at every event.
//!
//! ```text
//! lockstep_check check <engine> <hvx-run> <bzImage> <initramfs> <redis image> <outdir>
//! lockstep_check cost  <engine> <hvx-run> <bzImage> <initramfs> <image> <outdir> "<schedule>" <rb>...
//! ```
//!
//! `check` runs the M116 configurations on the onboarded Redis image (`explore-ob-redis-nq`, the
//! schedule `disk=none | run K=redis@5+2`): bytes typed into the serial input at exact icounts
//! after S1, in reaction to a console line, before and after a mid-run snapshot (SNAP, RESUME),
//! each twice from S1 and, where the design asks for it, from a fresh boot (`--control one`). Every
//! group of runs must give one fingerprint; where M116 recorded the value (same module, same
//! image), it must be that value. Transcripts of every run go to `<outdir>/transcripts/`.
//!
//! `cost` runs the schedule as a plain RUN and as a CTL run whose host only answers CONT, once per
//! given cadence (`rb=<n>` added to the host part; 0 = none), and prints the engine's seconds, the
//! number of stops and the host-side wall time.
//!
//! Every VM goes through `<hvx-run>` (the box's shared module lock). Not a cargo test: it needs
//! the hvx module and the guest images.

use hydo_engine::hvx::Hvx;
use hydo_engine::lockstep::{Lockstep, LockstepStart, Next, SendAnswer, SnapAnswer};
use hydo_engine::{BootSpec, CpuLevel, Engine, RunResult, SnapshotStore, Worker};
use hydo_schedule::Schedule;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

const S: &str = "disk=none | run K=redis@5+2";
const SB: &str = "disk=none rb=16 | run K=redis@5+2";
const PING1: &[u8] = b"hydo-agent-ping 1"; // no newline: the shell init would take a line as its command (M116)
const PING2: &[u8] = b"hydo-agent-ping 2";

/// Timed sends: (icount after the start stop, bytes).
type Sends = Vec<(u64, &'static [u8])>;
/// One group: M116's value (if recorded) and every (role, fingerprint).
type Group = (Option<&'static str>, Vec<(String, String)>);

/// When to snapshot in a scripted run.
#[derive(Clone, Copy, PartialEq)]
enum SnapAt {
    None,
    /// at the first stop with icount >= start + rel (unacknowledged older sends acknowledged first)
    Rel(u64),
    /// at the reaction's stop, after its send (with or without `retype`)
    AfterReact {
        retype: bool,
    },
    /// at the reaction's stop, before its send
    BeforeReact,
}

/// A scripted host: fixed sends, at most one reaction to a console line, at most one snapshot.
#[derive(Clone)]
struct Plan {
    sends: Vec<(u64, &'static [u8])>,
    react: Option<(&'static str, u64)>,
    snap: SnapAt,
}

impl Plan {
    fn none() -> Plan {
        Plan {
            sends: vec![],
            react: None,
            snap: SnapAt::None,
        }
    }
}

struct Outcome {
    result: RunResult,
    stops: u64,
    snap_refused: Option<String>,
}

/// Drive one controlled session with a plan. A resumed run uses `Plan::none()`: its queue comes
/// from the snapshot.
fn drive(s: &mut Lockstep<'_>, plan: &Plan, snap_name: &str) -> Outcome {
    let mut start = None;
    let mut reacted = false;
    let mut snapped = false;
    let mut snap_refused = None;
    let mut typed: Vec<(u64, u64)> = Vec::new(); // (id, typed at)
    let mut acked: Vec<u64> = Vec::new();
    loop {
        let rep = match s.next_stop().expect("next") {
            Next::Done(r) => {
                return Outcome {
                    stops: r.extra("stops").and_then(|v| v.parse().ok()).unwrap_or(0),
                    result: r,
                    snap_refused,
                }
            }
            Next::Stop(rep) => rep,
        };
        let now = rep.stop.at;
        if start.is_none() {
            start = Some(now);
            for (rel, b) in &plan.sends {
                if let SendAnswer::Typed { id, at } = s.send(now + rel, b).expect("send") {
                    typed.push((id, at));
                }
            }
        }
        let t0 = start.unwrap();
        typed.extend(rep.sent.iter().map(|x| (x.id, x.at)));
        if let Some((marker, delay)) = plan.react {
            if !reacted && rep.events.iter().any(|e| e.text_lossy().contains(marker)) {
                reacted = true;
                if plan.snap == SnapAt::BeforeReact {
                    take(s, snap_name, false, &mut snap_refused);
                    snapped = true;
                }
                if let SendAnswer::Typed { id, at } = s.send(now + delay, PING1).expect("send") {
                    typed.push((id, at));
                }
                if let SnapAt::AfterReact { retype } = plan.snap {
                    if !retype {
                        // the rule: refused while a typed send is not acknowledged
                        take(s, snap_name, false, &mut snap_refused);
                    }
                    take(s, snap_name, true, &mut snap_refused);
                    snapped = true;
                }
            }
        }
        if let SnapAt::Rel(rel) = plan.snap {
            if !snapped && now >= t0 + rel {
                for (id, at) in &typed {
                    if *at < now && !acked.contains(id) {
                        // test-only: the shell init has no agent to acknowledge; bytes typed at
                        // an earlier stop (thousands of events ago here) have been read
                        s.ack(*id).expect("ack").expect("ack accepted");
                        acked.push(*id);
                    }
                }
                take(s, snap_name, false, &mut snap_refused);
                snapped = true;
            }
        }
        s.cont().expect("cont");
    }
}

fn take(s: &mut Lockstep<'_>, name: &str, retype: bool, refused: &mut Option<String>) {
    match s.snap(name, retype).expect("snap") {
        SnapAnswer::Taken { .. } => {}
        SnapAnswer::Refused(why) => *refused = Some(why),
    }
}

struct Env {
    hvx: Hvx,
    spec: BootSpec,
    outdir: PathBuf,
}

impl Env {
    fn transcript(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.outdir.join("transcripts").join(format!("{name}.txt")))
            .expect("transcript")
    }

    fn ctl(&self, w: &mut dyn Worker, tag: &str, sched: &str, plan: &Plan, snap: &str) -> Outcome {
        let mut log = self.transcript(tag);
        let start = LockstepStart::Run {
            tag: tag.into(),
            schedule: Schedule::parse(sched).expect("schedule"),
        };
        let mut s = w.lockstep(&start, Some(&mut log)).expect("lockstep");
        drive(&mut s, plan, snap)
    }

    fn resume(&self, w: &mut dyn Worker, tag: &str, snap: &str) -> Outcome {
        let mut log = self.transcript(tag);
        let start = LockstepStart::Resume {
            tag: tag.into(),
            snapshot: snap.into(),
        };
        let mut s = w.lockstep(&start, Some(&mut log)).expect("lockstep");
        drive(&mut s, &Plan::none(), "-")
    }

    fn one(&self, name: &str, sched: &str, plan: &Plan) -> Outcome {
        let mut spec = self.spec.clone();
        spec.outdir = self.outdir.join(name);
        std::fs::create_dir_all(&spec.outdir).expect("one dir");
        let mut c = self
            .hvx
            .one_lockstep(
                &spec,
                &Schedule::parse(sched).expect("schedule"),
                &self.outdir.join(format!("{name}.err")),
            )
            .expect("one");
        let mut log = self.transcript(name);
        let o = drive(&mut c.session(Some(&mut log)), plan, "-");
        c.wait().expect("wait");
        o
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 8 || !matches!(a[1].as_str(), "check" | "cost") {
        eprintln!("usage: lockstep_check check|cost <engine> <hvx-run> <bzImage> <initramfs> <image> <outdir> [\"<schedule>\" <rb>...]");
        std::process::exit(2);
    }
    let outdir = PathBuf::from(&a[7]);
    std::fs::create_dir_all(outdir.join("transcripts")).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let hvx = Hvx {
        engine: PathBuf::from(&a[2]),
        hvx_run: Some(PathBuf::from(&a[3])),
        snap_dir: std::env::var("HYDO_SNAP_DIR").unwrap_or_else(|_| "/var/tmp/hvx-snap".into()),
        extra_env: vec!["HVX_EXPLORE_WALL_SECS=900".into()],
        env: Vec::new(),
    };
    let spec = BootSpec {
        image: PathBuf::from(&a[6]),
        ram_mib: 1024,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel: PathBuf::from(&a[4]),
        initrd: Some(PathBuf::from(&a[5])),
        outdir: outdir.join("serve"),
        snapshots: SnapshotStore::Memory,
    };
    std::fs::create_dir_all(&spec.outdir).expect("serve dir");
    let env = Env { hvx, spec, outdir };
    let ok = if a[1] == "check" {
        check(&env)
    } else {
        cost(&env, &a[8..])
    };
    std::process::exit(if ok { 0 } else { 1 });
}

fn start(env: &Env) -> Box<dyn Worker> {
    let w = env
        .hvx
        .start(&env.spec, &env.outdir.join("worker.err"))
        .expect("start");
    println!("worker READY boot_secs={}", w.boot_secs());
    w
}

/// Each group's runs must agree; `want` is M116's recorded value for the group, if any.
struct Groups {
    g: BTreeMap<&'static str, Group>,
}

impl Groups {
    fn add(&mut self, group: &'static str, want: Option<&'static str>, role: &str, o: &Outcome) {
        println!(
            "{group:<14} {role:<10} fp={} stops={:<6} typed={} inh={} verdict={}",
            o.result.fp,
            o.stops,
            o.result.extra("typed").unwrap_or("-"),
            o.result.extra("inh").unwrap_or("-"),
            o.result.verdict
        );
        let e = self.g.entry(group).or_insert((want, Vec::new()));
        e.1.push((role.to_string(), o.result.fp.clone()));
    }

    fn add_run(
        &mut self,
        group: &'static str,
        want: Option<&'static str>,
        role: &str,
        r: &RunResult,
    ) {
        self.add(
            group,
            want,
            role,
            &Outcome {
                result: r.clone(),
                stops: 0,
                snap_refused: None,
            },
        );
    }

    fn verdict(&self) -> bool {
        let mut all = true;
        println!("\n== summary");
        for (name, (want, runs)) in &self.g {
            let first = &runs[0].1;
            let same = runs.iter().all(|(_, f)| f == first);
            let as_m116 = want.is_none_or(|w| w == first);
            all &= same && as_m116;
            println!(
                "{name:<14} {} runs {} fp={first}{}",
                runs.len(),
                if same { "SAME" } else { "DIFFERENT" },
                match want {
                    Some(w) if *w == first => " (= M116)".to_string(),
                    Some(w) => format!(" (M116 recorded {w}: DIFFERENT)"),
                    None => String::new(),
                }
            );
        }
        println!("lockstep_check: {}", if all { "ALL PASS" } else { "FAIL" });
        all
    }
}

fn check(env: &Env) -> bool {
    let mut g = Groups { g: BTreeMap::new() };
    let mut wb = start(env);
    let w = wb.as_mut();
    let mut extra_ok = true;
    let p = |sends: Vec<(u64, &'static [u8])>| Plan {
        sends,
        react: None,
        snap: SnapAt::None,
    };

    // 1. inert: a controlled run whose host only says CONT equals the plain run
    for (grp, sched, want) in [
        ("inert-plain", S, "fd4b848eb8969419"),
        ("inert-rb16", SB, "a6e72fcdff5f17ed"),
    ] {
        let r = w
            .run(&format!("{grp}-run"), &Schedule::parse(sched).unwrap())
            .unwrap();
        g.add_run(grp, Some(want), "RUN", &r);
        let o = env.ctl(w, &format!("{grp}-ctl"), sched, &Plan::none(), "-");
        g.add(grp, Some(want), "CTL", &o);
    }
    // 2. typed at exact times (plain run calls: the M116 batch 1 values)
    let timed: [(&'static str, Sends, &'static str); 4] = [
        ("at20000", vec![(20000, PING1)], "146dec7ffae541c1"),
        ("at20001", vec![(20001, PING1)], "46a292f1a33788cc"),
        ("at60000", vec![(60000, PING1)], "023bcafa7233eaf2"),
        (
            "two",
            vec![(20000, PING1), (60000, PING2)],
            "2309a3b53b2db7d0",
        ),
    ];
    for (grp, sends, want) in &timed {
        for i in ["a", "b"] {
            let o = env.ctl(w, &format!("{grp}-{i}"), S, &p(sends.clone()), "-");
            g.add(grp, Some(want), i, &o);
        }
    }
    // 3. reactions to a console line, observed at a fixed cadence (rb=16)
    for (grp, delay, want) in [
        ("react0", 0u64, "b83bf67d682b4609"),
        ("react5000", 5000, "f6d7670d74596283"),
    ] {
        let plan = Plan {
            sends: vec![],
            react: Some(("KILLED redis", delay)),
            snap: SnapAt::None,
        };
        for i in ["a", "b"] {
            let o = env.ctl(w, &format!("{grp}-{i}"), SB, &plan, "-");
            g.add(grp, Some(want), i, &o);
        }
    }
    // 4. a mid-run snapshot (the M116 branch point: tick 3 = +11,569), typed after / before it
    for (grp, rel, want) in [
        ("br-after", 60000u64, "456001c6ec2263cb"),
        ("br-before", 5000, "8f87ca0fcdddf60a"),
    ] {
        let plan = Plan {
            sends: vec![(rel, PING1)],
            react: None,
            snap: SnapAt::Rel(11569),
        };
        let snap = grp;
        let o = env.ctl(w, &format!("{grp}-base"), SB, &plan, snap);
        g.add(grp, Some(want), "base", &o);
        for i in ["c1", "c2"] {
            let o = env.resume(w, &format!("{grp}-{i}"), snap);
            g.add(grp, Some(want), i, &o);
        }
        let o = env.ctl(
            w,
            &format!("{grp}-straight"),
            SB,
            &p(vec![(rel, PING1)]),
            "-",
        );
        g.add(grp, Some(want), "straight", &o);
    }
    // 5. the snapshot rule at the reaction's own stop: typed, then SNAP -> refused until `retype`;
    //    the continuations type it again and equal the base
    let plan = Plan {
        sends: vec![],
        react: Some(("KILLED", 0)),
        snap: SnapAt::AfterReact { retype: false },
    };
    let o = env.ctl(w, "same-base", SB, &plan, "same");
    let refused = o.snap_refused.clone().unwrap_or_default();
    println!("same-base: SNAP without retype answered: {refused:?}");
    extra_ok &= refused.starts_with("snap same unacked=");
    g.add("same-retype", Some("b83bf67d682b4609"), "base", &o);
    for i in ["c1", "c2"] {
        let o = env.resume(w, &format!("same-{i}"), "same");
        g.add("same-retype", Some("b83bf67d682b4609"), i, &o);
    }
    //    SNAP before the send: the continuations never get the line (M116's "lost" case)
    let plan = Plan {
        sends: vec![],
        react: Some(("KILLED", 0)),
        snap: SnapAt::BeforeReact,
    };
    let o = env.ctl(w, "lost-base", SB, &plan, "lost");
    g.add("lost-base", Some("b83bf67d682b4609"), "base", &o);
    for i in ["c1", "c2"] {
        let o = env.resume(w, &format!("lost-{i}"), "lost");
        g.add("lost-cont", Some("a6e72fcdff5f17ed"), i, &o);
    }
    wb.close().expect("close");
    // 6. fresh boots (`--control one`)
    g.add(
        "inert-plain",
        Some("fd4b848eb8969419"),
        "one",
        &env.one("one-inert", S, &Plan::none()),
    );
    g.add(
        "at20000",
        Some("146dec7ffae541c1"),
        "one",
        &env.one("one-at20000", S, &p(vec![(20000, PING1)])),
    );
    g.add(
        "react0",
        Some("b83bf67d682b4609"),
        "one",
        &env.one(
            "one-react0",
            SB,
            &Plan {
                sends: vec![],
                react: Some(("KILLED redis", 0)),
                snap: SnapAt::None,
            },
        ),
    );
    g.add(
        "br-after",
        Some("456001c6ec2263cb"),
        "one",
        &env.one("one-br-after", SB, &p(vec![(60000, PING1)])),
    );
    g.verdict() && extra_ok
}

fn cost(env: &Env, args: &[String]) -> bool {
    let Some((sched, rbs)) = args.split_first() else {
        eprintln!("cost: give a schedule and cadences");
        return false;
    };
    let (host, guest) = sched.split_once('|').expect("schedule with |");
    let mut w = start(env);
    let mut ok = true;
    for rb in rbs {
        let s = if rb == "0" {
            sched.clone()
        } else {
            format!("{} rb={rb} |{guest}", host.trim_end())
        };
        let t = Instant::now();
        let r = w
            .run(
                &format!("cost-run-rb{rb}"),
                &Schedule::parse(&s).expect("schedule"),
            )
            .expect("run");
        let run_wall = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let o = env.ctl(
            w.as_mut(),
            &format!("cost-ctl-rb{rb}"),
            &s,
            &Plan::none(),
            "-",
        );
        let ctl_wall = t.elapsed().as_secs_f64();
        let same = o.result.fp == r.fp;
        ok &= same;
        println!(
            "rb={rb:<6} RUN secs={} wall={run_wall:.1} fp={} | CTL secs={} wall={ctl_wall:.1} stops={} fp={} {} | {}",
            r.secs_text,
            r.fp,
            o.result.secs_text,
            o.stops,
            o.result.fp,
            if same { "SAME" } else { "DIFFERENT" },
            o.result.verdict
        );
        std::io::stdout().flush().ok();
    }
    w.close().expect("close");
    ok
}
