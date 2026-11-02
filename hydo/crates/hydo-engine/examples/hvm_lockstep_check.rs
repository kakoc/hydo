//! Lock-step control on the Mac engine (M119; on an agent image since M145), the Mac counterpart
//! of `lockstep_check` (hvx): the M116 Mac experiment of docs/design/guest-control.md as a
//! permanent check, driven through the typed client ([`hydo_engine::lockstep`]).
//!
//! ```text
//! hvm_lockstep_check <engine> <Image> <agent image .cpio.gz> <outdir>
//! ```
//!
//! The image is an agent image (the Redis example's, `hydo build`; any agent image works, the
//! recorded values are the Redis example's of Milestone 145). The host boots it under control at
//! the controller's cadence (`rb=64`), takes `SNAP S1` at the stop where the agent's hello line
//! is seen and ends the boot; the agent then idles, waiting for commands. Every run of the check
//! starts from that S1 (a worker's `CTL`, or a fresh boot through `one_controlled`, whose `SNAP
//! S1` starts the workload at that same stop) and ends at the first stop at or after S1 + 20,000
//! exits, where the host says END. What the host types are lines the agent refuses (they are not
//! its protocol), so each typed line makes the agent print one refusal frame:
//!
//! - timed sends: a line typed at an exact exit count after S1 (500, 1,500, 1,501, 3,000; two
//!   lines; another line at 1,500), each run twice from S1 and once from a fresh boot;
//! - reactions: a line typed at the stop where the agent's refusal of the first line is seen, at
//!   once or 200 exits later, twice from S1 and once from a fresh boot;
//! - mid-run branches at the reaction's stop (SNAP, RESUME): a snapshot before the reaction with
//!   the reaction sent again by the host (= the straight run) or not (= the first line alone); a
//!   snapshot with the reaction queued (carried in the snapshot); a snapshot right after typing
//!   the reaction (refused without `retype`, then taken with it; RESUME types it again).
//!
//! Every group must give one fingerprint, every branch the value of the run it must equal, and
//! every value the recorded one below (M116's Mac values were measured on the shell-init image
//! `explore-ob-redis-oci-arm64`, whose engine path was deleted in M145; these were recorded on
//! the first run of this check on the M145 branch and reproduced by the second; a different agent
//! or image changes them: rerun with `HVM_LOCKSTEP_RECORD=1` to print instead of compare).
//! Transcripts of every run go to `<outdir>/transcripts/`. Not a cargo test: it needs the Mac's
//! hypervisor and the image.

use hydo_engine::hvm::Hvm;
use hydo_engine::lockstep::{Lockstep, LockstepStart, Next, SendAnswer, SnapAnswer, StopWhy};
use hydo_engine::{
    BootSpec, ControlledEngine, ControlledWorker, CpuLevel, RunResult, SnapshotStore,
};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};

/// Every run's schedule: the controller's cadence on hvm, nothing else (the host does the rest).
const S: &str = "disk=none rb=64 | run";
/// END at the first stop at or after S1 + this many exits.
const END_AFTER: u64 = 20_000;
// lines the agent refuses (not its protocol): one refusal frame each
const PING1: &[u8] = b"hydo-agent-ping 1\n";
const PING2: &[u8] = b"hydo-agent-ping 2\n";
/// The agent's frames start so (the hello, and each refusal).
const AGENT: &str = "HYDO-AG ";

/// Timed sends: (exits after S1, bytes).
type Sends = Vec<(u64, &'static [u8])>;

#[derive(Clone, Copy, PartialEq)]
enum SnapAt {
    None,
    /// at the reaction's stop, before its send
    BeforeReact,
    /// at the reaction's stop, after a send typed at once: refused without `retype`, then with it
    AfterTyped,
    /// at the reaction's stop, after a send queued for later: no retype needed
    AfterQueued,
}

#[derive(Clone)]
struct Plan {
    sends: Sends,
    /// typed (`delay` exits after the stop) when the agent's first refusal is seen
    react: Option<(u64, &'static [u8])>,
    snap: SnapAt,
    /// for a RESUME: sent at the resume stop (the host's own state, carried by the host)
    resend: Option<(u64, &'static [u8])>,
}

fn plan() -> Plan {
    Plan {
        sends: vec![],
        react: None,
        snap: SnapAt::None,
        resend: None,
    }
}

/// The host of one run from S1 (`s1_at`: S1's exit count; the run ends at s1_at + END_AFTER).
struct Driver<'p> {
    p: &'p Plan,
    s1_at: u64,
    snap_name: &'static str,
    first: bool,
    reacted: bool,
    /// typed sends not acknowledged yet (acknowledged when the agent's next frame is seen)
    typed: Vec<u64>,
}

impl Driver<'_> {
    /// The first stop's actions (the S1 stop of a CTL or a fresh boot, a RESUME's stop).
    fn first_stop(&mut self, s: &mut Lockstep<'_>, now: u64, why: StopWhy) {
        self.first = false;
        if why == StopWhy::Resume && now != self.s1_at {
            if let Some((d, b)) = self.p.resend {
                self.send(s, now + d, b);
            }
            return;
        }
        for (rel, b) in &self.p.sends {
            self.send(s, now + rel, b);
        }
    }

    fn send(&mut self, s: &mut Lockstep<'_>, at: u64, b: &[u8]) -> SendAnswer {
        let a = s.send(at, b).expect("send");
        if let SendAnswer::Typed { id, .. } = a {
            self.typed.push(id);
        }
        a
    }

    /// One stop after the first.
    fn stop(&mut self, s: &mut Lockstep<'_>, now: u64, events: bool, sent: &[u64]) -> bool {
        self.typed.extend_from_slice(sent);
        if events {
            for id in std::mem::take(&mut self.typed) {
                s.ack(id).expect("ACK").expect("ack accepted");
            }
        }
        if let Some((delay, b)) = self.p.react {
            if !self.reacted && events {
                self.reacted = true;
                if self.p.snap == SnapAt::BeforeReact {
                    taken(s.snap(self.snap_name, false).expect("snap"), false);
                }
                let a = self.send(s, now + delay, b);
                match self.p.snap {
                    SnapAt::AfterTyped => {
                        assert!(matches!(a, SendAnswer::Typed { .. }), "{a:?}");
                        match s.snap(self.snap_name, false).expect("snap") {
                            SnapAnswer::Refused(r) => assert!(r.contains("unacked="), "{r}"),
                            t => panic!(
                                "SNAP with typed, unacknowledged input must be refused: {t:?}"
                            ),
                        }
                        taken(s.snap(self.snap_name, true).expect("snap retype"), true);
                    }
                    SnapAt::AfterQueued => {
                        taken(s.snap(self.snap_name, false).expect("snap"), false)
                    }
                    _ => {}
                }
            }
        }
        now >= self.s1_at + END_AFTER
    }

    fn drive(mut self, s: &mut Lockstep<'_>) -> RunResult {
        loop {
            let rep = match s.next_stop().expect("next stop") {
                Next::Done(r) => return r,
                Next::Stop(rep) => rep,
            };
            let now = rep.stop.at;
            if rep.stop.why == StopWhy::Exit {
                return s.end(Some("CHECK: ERROR the guest stopped")).expect("END");
            }
            if self.first {
                self.first_stop(s, now, rep.stop.why);
            } else {
                let agent = rep.events.iter().any(|e| e.text_lossy().contains(AGENT));
                let sent: Vec<u64> = rep
                    .sent
                    .iter()
                    .filter(|x| !x.retype)
                    .map(|x| x.id)
                    .collect();
                if self.stop(s, now, agent, &sent) {
                    return s.end(Some("CHECK: OK ended by the host")).expect("END");
                }
            }
            s.cont().expect("cont");
        }
    }
}

fn taken(a: SnapAnswer, retype: bool) {
    match a {
        SnapAnswer::Taken { retype: ids, .. } => {
            assert_eq!(!ids.is_empty(), retype, "retype ids {ids:?}")
        }
        SnapAnswer::Refused(r) => panic!("SNAP refused: {r}"),
    }
}

fn log(outdir: &Path, tag: &str) -> std::fs::File {
    std::fs::File::create(outdir.join("transcripts").join(format!("{tag}.txt")))
        .expect("transcript")
}

/// The boot's host: SNAP S1 at the stop where the agent's hello is seen; then END (serve) or, in
/// a fresh boot, the run's own first stop (returns the stop's exit count).
fn boot_to_s1(s: &mut Lockstep<'_>) -> u64 {
    loop {
        let rep = match s.next_stop().expect("next stop") {
            Next::Done(r) => panic!("the boot ended before the agent's hello: {}", r.line),
            Next::Stop(rep) => rep,
        };
        if rep.stop.why == StopWhy::Exit {
            panic!("the guest stopped before the agent's hello");
        }
        if rep.events.iter().any(|e| e.text_lossy().contains(AGENT)) {
            taken(s.snap("S1", false).expect("SNAP S1"), false);
            return rep.stop.at;
        }
        s.cont().expect("cont");
    }
}

struct Ctx {
    engine: Hvm,
    spec: BootSpec,
    outdir: PathBuf,
    w: Box<dyn ControlledWorker>,
    s1_at: u64,
    n: usize,
}

impl Ctx {
    fn driver<'p>(&self, p: &'p Plan, snap_name: &'static str) -> Driver<'p> {
        Driver {
            p,
            s1_at: self.s1_at,
            snap_name,
            first: true,
            reacted: false,
            typed: Vec::new(),
        }
    }

    fn ctl(&mut self, start: LockstepStart, p: &Plan, snap: &'static str) -> RunResult {
        let mut l = log(&self.outdir, start.tag());
        let d = self.driver(p, snap);
        let mut s = self.w.lockstep(&start, Some(&mut l)).expect("lockstep");
        d.drive(&mut s)
    }

    /// One run from a fresh boot: the boot's host, then the plan from the S1 stop on.
    fn fresh(&mut self, p: &Plan) -> RunResult {
        self.n += 1;
        let tag = format!("one{}", self.n);
        let mut spec = self.spec.clone();
        spec.outdir = self.outdir.join(&tag);
        std::fs::create_dir_all(&spec.outdir).expect("dir");
        let sc = Schedule::parse(S).expect("schedule");
        let mut one = self
            .engine
            .one_controlled(&spec, &sc, &self.outdir.join(format!("{tag}.err")))
            .expect("one_controlled");
        let mut l = log(&self.outdir, &tag);
        let mut d = self.driver(p, "s");
        let r = {
            let mut s = one.session(Some(&mut l));
            let at = boot_to_s1(&mut s);
            assert_eq!(at, self.s1_at, "a fresh boot's S1 is the serve boot's");
            d.first_stop(&mut s, at, StopWhy::Start);
            s.cont().expect("cont");
            d.drive(&mut s)
        };
        one.wait().expect("wait");
        r
    }
}

fn start(tag: &str) -> LockstepStart {
    LockstepStart::Run {
        tag: tag.into(),
        schedule: Schedule::parse(S).expect("schedule"),
    }
}

fn resume(tag: &str, snap: &str) -> LockstepStart {
    LockstepStart::Resume {
        tag: tag.into(),
        snapshot: snap.into(),
    }
}

/// The recorded values (the Redis example's agent image of M143/M145, hydo-agent 0.3.0, the
/// kernel `Image-explore`; S1 at exit 2,432). As in M116's Mac table, a line typed at +1,501 lands
/// where one typed at +1,500 does.
const WANT: &[(&str, &str)] = &[
    ("base", "94d8815ab15562ca"),
    ("at500", "a23fdf68588f838d"),
    ("at1500", "e1ad13c4c0a777a3"),
    ("at1501", "e1ad13c4c0a777a3"),
    ("at3000", "72652c82156b3a60"),
    ("at1500-other", "040f03c4cc41acd5"),
    ("two", "b8c2f1be9330f81c"),
    ("react", "0e3052408af4d8a9"),
    ("react200", "96417481695cd567"),
];

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 5 {
        eprintln!("usage: hvm_lockstep_check <engine> <Image> <agent image .cpio.gz> <outdir>");
        std::process::exit(2);
    }
    let record = std::env::var_os("HVM_LOCKSTEP_RECORD").is_some();
    let outdir = PathBuf::from(&a[4]);
    std::fs::create_dir_all(outdir.join("transcripts")).expect("outdir");
    std::fs::create_dir_all(outdir.join("snap")).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let image = PathBuf::from(&a[3]).canonicalize().expect("image");
    if let Err(e) = hydo_engine::image::control(&image) {
        eprintln!("hvm_lockstep_check: {e}");
        std::process::exit(2);
    }
    let engine = Hvm {
        hvmx: PathBuf::from(&a[1]),
        env: vec![(
            "HVM_SNAP_DIR".into(),
            outdir.join("snap").display().to_string(),
        )],
        config: hydo_config::Env::process(),
    };
    let spec = BootSpec {
        image,
        ram_mib: 1024,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel: PathBuf::from(&a[2]),
        initrd: None,
        outdir: outdir.join("serve"),
        snapshots: SnapshotStore::Files,
    };
    std::fs::create_dir_all(&spec.outdir).expect("serve dir");
    let boot_s = Schedule::parse(S).expect("schedule");
    let mut boot = engine
        .start_controlled(&spec, &boot_s, &outdir.join("serve.err"))
        .expect("start_controlled");
    let s1_at = {
        let mut l = log(&outdir, "boot");
        let mut s = boot.session(Some(&mut l));
        let at = boot_to_s1(&mut s);
        s.end(Some("CHECK: boot")).expect("END");
        at
    };
    let w = boot.ready().expect("READY");
    println!("S1 at exit {s1_at}");
    let mut cx = Ctx {
        engine,
        spec,
        outdir: outdir.clone(),
        w,
        s1_at,
        n: 0,
    };
    let mut failed = false;
    let mut group = |name: &str, want: Option<&str>, rs: &[(&str, &RunResult)]| -> String {
        let fp = rs[0].1.fp.clone();
        let same = rs.iter().all(|(_, r)| r.fp == fp);
        let want = want.or_else(|| {
            WANT.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| *v)
                .filter(|_| !record)
        });
        let ok = same && want.is_none_or(|w| w == fp);
        failed |= !ok;
        let all: Vec<String> = rs.iter().map(|(k, r)| format!("{k}={}", r.fp)).collect();
        println!(
            "{} {name} {fp} typed={} stops={}{}",
            if ok { "OK  " } else { "FAIL" },
            rs[0].1.extra("typed").unwrap_or("-"),
            rs[0].1.extra("stops").unwrap_or("-"),
            if ok {
                String::new()
            } else {
                format!(" [{}] want {want:?}", all.join(" "))
            }
        );
        fp
    };
    let timed: [(&str, Sends); 7] = [
        ("base", vec![]),
        ("at500", vec![(500, PING1)]),
        ("at1500", vec![(1500, PING1)]),
        ("at1501", vec![(1501, PING1)]),
        ("at3000", vec![(3000, PING1)]),
        ("at1500-other", vec![(1500, PING2)]),
        ("two", vec![(500, PING1), (3000, PING2)]),
    ];
    let mut fps = std::collections::BTreeMap::new();
    for (name, sends) in timed {
        let p = Plan { sends, ..plan() };
        let ra = cx.ctl(start(&format!("{name}-a")), &p, "s");
        let rb = cx.ctl(start(&format!("{name}-b")), &p, "s");
        let rf = cx.fresh(&p);
        fps.insert(
            name,
            group(name, None, &[("a", &ra), ("b", &rb), ("one", &rf)]),
        );
    }
    // reactions: PING1 at +500, PING2 at the stop where the agent's refusal of PING1 is seen
    let react = |delay: u64, snap: SnapAt| Plan {
        sends: vec![(500, PING1)],
        react: Some((delay, PING2)),
        snap,
        resend: None,
    };
    for (name, delay) in [("react", 0u64), ("react200", 200)] {
        let p = react(delay, SnapAt::None);
        let ra = cx.ctl(start(&format!("{name}-a")), &p, "s");
        let rb = cx.ctl(start(&format!("{name}-b")), &p, "s");
        let rf = cx.fresh(&p);
        fps.insert(
            name,
            group(name, None, &[("a", &ra), ("b", &rb), ("one", &rf)]),
        );
    }
    // branches at the reaction's stop
    let b1 = cx.ctl(
        start("br-before"),
        &react(200, SnapAt::BeforeReact),
        "before",
    );
    let resent = Plan {
        resend: Some((200, PING2)),
        ..plan()
    };
    let v_resent = cx.ctl(resume("br-before-resent", "before"), &resent, "s");
    let v_lost = cx.ctl(resume("br-before-lost", "before"), &plan(), "s");
    group(
        "branch: snapshot before the reaction, the host sends it again (= react200)",
        Some(&fps["react200"]),
        &[("base", &b1), ("resumed", &v_resent)],
    );
    group(
        "branch: snapshot before the reaction, nothing sent (= at500)",
        Some(&fps["at500"]),
        &[("resumed", &v_lost)],
    );
    let b2 = cx.ctl(
        start("br-queued"),
        &react(200, SnapAt::AfterQueued),
        "queued",
    );
    let v_q1 = cx.ctl(resume("br-queued-1", "queued"), &plan(), "s");
    let v_q2 = cx.ctl(resume("br-queued-2", "queued"), &plan(), "s");
    group(
        "branch: snapshot with the reaction queued (carried in the snapshot; = react200)",
        Some(&fps["react200"]),
        &[("base", &b2), ("resumed", &v_q1), ("again", &v_q2)],
    );
    let b3 = cx.ctl(start("br-typed"), &react(0, SnapAt::AfterTyped), "typed");
    let v_t1 = cx.ctl(resume("br-typed-1", "typed"), &plan(), "s");
    let v_t2 = cx.ctl(resume("br-typed-2", "typed"), &plan(), "s");
    group(
        "branch: snapshot right after typing (SNAP retype; RESUME types it again; = react)",
        Some(&fps["react"]),
        &[("base", &b3), ("resumed", &v_t1), ("again", &v_t2)],
    );
    let distinct: std::collections::BTreeSet<&String> = fps.values().collect();
    println!(
        "{} distinct fingerprints of {} groups",
        distinct.len(),
        fps.len()
    );
    let _ = cx.w.close();
    if record {
        println!("(HVM_LOCKSTEP_RECORD: values printed, not compared)");
    }
    println!(
        "hvm_lockstep_check: {}",
        if failed { "FAILED" } else { "ALL PASS" }
    );
    std::process::exit(i32::from(failed));
}
