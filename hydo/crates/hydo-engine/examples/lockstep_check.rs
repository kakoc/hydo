//! Lock-step control on a real hvx engine (milestone 118, moved to an agent image in milestone
//! 145): the feasibility experiments of docs/design/guest-control.md as a permanent check, plus
//! the cost of stopping at every event.
//!
//! ```text
//! lockstep_check check <engine> <hvx-run> <bzImage> <initramfs> <redis agent image> <outdir>
//! lockstep_check cost  <engine> <hvx-run> <bzImage> <initramfs> <agent image> <outdir> <end> <rb>...
//! ```
//!
//! The image is `examples/redis` built with `hydo build` (an agent image; until M145 the check
//! ran on the shell-init image `explore-ob-redis-nq`, whose engine path was deleted then). A
//! small scripted host boots it under control (`start_controlled`: the agent's hello, `SNAP S0`,
//! a `sync`, `SNAP S1`, END), then every run is a CTL from that S1 (or, for the fresh-boot
//! runs, `one_controlled` with the same boot, whose SNAP S1 starts the workload at that stop).
//! The runs type agent commands into the guest (a `sync`, a `clock_get`) at exact icounts
//! relative to S1, in reaction to the agent's reply line, before and after a mid-run snapshot
//! (SNAP, RESUME, with and without `retype`), and every run ends with the host's END at the first
//! stop at or after S1 + [`END_AT`]. Each group of runs (twice from S1 and, where the design asks
//! for it, from a fresh boot, a snapshot's continuations, the same sends without the snapshot)
//! must give one fingerprint, and it must be the recorded value ([`RECORDED`]: measured by this
//! program on the bare-metal test machine in M145, the first run recording, the second
//! reproducing; the M116/M118 values were the shell image's). Continuations from a snapshot must
//! also report their base's stop count and input hash. Transcripts go to `<outdir>/transcripts/`;
//! `hydo-engine/tests/lockstep/` keeps a few of them. `hv/hvx/lockstep-metal.sh` runs it.
//!
//! `cost` boots the image once and runs a CTL whose host only answers CONT, ended at S1 + `<end>`,
//! once per given cadence (`rb=<n>`), and prints the engine's seconds, the stops and the host's
//! wall time.
//!
//! Every VM goes through `<hvx-run>` (the box's shared module lock), or runs in this process when
//! it can open the device. Not a cargo test: it needs the hvx module and the image.

use hydo_agent_proto::frame::{deframe_line, Deframed};
use hydo_agent_proto::msg::ClockGet;
use hydo_agent_proto::{Command, Session};
use hydo_engine::hvx::Hvx;
use hydo_engine::lockstep::{Lockstep, LockstepStart, Next, SendAnswer, SnapAnswer};
use hydo_engine::{
    BootSpec, ControlledEngine, ControlledWorker, CpuLevel, RunResult, SnapshotStore,
};
use hydo_schedule::Schedule;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

/// The boot's and most runs' cadence (the controller's default on hvx).
const SB: &str = "disk=none rb=16 | run";
/// Every event a stop: nothing is ever capped.
const SB1: &str = "disk=none rb=1 | run";
/// Every run ends at the first stop at or after S1 + this icount (the agent answers a command
/// within a few hundred).
const END_AT: u64 = 12_000;
/// The verdict every run's END gives.
const END_VERDICT: &str = "LOCKSTEP: ended by the host at its stop";

/// The recorded fingerprint of each group (M145, the Redis agent image `bde714a7…` with
/// hydo-agent 0.3.0 and the host-call port on). A group not listed is checked for agreement only.
/// Equal values are expected: `at2001` is `at2000` (both typed at the same stop), `br-after` is
/// `at6000` (a snapshot does not change the run), `lost-cont` is `at2000` (the continuation that
/// never got the reaction's line is the run that never sent it), and `once16` differs from
/// `queued16` (the same icount, other run-call returns before it: M118's finding), while at rb=1
/// the two agree (`rb1-point`). The boot: `RESULT boot fp=130bebddaeec8183`, S1 at icount 819,706.
const RECORDED: &[(&str, &str)] = &[
    ("at2000", "160fdb20d65534e8"),
    ("at2001", "160fdb20d65534e8"),
    ("at6000", "c88d7edb430648f1"),
    ("br-after", "c88d7edb430648f1"),
    ("br-before", "ab3a11cd8f15d505"),
    ("inert", "a699e483a3c7e1e4"),
    ("lost-cont", "160fdb20d65534e8"),
    ("once16", "c30aae82ed48bb6c"),
    ("queued16", "b7a6b4ec70e8ed2a"),
    ("rb1-point", "f1d0bf49fec6528e"),
    ("react0", "91dbebf3d6f528e1"),
    ("react500", "5d9b4195bc5b9605"),
    ("two", "d94fdcec1e5c5a19"),
];

/// A command the host types: the session's `sync(1)` or a `clock_get`.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Sync,
    Clock,
}

/// When to snapshot in a scripted run.
#[derive(Clone, Copy, PartialEq)]
enum SnapAt {
    None,
    /// at the first stop with icount >= S1 + rel where every typed send is answered and acked
    Rel(u64),
    /// at the reaction's stop, after its send (with or without `retype`)
    AfterReact {
        retype: bool,
    },
    /// at the reaction's stop, before its send
    BeforeReact,
}

/// A scripted host: sends at icounts relative to S1 (queued at the start), at most one reaction
/// to the reply of the first send (a `clock_get` typed `delay` later), at most one snapshot, a
/// send typed at once at the first stop at or after S1 + `at_stop`, and the END.
#[derive(Clone)]
struct Plan {
    sends: Vec<(u64, Op)>,
    react: Option<u64>,
    snap: SnapAt,
    at_stop: Option<u64>,
    /// a send typed at this absolute icount, queued at the start
    at_abs: Option<u64>,
}

impl Plan {
    fn none() -> Plan {
        Plan {
            sends: vec![],
            react: None,
            snap: SnapAt::None,
            at_stop: None,
            at_abs: None,
        }
    }
    fn sends(sends: Vec<(u64, Op)>) -> Plan {
        Plan {
            sends,
            ..Plan::none()
        }
    }
}

struct Outcome {
    result: RunResult,
    stops: u64,
    snap_refused: Option<String>,
    /// the icount the plan's at-once send was typed at
    typed_at: Option<u64>,
    /// how many replies to the host's commands the run saw
    replies: usize,
}

/// The host's state in a run: the agent session (from the boot's S1) and the sends in flight.
#[derive(Clone)]
struct Host {
    sess: Session,
    /// (agent id, engine send id, answered)
    flight: Vec<(u64, u64, bool)>,
    acked: Vec<u64>,
}

impl Host {
    fn type_op(&mut self, s: &mut Lockstep<'_>, at: u64, op: Op) -> Option<(u64, u64)> {
        let out = match op {
            Op::Sync => self.sess.sync(1),
            Op::Clock => self.sess.command(Command::ClockGet(ClockGet {})),
        }
        .expect("the agent offers sync and clock_get");
        let (id, typed) = match s.send(at, &out.bytes).expect("send") {
            SendAnswer::Typed { id, at } => (id, Some(at)),
            SendAnswer::Queued { id, .. } => (id, None),
        };
        self.flight.push((out.id, id, false));
        typed.map(|t| (id, t))
    }

    /// The replies among the stop's events; acknowledges each one's send. The count of new ones.
    fn replies(&mut self, s: &mut Lockstep<'_>, events: &[hydo_engine::lockstep::Event]) -> usize {
        let mut n = 0;
        for e in events {
            let Deframed::Agent(json) = deframe_line(&e.text) else {
                continue;
            };
            for f in self.flight.iter_mut().filter(|f| !f.2) {
                if json.contains(&format!("\"re\":{},", f.0)) {
                    f.2 = true;
                    n += 1;
                    s.ack(f.1).expect("ack").expect("ack accepted");
                    self.acked.push(f.1);
                }
            }
        }
        n
    }

    fn all_answered(&self) -> bool {
        self.flight.iter().all(|f| f.2)
    }
}

/// Drive one controlled session from S1 (its start stop, at `t0`, is the current stop when
/// `here` is given: the fresh boot's SNAP S1 stop) with `plan`; END at `t0 + END_AT`.
fn drive(
    s: &mut Lockstep<'_>,
    host: &mut Host,
    plan: &Plan,
    snap_name: &str,
    t0: Option<u64>,
    here: Option<u64>,
) -> Outcome {
    let mut t0 = t0;
    let mut reacted = false;
    let mut at_stop_done = false;
    let mut snapped = false;
    let mut snap_refused = None;
    let mut typed_at = None;
    let mut replies = 0;
    let mut first_stop = here;
    loop {
        let (now, events) = match first_stop.take() {
            Some(at) => (at, Vec::new()),
            None => match s.next_stop().expect("next") {
                Next::Done(r) => {
                    return Outcome {
                        stops: r.extra("stops").and_then(|v| v.parse().ok()).unwrap_or(0),
                        result: r,
                        snap_refused,
                        typed_at,
                        replies,
                    }
                }
                Next::Stop(rep) => (rep.stop.at, rep.events),
            },
        };
        let start = t0.is_none();
        let t = *t0.get_or_insert(now);
        if start {
            for (rel, op) in &plan.sends {
                host.type_op(s, t + rel, *op);
            }
            if let Some(a) = plan.at_abs {
                host.type_op(s, a, Op::Clock);
            }
        }
        let got = host.replies(s, &events);
        replies += got;
        if let Some(rel) = plan.at_stop {
            if !at_stop_done && now >= t + rel {
                at_stop_done = true;
                if let Some((_, at)) = host.type_op(s, now, Op::Clock) {
                    typed_at = Some(at);
                }
            }
        }
        if let Some(delay) = plan.react {
            // the reaction: to the reply of the plan's first send
            if !reacted && host.flight.first().is_some_and(|f| f.2) && got > 0 {
                reacted = true;
                if plan.snap == SnapAt::BeforeReact {
                    take(s, snap_name, false, &mut snap_refused);
                    snapped = true;
                }
                host.type_op(s, now + delay, Op::Clock);
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
            if !snapped && now >= t + rel && host.all_answered() {
                take(s, snap_name, false, &mut snap_refused);
                snapped = true;
            }
        }
        if now >= t + END_AT {
            let r = s.end(Some(END_VERDICT)).expect("end");
            return Outcome {
                stops: r.extra("stops").and_then(|v| v.parse().ok()).unwrap_or(0),
                result: r,
                snap_refused,
                typed_at,
                replies,
            };
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

/// The boot (serve worker and fresh boot alike): the hello, SNAP S0, a `sync(0)` and its reply,
/// SNAP S1. A serve boot ENDs there; a fresh boot returns at the S1 stop (its icount) with the
/// host, and the workload phase starts at that stop.
fn boot(s: &mut Lockstep<'_>, serve: bool) -> Result<(Host, u64, Option<RunResult>), String> {
    let mut host: Option<Host> = None;
    let mut sync_sent = false;
    let mut n = 0u64;
    loop {
        let rep = match s.next_stop().expect("next") {
            Next::Done(r) => return Err(format!("the boot ended early: {}", r.line)),
            Next::Stop(rep) => rep,
        };
        n += 1;
        if n > 2_000_000 {
            return Err("no S1 after 2,000,000 boot stops".into());
        }
        let now = rep.stop.at;
        match host.as_mut() {
            None => {
                for e in &rep.events {
                    if let Deframed::Agent(json) = deframe_line(&e.text) {
                        let sess = Session::from_hello(&json).map_err(|e| format!("hello: {e}"))?;
                        host = Some(Host {
                            sess,
                            flight: Vec::new(),
                            acked: Vec::new(),
                        });
                    }
                }
                if let Some(h) = host.as_mut() {
                    if let SnapAnswer::Refused(e) = s.snap("S0", false).expect("snap") {
                        return Err(format!("SNAP S0: {e}"));
                    }
                    let out = h.sess.sync(0).expect("sync");
                    let id = match s.send(now, &out.bytes).expect("send") {
                        SendAnswer::Typed { id, .. } | SendAnswer::Queued { id, .. } => id,
                    };
                    h.flight.push((out.id, id, false));
                    sync_sent = true;
                }
            }
            Some(h) => {
                h.replies(s, &rep.events);
                if sync_sent && h.all_answered() {
                    if let SnapAnswer::Refused(e) = s.snap("S1", false).expect("snap") {
                        return Err(format!("SNAP S1: {e}"));
                    }
                    let mut h = h.clone();
                    h.flight.clear();
                    h.acked.clear();
                    if serve {
                        let r = s.end(Some("LOCKSTEP: boot")).expect("end");
                        return Ok((h, now, Some(r)));
                    }
                    return Ok((h, now, None));
                }
            }
        }
        s.cont().expect("cont");
    }
}

struct Env {
    hvx: Hvx,
    spec: BootSpec,
    outdir: PathBuf,
}

/// The worker after its boot: the host at S1 and S1's icount.
struct Booted {
    w: Box<dyn ControlledWorker>,
    host: Host,
    s1_at: u64,
}

impl Env {
    fn transcript(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.outdir.join("transcripts").join(format!("{name}.txt")))
            .expect("transcript")
    }

    fn start(&self) -> Booted {
        let mut cb = self
            .hvx
            .start_controlled(
                &self.spec,
                &Schedule::parse(SB).unwrap(),
                &self.outdir.join("worker.err"),
            )
            .expect("start_controlled");
        let (host, s1_at, r) = {
            let mut log = self.transcript("boot");
            let mut s = cb.session(Some(&mut log));
            boot(&mut s, true).unwrap_or_else(|e| panic!("the boot: {e}"))
        };
        println!(
            "boot: {} (S1 at icount {s1_at})",
            r.map(|r| r.line).unwrap_or_default()
        );
        let w = cb.ready().expect("READY");
        println!("worker READY boot_secs={}", w.boot_secs());
        Booted { w, host, s1_at }
    }

    fn ctl(&self, b: &mut Booted, tag: &str, sched: &str, plan: &Plan, snap: &str) -> Outcome {
        let mut log = self.transcript(tag);
        let start = LockstepStart::Run {
            tag: tag.into(),
            schedule: Schedule::parse(sched).expect("schedule"),
        };
        let mut host = b.host.clone();
        let mut s = b.w.lockstep(&start, Some(&mut log)).expect("lockstep");
        drive(&mut s, &mut host, plan, snap, None, None)
    }

    fn resume(&self, b: &mut Booted, tag: &str, snap: &str) -> Outcome {
        let mut log = self.transcript(tag);
        let start = LockstepStart::Resume {
            tag: tag.into(),
            snapshot: snap.into(),
        };
        let mut host = b.host.clone();
        let mut s = b.w.lockstep(&start, Some(&mut log)).expect("lockstep");
        drive(&mut s, &mut host, &Plan::none(), "-", Some(b.s1_at), None)
    }

    fn one(&self, name: &str, plan: &Plan) -> Outcome {
        let mut spec = self.spec.clone();
        spec.outdir = self.outdir.join(name);
        std::fs::create_dir_all(&spec.outdir).expect("one dir");
        let mut c = self
            .hvx
            .one_controlled(
                &spec,
                &Schedule::parse(SB).expect("schedule"),
                &self.outdir.join(format!("{name}.err")),
            )
            .expect("one");
        let mut log = self.transcript(name);
        let o = {
            let mut s = c.session(Some(&mut log));
            let (mut host, s1_at, _) =
                boot(&mut s, false).unwrap_or_else(|e| panic!("{name}: {e}"));
            drive(&mut s, &mut host, plan, "-", None, Some(s1_at))
        };
        c.wait().expect("wait");
        o
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 8 || !matches!(a[1].as_str(), "check" | "cost") {
        eprintln!("usage: lockstep_check check|cost <engine> <hvx-run> <bzImage> <initramfs> <image> <outdir> [<end> <rb>...]");
        std::process::exit(2);
    }
    let outdir = PathBuf::from(&a[7]);
    std::fs::create_dir_all(outdir.join("transcripts")).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let hvx = Hvx {
        engine: PathBuf::from(&a[2]),
        hvx_run: Some(PathBuf::from(&a[3])),
        extra_env: vec!["HVX_EXPLORE_WALL_SECS=900".into()],
        env: Vec::new(),
        config: hydo_config::Env::process(),
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
    match hydo_engine::image::control(&spec.image) {
        Ok(p) => println!("image {}: agent protocol {p}", spec.image.display()),
        Err(e) => {
            eprintln!("lockstep_check: {e}");
            std::process::exit(2);
        }
    }
    let env = Env { hvx, spec, outdir };
    let ok = if a[1] == "check" {
        check(&env)
    } else {
        cost(&env, &a[8..])
    };
    std::process::exit(if ok { 0 } else { 1 });
}

/// Each group's runs must agree, and equal the recorded value when there is one.
struct Groups {
    g: BTreeMap<&'static str, Vec<(String, String)>>,
}

fn recorded(group: &str) -> Option<&'static str> {
    RECORDED.iter().find(|(g, _)| *g == group).map(|(_, v)| *v)
}

impl Groups {
    fn add(&mut self, group: &'static str, role: &str, o: &Outcome) {
        println!(
            "{group:<12} {role:<9} fp={} stops={:<6} typed={} inh={} replies={} verdict={}",
            o.result.fp,
            o.stops,
            o.result.extra("typed").unwrap_or("-"),
            o.result.extra("inh").unwrap_or("-"),
            o.replies,
            o.result.verdict
        );
        self.g
            .entry(group)
            .or_default()
            .push((role.to_string(), o.result.fp.clone()));
    }

    fn verdict(&self) -> bool {
        let mut all = true;
        println!("\n== summary");
        for (name, runs) in &self.g {
            let first = &runs[0].1;
            let same = runs.iter().all(|(_, f)| f == first);
            let want = recorded(name);
            let as_recorded = want.is_none_or(|w| w == first);
            all &= same && as_recorded;
            println!(
                "{name:<12} {} runs {} fp={first}{}",
                runs.len(),
                if same { "SAME" } else { "DIFFERENT" },
                match want {
                    Some(w) if w == first => " (= recorded)".to_string(),
                    Some(w) => format!(" (recorded {w}: DIFFERENT)"),
                    None => " (not recorded)".to_string(),
                }
            );
        }
        println!("the values as RECORDED lines:");
        for (name, runs) in &self.g {
            println!("    (\"{name}\", \"{}\"),", runs[0].1);
        }
        println!("lockstep_check: {}", if all { "ALL PASS" } else { "FAIL" });
        all
    }
}

/// A run that the agent answered: every send of the plan got its reply, and END gave the verdict.
fn answered(o: &Outcome, want: usize) -> bool {
    let ok = o.replies == want && o.result.verdict == END_VERDICT;
    if !ok {
        println!(
            "{}: {} replies (want {want}), verdict {:?}",
            o.result.tag, o.replies, o.result.verdict
        );
    }
    ok
}

fn check(env: &Env) -> bool {
    let mut g = Groups { g: BTreeMap::new() };
    let mut b = env.start();
    let mut extra_ok = true;
    let sync_at = |rel| Plan::sends(vec![(rel, Op::Sync)]);

    // 1. inert: a controlled run whose host only says CONT, ended at its stop
    for i in ["a", "b"] {
        let o = env.ctl(&mut b, &format!("inert-{i}"), SB, &Plan::none(), "-");
        extra_ok &= answered(&o, 0);
        g.add("inert", i, &o);
    }
    // 2. typed at exact times (queued at the start: the run calls before them are capped)
    let timed: [(&'static str, Vec<(u64, Op)>); 4] = [
        ("at2000", vec![(2000, Op::Sync)]),
        ("at2001", vec![(2001, Op::Sync)]),
        ("at6000", vec![(6000, Op::Sync)]),
        ("two", vec![(2000, Op::Sync), (6000, Op::Clock)]),
    ];
    for (grp, sends) in &timed {
        for i in ["a", "b"] {
            let o = env.ctl(
                &mut b,
                &format!("{grp}-{i}"),
                SB,
                &Plan::sends(sends.clone()),
                "-",
            );
            extra_ok &= answered(&o, sends.len());
            g.add(grp, i, &o);
        }
    }
    // 3. reactions to the agent's reply line (a clock_get at once, or 500 later)
    for (grp, delay) in [("react0", 0u64), ("react500", 500)] {
        let plan = Plan {
            react: Some(delay),
            ..sync_at(2000)
        };
        for i in ["a", "b"] {
            let o = env.ctl(&mut b, &format!("{grp}-{i}"), SB, &plan, "-");
            extra_ok &= answered(&o, 2);
            g.add(grp, i, &o);
        }
    }
    //    Typed at once at a stop, and queued at the start for the same icount: at rb=16 the
    //    queued one's capped run calls come first and the guest may differ (M118: hvx guests
    //    depend on where run calls return); at rb=1 every event is a stop, nothing is capped, and
    //    the two agree.
    for (rb, sched, grp_once, grp_q) in [
        (16, SB, "once16", "queued16"),
        (1, SB1, "rb1-point", "rb1-point"),
    ] {
        let once = Plan {
            at_stop: Some(2005),
            ..sync_at(1000)
        };
        let mut at = None;
        for i in ["a", "b"] {
            let o = env.ctl(&mut b, &format!("once{rb}-{i}"), sched, &once, "-");
            extra_ok &= answered(&o, 2);
            at = at.or(o.typed_at);
            g.add(grp_once, &format!("once-{i}"), &o);
        }
        let Some(at) = at else {
            println!("once{rb}: the at-once send was not typed at once");
            extra_ok = false;
            continue;
        };
        println!("once{rb}: typed at icount {at} (S1 + {})", at - b.s1_at);
        let queued = Plan {
            at_abs: Some(at),
            ..sync_at(1000)
        };
        for i in ["a", "b"] {
            let o = env.ctl(&mut b, &format!("queued{rb}-{i}"), sched, &queued, "-");
            extra_ok &= answered(&o, 2);
            g.add(grp_q, &format!("queued-{i}"), &o);
        }
    }
    // 4. a mid-run snapshot (S1 + 1,157), a send typed after it / answered before it; the
    //    continuations (RESUME) and the same sends without a snapshot give the base's value
    for (grp, rel) in [("br-after", 6000u64), ("br-before", 500)] {
        let plan = Plan {
            snap: SnapAt::Rel(1157),
            ..sync_at(rel)
        };
        let base = env.ctl(&mut b, &format!("{grp}-base"), SB, &plan, grp);
        extra_ok &= answered(&base, 1);
        g.add(grp, "base", &base);
        for i in ["c1", "c2"] {
            let o = env.resume(&mut b, &format!("{grp}-{i}"), grp);
            extra_ok &= same_book(&base, &o);
            g.add(grp, i, &o);
        }
        let o = env.ctl(&mut b, &format!("{grp}-straight"), SB, &sync_at(rel), "-");
        g.add(grp, "straight", &o);
    }
    // 5. the snapshot rule at the reaction's own stop: typed, then SNAP -> refused until `retype`;
    //    the continuations type it again and equal the base (= react0)
    let plan = Plan {
        react: Some(0),
        snap: SnapAt::AfterReact { retype: false },
        ..sync_at(2000)
    };
    let o = env.ctl(&mut b, "same-base", SB, &plan, "same");
    let refused = o.snap_refused.clone().unwrap_or_default();
    println!("same-base: SNAP without retype answered: {refused:?}");
    extra_ok &= refused.starts_with("snap same unacked=");
    g.add("react0", "same-base", &o);
    for i in ["c1", "c2"] {
        let c = env.resume(&mut b, &format!("same-{i}"), "same");
        extra_ok &= same_book(&o, &c);
        g.add("react0", &format!("same-{i}"), &c);
    }
    //    SNAP before the send: the continuations never get the clock_get (M116's "lost" case)
    let plan = Plan {
        react: Some(0),
        snap: SnapAt::BeforeReact,
        ..sync_at(2000)
    };
    let o = env.ctl(&mut b, "lost-base", SB, &plan, "lost");
    g.add("react0", "lost-base", &o);
    for i in ["c1", "c2"] {
        let o = env.resume(&mut b, &format!("lost-{i}"), "lost");
        g.add("lost-cont", i, &o);
    }
    b.w.close().expect("close");
    // 6. fresh boots (`--control boot one`: the same boot; its SNAP S1 starts the workload)
    g.add("inert", "one", &env.one("one-inert", &Plan::none()));
    g.add("at2000", "one", &env.one("one-at2000", &sync_at(2000)));
    g.add(
        "react0",
        "one",
        &env.one(
            "one-react0",
            &Plan {
                react: Some(0),
                ..sync_at(2000)
            },
        ),
    );
    g.add("br-after", "one", &env.one("one-br-after", &sync_at(6000)));
    g.verdict() && extra_ok
}

/// A continuation reports its base's stop count, typed count and input hash.
fn same_book(base: &Outcome, cont: &Outcome) -> bool {
    let same = base.stops == cont.stops
        && base.result.extra("typed") == cont.result.extra("typed")
        && base.result.extra("inh") == cont.result.extra("inh");
    if !same {
        println!(
            "continuation {}: stops/typed/inh differ from its base {}",
            cont.result.tag, base.result.tag
        );
    }
    same
}

fn cost(env: &Env, args: &[String]) -> bool {
    let Some((end, rbs)) = args.split_first() else {
        eprintln!("cost: give the end (an icount after S1) and cadences");
        return false;
    };
    let end: u64 = end.parse().expect("the end: an icount");
    let mut b = env.start();
    let mut ok = true;
    for rb in rbs {
        let sched = if rb == "0" {
            "disk=none | run".to_string()
        } else {
            format!("disk=none rb={rb} | run")
        };
        let t = Instant::now();
        let mut log = env.transcript(&format!("cost-rb{rb}"));
        let start = LockstepStart::Run {
            tag: format!("cost-rb{rb}"),
            schedule: Schedule::parse(&sched).expect("schedule"),
        };
        let mut s = b.w.lockstep(&start, Some(&mut log)).expect("lockstep");
        let mut t0 = None;
        let r = loop {
            match s.next_stop().expect("next") {
                Next::Done(r) => break r,
                Next::Stop(rep) => {
                    let t = *t0.get_or_insert(rep.stop.at);
                    if rep.stop.at >= t + end {
                        break s.end(Some(END_VERDICT)).expect("end");
                    }
                    s.cont().expect("cont");
                }
            }
        };
        let wall = t.elapsed().as_secs_f64();
        ok &= r.verdict == END_VERDICT;
        println!(
            "rb={rb:<6} CTL secs={} wall={wall:.1} stops={} fp={} | {}",
            r.secs_text,
            r.extra("stops").unwrap_or("-"),
            r.fp,
            r.verdict
        );
        std::io::stdout().flush().ok();
    }
    b.w.close().expect("close");
    ok
}
