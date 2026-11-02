//! Lock-step control on hvmx (M119), the Mac counterpart of `lockstep_check` (hvx, M118): the
//! M116 Mac experiment of docs/design/guest-control.md as a permanent check, driven through the
//! same typed client ([`hydo_engine::lockstep`]) that drives hvx_explore.
//!
//! ```text
//! hvm_lockstep_check check <hvmx> <Image> <explore-ob-redis-oci-arm64.cpio.gz> <outdir>
//! hvm_lockstep_check run   <hvmx> <Image> <image .cpio.gz> <outdir> "<schedule>"...
//! ```
//!
//! `check` runs the M116 configurations on the experiment's image (`disk=none | run K=redis@5+2`):
//! a line typed into the serial input at an exact exit count after S1, in reaction to a console
//! line, and around mid-run snapshots (SNAP, RESUME). Each configuration runs twice from the
//! restored S1 (`CTL`) and, unless it is a branch, once from a fresh boot (`--control one`); every
//! group must give one fingerprint, and where M116 recorded the value on this image, that value.
//! `run` runs each schedule as a plain RUN and as a CTL run whose host only answers CONT (from S1
//! and from a fresh boot): the three fingerprints must be equal. Transcripts of every controlled run
//! go to `<outdir>/transcripts/`. Not a cargo test: it needs the Mac's hypervisor and the images.

use hydo_engine::hvm::Hvm;
use hydo_engine::lockstep::{Lockstep, LockstepStart, Next, SnapAnswer, StopWhy};
use hydo_engine::{BootSpec, CpuLevel, Engine, RunResult, SnapshotStore, Worker};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};

const S: &str = "disk=none | run K=redis@5+2";
/// The same schedule with a stop at every exit: a reaction lands at the exit where its line is seen.
const S1: &str = "disk=none rb=1 | run K=redis@5+2";
// with the newline, as M116's Mac experiment typed it (gc_hvm.c)
const PING1: &[u8] = b"hydo-agent-ping 1\n";
const PING2: &[u8] = b"hydo-agent-ping 2\n";
const KILLED: &str = "KILLED redis";
/// Timed sends: (exits after the start stop, bytes).
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
    sends: Vec<(u64, &'static [u8])>,
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

fn drive(s: &mut Lockstep<'_>, p: &Plan, snap_name: &str) -> RunResult {
    let mut first = true;
    let mut reacted = false;
    loop {
        let rep = match s.next_stop().expect("next stop") {
            Next::Done(r) => return r,
            Next::Stop(rep) => rep,
        };
        let now = rep.stop.at;
        if first {
            first = false;
            for (rel, b) in &p.sends {
                s.send(now + rel, b).expect("send");
            }
            if rep.stop.why == StopWhy::Resume {
                if let Some((d, b)) = p.resend {
                    s.send(now + d, b).expect("resend");
                }
            }
        }
        if let Some((delay, b)) = p.react {
            if !reacted && rep.events.iter().any(|e| e.text_lossy().contains(KILLED)) {
                reacted = true;
                if p.snap == SnapAt::BeforeReact {
                    taken(s.snap(snap_name, false).expect("snap"), false);
                }
                s.send(now + delay, b).expect("send");
                match p.snap {
                    SnapAt::AfterTyped => {
                        match s.snap(snap_name, false).expect("snap") {
                            SnapAnswer::Refused(r) => assert!(r.contains("unacked="), "{r}"),
                            t => panic!(
                                "SNAP with typed, unacknowledged input must be refused: {t:?}"
                            ),
                        }
                        taken(s.snap(snap_name, true).expect("snap retype"), true);
                    }
                    SnapAt::AfterQueued => taken(s.snap(snap_name, false).expect("snap"), false),
                    _ => {}
                }
            }
        }
        s.cont().expect("cont");
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

fn ctl(w: &mut dyn Worker, outdir: &Path, start: LockstepStart, p: &Plan, snap: &str) -> RunResult {
    let mut l = log(outdir, start.tag());
    let mut s = w.lockstep(&start, Some(&mut l)).expect("lockstep");
    drive(&mut s, p, snap)
}

struct Ctx {
    engine: Hvm,
    spec: BootSpec,
    outdir: PathBuf,
    n: usize,
}

impl Ctx {
    fn fresh(&mut self, sched: &str, p: &Plan) -> RunResult {
        self.n += 1;
        let tag = format!("one{}", self.n);
        let mut spec = self.spec.clone();
        spec.outdir = self.outdir.join(&tag);
        std::fs::create_dir_all(&spec.outdir).expect("dir");
        let sc = Schedule::parse(sched).expect("schedule");
        let mut one = self
            .engine
            .one_lockstep(&spec, &sc, &self.outdir.join(format!("{tag}.err")))
            .expect("one_lockstep");
        let mut l = log(&self.outdir, &tag);
        let r = drive(&mut one.session(Some(&mut l)), p, "s");
        one.wait().expect("wait");
        r
    }
}

fn start(tag: &str, sched: &str) -> LockstepStart {
    LockstepStart::Run {
        tag: tag.into(),
        schedule: Schedule::parse(sched).expect("schedule"),
    }
}

fn resume(tag: &str, snap: &str) -> LockstepStart {
    LockstepStart::Resume {
        tag: tag.into(),
        snapshot: snap.into(),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 || (a[1] != "check" && a[1] != "run") {
        eprintln!("usage: hvm_lockstep_check check|run <hvmx> <Image> <image.cpio.gz> <outdir> [\"<schedule>\"...]");
        std::process::exit(2);
    }
    let outdir = PathBuf::from(&a[5]);
    std::fs::create_dir_all(outdir.join("transcripts")).expect("outdir");
    std::fs::create_dir_all(outdir.join("snap")).expect("outdir");
    let outdir = outdir.canonicalize().expect("outdir");
    let engine = Hvm {
        hvmx: PathBuf::from(&a[2]),
        env: vec![(
            "HVM_SNAP_DIR".into(),
            outdir.join("snap").display().to_string(),
        )],
        config: hydo_config::Env::process(),
    };
    let spec = BootSpec {
        image: PathBuf::from(&a[4]).canonicalize().expect("image"),
        ram_mib: 1024,
        vcpus: 1,
        cpu: CpuLevel::V2,
        kernel: PathBuf::from(&a[3]),
        initrd: None,
        outdir: outdir.join("serve"),
        snapshots: SnapshotStore::Files,
    };
    std::fs::create_dir_all(&spec.outdir).expect("serve dir");
    let mut w = engine
        .start(&spec, &outdir.join("serve.err"))
        .expect("start");
    let mut cx = Ctx {
        engine,
        spec,
        outdir: outdir.clone(),
        n: 0,
    };
    let mut failed = false;
    let mut group = |name: &str, want: Option<&str>, rs: &[(&str, &RunResult)]| -> String {
        let fp = rs[0].1.fp.clone();
        let same = rs.iter().all(|(_, r)| r.fp == fp);
        let ok = same && want.is_none_or(|w| w == fp);
        failed |= !ok;
        let all: Vec<String> = rs.iter().map(|(k, r)| format!("{k}={}", r.fp)).collect();
        let v = rs[0].1.verdict.split(" (").next().unwrap_or("").to_string();
        println!(
            "{} {name} {fp} typed={} {}{}",
            if ok { "OK  " } else { "FAIL" },
            rs[0].1.extra("typed").unwrap_or("-"),
            v,
            if ok {
                String::new()
            } else {
                format!(" [{}] want {want:?}", all.join(" "))
            }
        );
        fp
    };
    if a[1] == "run" {
        for (i, sc) in a[6..].iter().enumerate() {
            let r = w
                .run(&format!("r{i}"), &Schedule::parse(sc).expect("schedule"))
                .expect("RUN");
            let c = ctl(
                w.as_mut(),
                &outdir,
                start(&format!("c{i}"), sc),
                &plan(),
                "s",
            );
            let f = cx.fresh(sc, &plan());
            group(
                &format!("run:{sc}"),
                None,
                &[("run", &r), ("ctl", &c), ("one", &f)],
            );
        }
        w.close().expect("close");
        std::process::exit(i32::from(failed));
    }
    // M116's Mac table (docs/design/guest-control.md "Feasibility experiments", same image)
    let timed: [(&str, Option<&str>, Sends); 7] = [
        ("base", Some("6b9321d4b13e5d12"), vec![]),
        ("at500", Some("dae633ebb392b3d8"), vec![(500, PING1)]),
        ("at1500", Some("7b01d5c261a5334a"), vec![(1500, PING1)]),
        ("at3000", Some("ec004616904a49f0"), vec![(3000, PING1)]),
        (
            "at1500-other",
            Some("5406af0d7002e705"),
            vec![(1500, PING2)],
        ),
        ("at1501", Some("7b01d5c261a5334a"), vec![(1501, PING1)]),
        (
            "two",
            Some("d36401c377728af4"),
            vec![(500, PING1), (3000, PING2)],
        ),
    ];
    for (name, want, sends) in timed {
        let p = Plan { sends, ..plan() };
        let ra = ctl(w.as_mut(), &outdir, start(&format!("{name}-a"), S), &p, "s");
        let rb = ctl(w.as_mut(), &outdir, start(&format!("{name}-b"), S), &p, "s");
        let rf = cx.fresh(S, &p);
        group(name, want, &[("a", &ra), ("b", &rb), ("one", &rf)]);
    }
    // reactions: the first stop whose events show KILLED redis (rb=1: a stop at every exit)
    let mut straight = std::collections::BTreeMap::new();
    for (name, delay) in [("react", 0u64), ("react200", 200)] {
        let p = Plan {
            react: Some((delay, PING1)),
            ..plan()
        };
        let ra = ctl(
            w.as_mut(),
            &outdir,
            start(&format!("{name}-a"), S1),
            &p,
            "s",
        );
        let rb = ctl(
            w.as_mut(),
            &outdir,
            start(&format!("{name}-b"), S1),
            &p,
            "s",
        );
        let rf = cx.fresh(S1, &p);
        straight.insert(
            name,
            group(name, None, &[("a", &ra), ("b", &rb), ("one", &rf)]),
        );
    }
    let base = ctl(w.as_mut(), &outdir, start("rb1-base", S1), &plan(), "s");
    group(
        "rb1-nothing-typed (= base)",
        Some("6b9321d4b13e5d12"),
        &[("a", &base)],
    );
    // branches at the reaction's stop
    let p200 = Plan {
        react: Some((200, PING1)),
        ..plan()
    };
    let b1 = ctl(
        w.as_mut(),
        &outdir,
        start("br-before", S1),
        &Plan {
            snap: SnapAt::BeforeReact,
            ..p200.clone()
        },
        "before",
    );
    let v_resent = ctl(
        w.as_mut(),
        &outdir,
        resume("br-before-resent", "before"),
        &Plan {
            resend: Some((200, PING1)),
            ..plan()
        },
        "-",
    );
    let v_lost = ctl(
        w.as_mut(),
        &outdir,
        resume("br-before-lost", "before"),
        &plan(),
        "-",
    );
    group(
        "branch: snapshot before the reaction, the host sends it again",
        Some(&straight["react200"]),
        &[("base", &b1), ("resumed", &v_resent)],
    );
    group(
        "branch: snapshot before the reaction, nothing sent (the line is lost)",
        Some("6b9321d4b13e5d12"),
        &[("resumed", &v_lost)],
    );
    let b2 = ctl(
        w.as_mut(),
        &outdir,
        start("br-queued", S1),
        &Plan {
            snap: SnapAt::AfterQueued,
            ..p200
        },
        "queued",
    );
    let v_q1 = ctl(
        w.as_mut(),
        &outdir,
        resume("br-queued-1", "queued"),
        &plan(),
        "-",
    );
    let v_q2 = ctl(
        w.as_mut(),
        &outdir,
        resume("br-queued-2", "queued"),
        &plan(),
        "-",
    );
    group(
        "branch: snapshot with the reaction queued (carried in the snapshot)",
        Some(&straight["react200"]),
        &[("base", &b2), ("resumed", &v_q1), ("again", &v_q2)],
    );
    let p0 = Plan {
        react: Some((0, PING1)),
        snap: SnapAt::AfterTyped,
        ..plan()
    };
    let b3 = ctl(w.as_mut(), &outdir, start("br-typed", S1), &p0, "typed");
    let v_t1 = ctl(
        w.as_mut(),
        &outdir,
        resume("br-typed-1", "typed"),
        &plan(),
        "-",
    );
    let v_t2 = ctl(
        w.as_mut(),
        &outdir,
        resume("br-typed-2", "typed"),
        &plan(),
        "-",
    );
    group(
        "branch: snapshot right after typing (SNAP retype; RESUME types it again)",
        Some(&straight["react"]),
        &[("base", &b3), ("resumed", &v_t1), ("again", &v_t2)],
    );
    w.close().expect("close");
    std::process::exit(i32::from(failed));
}
