//! The controlled boot of an agent image (M121), end to end on either engine, without the
//! controller crate: a small scripted host drives hydo-agent through `hydo-agent-proto`'s host
//! session over the engines' lock-step protocol ([`hydo_engine::lockstep`]).
//!
//! ```text
//! agent_boot_check hvx <hvx_explore> <hvx-run|-> <bzImage> <initramfs> <agent disk.img> <outdir> [rb]
//! agent_boot_check hvm <hvmx> <Image> <agent .cpio.gz> <outdir> [rb]
//! ```
//!
//! The image is `examples/redis` built with `hydo build --control agent`. The host:
//! - boots it under control (`Engine::start_controlled`), waits for the agent's hello, takes
//!   `SNAP S0` there and resynchronises (`sync`);
//! - provisions: waits for the data disk, mounts it, brings `lo` up, makes `/data/redis`, spawns
//!   Redis, probes it until it answers, `syncfs`; takes `SNAP S1` there and ends the boot;
//! - from S1 (a worker's CTL run, twice) resynchronises and runs the workload: `redis-cli SET`,
//!   `redis-cli GET`, reads `/etc/redis.conf`, sets the realtime clock and reads it back, and
//!   ends with a verdict built from the answers;
//! - checks S0 by a RESUME from it that provisions again and ends at the S1 point (it must give
//!   the boot's own fingerprint), and checks the fresh boot by `Engine::one_controlled` twice (the
//!   same host from the first instruction; its SNAP S1 starts the workload): every workload run must
//!   give one fingerprint, stop count and input hash.
//!
//! The host is a pure function of the events it saw (its state is cloned at S0 and S1 for the
//! continuations), so each run sends the same commands at the same stops. Transcripts go to
//! `<outdir>/<run>.txt`. Not a cargo test: it needs an engine and the image. Exit status 0 when
//! every comparison holds.

use hydo_agent_proto::frame::{deframe_line, Deframed};
use hydo_agent_proto::msg::{ClockGet, ClockSet, Mkdir, Mount, Probe, Read, Run, Spawn, Syncfs};
use hydo_agent_proto::{Command, Event, Incoming, Reply, Session};
use hydo_engine::hvm::Hvm;
use hydo_engine::hvx::Hvx;
use hydo_engine::lockstep::{Lockstep, LockstepStart, Next, SendAnswer, SnapAnswer, StopWhy};
use hydo_engine::{BootSpec, ControlledEngine, CpuLevel, RunResult, SnapshotStore};
use hydo_schedule::Schedule;
use std::path::{Path, PathBuf};

/// One step of the host's script (a dozen of them; their size does not matter).
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
enum Step {
    /// Wait for the agent's hello.
    Hello,
    /// SNAP <name> here (every typed send is acknowledged by then).
    Snap(&'static str),
    /// The boot session ends here (serve: after SNAP S1).
    EndBoot,
    /// A command; the step is done when its answer (and, for run/probe, its event) came.
    Cmd(&'static str, Command),
    /// The resynchronisation after a snapshot point.
    Sync(u64),
    /// End the run with the verdict built from the answers.
    End,
}

fn env() -> Option<Vec<String>> {
    Some(vec!["PATH=/bin:/sbin".into(), "HOME=/".into()])
}

fn run(argv: &[&str]) -> Command {
    Command::Run(Run {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: env(),
        kill_after_ms: Some(10_000),
        ..Default::default()
    })
}

fn probe(argv: &[&str], every_ms: u64, deadline_ms: u64, alive: Option<&str>) -> Command {
    Command::Probe(Probe {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        env: env(),
        every_ms,
        deadline_ms,
        alive: alive.map(str::to_string),
        ..Default::default()
    })
}

/// The whole script; `serve_boot` ends the boot right after SNAP S1 (the serve worker's boot).
fn script(serve_boot: bool) -> Vec<Step> {
    let mut s = vec![
        Step::Hello,
        Step::Snap("S0"),
        Step::Sync(0),
        Step::Cmd(
            "disk",
            probe(&["/bin/test", "-b", "/dev/vda2"], 10, 10_000, None),
        ),
        Step::Cmd(
            "mount",
            Command::Mount(Mount {
                src: "/dev/vda2".into(),
                dst: "/data".into(),
                fstype: "ext4".into(),
                ..Default::default()
            }),
        ),
        Step::Cmd("lo", run(&["/bin/ip", "link", "set", "lo", "up"])),
        Step::Cmd(
            "mkdir",
            Command::Mkdir(Mkdir {
                path: "/data/redis".into(),
                mode: None,
            }),
        ),
        Step::Cmd(
            "spawn",
            Command::Spawn(Spawn {
                h: "redis".into(),
                argv: vec!["/bin/redis-server".into(), "/etc/redis.conf".into()],
                env: env(),
                stdout: Some("/tmp/redis.log".into()),
                stderr: Some("/tmp/redis.log".into()),
                ..Default::default()
            }),
        ),
        Step::Cmd(
            "ready",
            probe(
                &["/bin/sh", "-c", "/bin/redis-cli ping | grep -q PONG"],
                20,
                20_000,
                Some("redis"),
            ),
        ),
        Step::Cmd("syncfs", Command::Syncfs(Syncfs {})),
        Step::Snap("S1"),
    ];
    if serve_boot {
        s.push(Step::EndBoot);
        return s;
    }
    s.extend([
        Step::Sync(1),
        Step::Cmd("set", run(&["/bin/redis-cli", "SET", "k:1", "v1"])),
        Step::Cmd("get", run(&["/bin/redis-cli", "GET", "k:1"])),
        Step::Cmd(
            "conf",
            Command::Read(Read {
                path: "/etc/redis.conf".into(),
                max: 40,
                ..Default::default()
            }),
        ),
        // the realtime clock (the T= fault's primitive): set it, then read it back
        Step::Cmd(
            "clock_set",
            Command::ClockSet(ClockSet {
                sec: Some(1_800_000_000),
                step: None,
            }),
        ),
        Step::Cmd("clock_get", Command::ClockGet(ClockGet {})),
        Step::End,
    ]);
    s
}

/// The host's state: a pure function of the events seen so far.
#[derive(Clone)]
struct Host {
    steps: Vec<Step>,
    at: usize,
    sess: Option<Session>,
    /// the command in flight: (step name, agent id, engine send id, answered, event seen)
    wait: Option<(String, u64, u64, bool, bool)>,
    /// engine send ids whose agent answer came and that are not ACKed yet
    to_ack: Vec<u64>,
    /// what the answers said, for the verdict: (step, text)
    notes: Vec<(String, String)>,
    failed: Option<String>,
    stops_waiting: u64,
    /// the icount (hvx) or exit count (hvm) of the current stop
    now: u64,
}

/// What a run of the host ended with.
enum Outcome {
    /// The boot ended at EndBoot (serve) or the run at End: the RESULT.
    Done(RunResult),
    /// Stopped right after SNAP <name> (the caller clones the host for continuations).
    Snapped(&'static str, Box<Host>),
}

impl Host {
    fn new(serve_boot: bool) -> Host {
        Host {
            steps: script(serve_boot),
            at: 0,
            sess: None,
            wait: None,
            to_ack: Vec::new(),
            notes: Vec::new(),
            failed: None,
            stops_waiting: 0,
            now: 0,
        }
    }

    /// The script after S1 for a continuation from the serve boot's S1 (it ended there).
    fn after_s1(mut self) -> Host {
        self.steps = script(false);
        self
    }

    fn verdict(&self) -> String {
        if let Some(f) = &self.failed {
            return format!("AGENT-E2E: ERROR {f}");
        }
        let n: Vec<String> = self.notes.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("AGENT-E2E: OK {}", n.join(" "))
    }

    fn note(&mut self, step: &str, text: &str) {
        let t: String = text
            .trim()
            .chars()
            .map(|c| if c.is_whitespace() { '_' } else { c })
            .take(40)
            .collect();
        self.notes.push((step.to_string(), t));
    }

    /// One console line.
    fn line(&mut self, text: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        let Some(sess) = self.sess.as_mut() else {
            match deframe_line(text) {
                Deframed::Agent(json) => match Session::from_hello(&json) {
                    Ok(s) => self.sess = Some(s),
                    Err(e) => self.failed = Some(format!("hello: {e}")),
                },
                Deframed::Corrupt { line, why } => {
                    self.failed = Some(format!("corrupt line before the hello ({why}): {line}"))
                }
                Deframed::Other(_) => {}
            }
            return;
        };
        let inc = match sess.console_line(text) {
            None => return,
            Some(Err(e)) => {
                self.failed = Some(e.to_string());
                return;
            }
            Some(Ok(i)) => i,
        };
        let Some((name, id, send, answered, evented)) = self.wait.clone() else {
            if !matches!(inc, Incoming::Event(Event::Exit(_))) {
                self.failed = Some(format!("an agent line with nothing in flight: {inc:?}"));
            }
            return;
        };
        let mut answered = answered;
        let mut evented = evented;
        match inc {
            Incoming::Reply { id: re, body } if re == id => {
                answered = true;
                self.to_ack.push(send);
                match body {
                    Reply::Read(r) => self.note(&name, r.data.lines().next().unwrap_or("")),
                    Reply::Sync(s) => self.note(&name, &s.nonce.to_string()),
                    Reply::ClockSet(c) => self.note(&name, &c.sec.to_string()),
                    Reply::ClockGet(c) => self.note(&name, &format!("{}.{:09}", c.sec, c.nsec)),
                    Reply::Run(_) | Reply::Probe(_) => {}
                    other => self.note(&name, other.name()),
                }
            }
            Incoming::Refused { error, .. } => {
                self.failed = Some(format!("{name} refused: {error:?}"));
                return;
            }
            Incoming::Event(Event::Done(d)) if d.id == id => {
                evented = true;
                if d.status != 0 {
                    self.failed = Some(format!("{name} exited {}: {}", d.status, d.out.trim()));
                    return;
                }
                if name != "lo" {
                    self.note(&name, &d.out);
                }
            }
            Incoming::Event(Event::Probe(p)) if p.id == id => {
                evented = true;
                if !p.ok {
                    self.failed = Some(format!(
                        "{name}: probe ended {:?} after {} tries",
                        p.why, p.tries
                    ));
                    return;
                }
            }
            // a CAN that cut a partial line (none is expected: every snapshot point is idle)
            Incoming::Event(Event::Dropped(_)) => {}
            Incoming::Event(Event::Exit(e)) => {
                self.failed = Some(format!(
                    "{} exited (status {}) during {name}",
                    e.h, e.status
                ));
                return;
            }
            other => {
                self.failed = Some(format!("unexpected during {name}: {other:?}"));
                return;
            }
        }
        let needs_event = matches!(
            self.steps.get(self.at),
            Some(Step::Cmd(_, Command::Run(_) | Command::Probe(_)))
        );
        if answered && (evented || !needs_event) {
            self.wait = None;
            self.at += 1;
            self.stops_waiting = 0;
        } else {
            self.wait = Some((name, id, send, answered, evented));
        }
    }

    fn send(&mut self, s: &mut Lockstep<'_>, at: u64, name: &str, cmd: Command, sync: Option<u64>) {
        let sess = self.sess.as_mut().expect("a session after the hello");
        let out = match sync {
            Some(n) => sess.sync(n),
            None => sess.command(cmd),
        };
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                self.failed = Some(format!("{name}: {e}"));
                return;
            }
        };
        let sent = match s.send(at, &out.bytes).expect("SEND") {
            SendAnswer::Typed { id, .. } => id,
            SendAnswer::Queued { id, .. } => id,
        };
        self.wait = Some((name.to_string(), out.id, sent, false, false));
    }

    /// Serve the stops until the run ends or a SNAP was taken (`stop_at_snap`).
    fn drive(&mut self, s: &mut Lockstep<'_>, stop_at_snap: bool) -> Outcome {
        loop {
            let rep = match s.next_stop().expect("next stop") {
                Next::Done(r) => return Outcome::Done(r),
                Next::Stop(rep) => rep,
            };
            self.now = rep.stop.at;
            if std::env::var_os("AGENT_CHECK_TIMES").is_some() && rep.stop.n % 1000 == 0 {
                eprintln!(
                    "{:8.2}s stop {} at={} {:?}",
                    t0().elapsed().as_secs_f64(),
                    rep.stop.n,
                    rep.stop.at,
                    rep.stop.phase
                );
            }
            for e in &rep.events {
                self.line(&e.text);
            }
            for id in std::mem::take(&mut self.to_ack) {
                if let Err(e) = s.ack(id).expect("ACK") {
                    self.failed = Some(format!("ack {id}: {e}"));
                }
            }
            if rep.stop.why == StopWhy::Exit {
                self.failed.get_or_insert("the guest stopped".into());
            }
            if self.failed.is_some() {
                return Outcome::Done(s.end(Some(&self.verdict())).expect("END"));
            }
            self.stops_waiting += 1;
            if self.stops_waiting > 500_000 {
                self.failed = Some(format!("step {} took more than 500000 stops", self.at));
                return Outcome::Done(s.end(Some(&self.verdict())).expect("END"));
            }
            // the steps that need no answer run at this stop; a command waits for its answer
            while self.wait.is_none() && self.failed.is_none() {
                let at = rep.stop.at;
                match self.steps.get(self.at).cloned() {
                    None | Some(Step::End) | Some(Step::EndBoot) => {
                        return Outcome::Done(s.end(Some(&self.verdict())).expect("END"))
                    }
                    Some(Step::Hello) => {
                        if self.sess.is_none() {
                            break;
                        }
                        self.at += 1;
                    }
                    Some(Step::Snap(name)) => {
                        match s.snap(name, false).expect("SNAP") {
                            SnapAnswer::Taken { .. } => {}
                            SnapAnswer::Refused(e) => {
                                self.failed = Some(format!("SNAP {name}: {e}"));
                                continue;
                            }
                        }
                        self.at += 1;
                        if stop_at_snap {
                            return Outcome::Snapped(name, Box::new(self.clone()));
                        }
                    }
                    Some(Step::Sync(n)) => {
                        self.send(s, at, "sync", Command::Syncfs(Syncfs {}), Some(n));
                    }
                    Some(Step::Cmd(name, cmd)) => self.send(s, at, name, cmd, None),
                }
            }
            s.cont().expect("CONT");
        }
    }

    /// Drive to the end, remembering the host at each snapshot (for the continuations).
    fn drive_all(
        mut self,
        s: &mut Lockstep<'_>,
        saved: &mut Vec<(&'static str, Host)>,
    ) -> RunResult {
        loop {
            match self.drive(s, true) {
                Outcome::Done(r) => return r,
                Outcome::Snapped(name, h) => {
                    saved.push((name, (*h).clone()));
                    self = *h;
                    // the same stop: the next steps (the sync) run before CONT
                    match self.drive_rest_of_stop(s) {
                        Some(r) => return r,
                        None => continue,
                    }
                }
            }
        }
    }

    /// After a SNAP inside a stop: run the remaining steps of that stop, then CONT.
    fn drive_rest_of_stop(&mut self, s: &mut Lockstep<'_>) -> Option<RunResult> {
        let at = self.now;
        while self.wait.is_none() && self.failed.is_none() {
            match self.steps.get(self.at).cloned() {
                None | Some(Step::End) | Some(Step::EndBoot) => {
                    return Some(s.end(Some(&self.verdict())).expect("END"))
                }
                Some(Step::Sync(n)) => {
                    self.send(s, at, "sync", Command::Syncfs(Syncfs {}), Some(n))
                }
                Some(Step::Cmd(name, cmd)) => self.send(s, at, name, cmd, None),
                Some(Step::Snap(_)) | Some(Step::Hello) => break,
            }
        }
        if self.failed.is_some() {
            return Some(s.end(Some(&self.verdict())).expect("END"));
        }
        s.cont().expect("CONT");
        None
    }
}

/// The process's start (for AGENT_CHECK_TIMES=1: host seconds every 1000 stops, on stderr).
fn t0() -> std::time::Instant {
    static T0: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *T0.get_or_init(std::time::Instant::now)
}

struct Check {
    engine: Box<dyn ControlledEngine>,
    spec: BootSpec,
    sched: Schedule,
    out: PathBuf,
    fails: usize,
}

fn log(out: &Path, name: &str) -> std::fs::File {
    std::fs::File::create(out.join(format!("{name}.txt"))).expect("transcript")
}

fn show(name: &str, r: &RunResult) {
    let mut l = r.line.clone();
    if let Some(i) = l.find(" secs=") {
        let j = l[i + 1..].find(' ').map(|k| i + 1 + k).unwrap_or(l.len());
        l.replace_range(i..j, "");
    }
    println!("{name:>8}: {l}");
}

fn key(r: &RunResult) -> (String, Option<String>, Option<String>, Option<String>) {
    (
        r.fp.clone(),
        r.extra("stops").map(str::to_string),
        r.extra("typed").map(str::to_string),
        r.extra("inh").map(str::to_string),
    )
}

impl Check {
    fn same(&mut self, what: &str, a: &RunResult, b: &RunResult) {
        let ok = key(a) == key(b) && a.verdict == b.verdict;
        println!("  {} {what}", if ok { "same:" } else { "DIFFERENT:" });
        if !ok {
            self.fails += 1;
        }
    }

    fn run(&mut self) {
        let err = self.out.join("engine.err");
        // 1. the serve worker's boot: S0 at the hello, S1 after provisioning, END there
        let mut boot = self
            .engine
            .start_controlled(&self.spec, &self.sched, &err)
            .expect("start_controlled");
        let mut saved = Vec::new();
        let rb = {
            let mut l = log(&self.out, "boot");
            let mut s = boot.session(Some(&mut l));
            Host::new(true).drive_all(&mut s, &mut saved)
        };
        show("boot", &rb);
        let mut w = boot.ready().expect("READY");
        let s0 = saved
            .iter()
            .find(|(n, _)| *n == "S0")
            .map(|(_, h)| h.clone());
        let s1 = saved
            .iter()
            .find(|(n, _)| *n == "S1")
            .map(|(_, h)| h.clone());
        let (Some(s0), Some(s1)) = (s0, s1) else {
            println!("  FAIL: the boot took no S0/S1 ({})", rb.verdict);
            self.fails += 1;
            return;
        };
        // 2. twice from S1 (CTL): resync, the workload, END
        let mut from_s1 = Vec::new();
        for tag in ["s1a", "s1b"] {
            let mut l = log(&self.out, tag);
            let start = LockstepStart::Run {
                tag: tag.into(),
                schedule: self.sched.clone(),
            };
            let mut s = w.lockstep(&start, Some(&mut l)).expect("CTL");
            let r = s1.clone().after_s1().drive_all(&mut s, &mut Vec::new());
            show(tag, &r);
            from_s1.push(r);
        }
        // 3. from S0 (RESUME): provision again, END at the S1 point = the boot's own end
        let r0 = {
            let mut l = log(&self.out, "s0");
            let start = LockstepStart::Resume {
                tag: "s0".into(),
                snapshot: "S0".into(),
            };
            let mut s = w.lockstep(&start, Some(&mut l)).expect("RESUME");
            let mut h = s0.clone();
            h.steps = script(true);
            // RESUME S1 is the boot's own end, so the continuation does not SNAP again
            h.steps.retain(|st| !matches!(st, Step::Snap("S1")));
            h.steps.insert(h.steps.len() - 1, Step::Snap("S1b"));
            h.drive_all(&mut s, &mut Vec::new())
        };
        show("s0", &r0);
        let _ = w.close();
        // 4. twice from a fresh boot: the same host from the first instruction
        let mut fresh = Vec::new();
        for tag in ["one1", "one2"] {
            let mut one = self
                .engine
                .one_controlled(&self.spec, &self.sched, &err)
                .expect("one_controlled");
            let r = {
                let mut l = log(&self.out, tag);
                let mut s = one.session(Some(&mut l));
                Host::new(false).drive_all(&mut s, &mut Vec::new())
            };
            let _ = one.wait();
            show(tag, &r);
            fresh.push(r);
        }
        println!("checks:");
        let (a, b) = (from_s1[0].clone(), from_s1[1].clone());
        self.same("S1 run a = S1 run b", &a, &b);
        self.same("S1 run a = fresh boot 1", &a, &fresh[0]);
        self.same("fresh boot 1 = fresh boot 2", &fresh[0], &fresh[1]);
        let ok_boot = r0.fp == rb.fp && r0.verdict == rb.verdict;
        println!(
            "  {} RESUME S0 to the S1 point = the boot's end (fp {} / {})",
            if ok_boot { "same:" } else { "DIFFERENT:" },
            r0.fp,
            rb.fp
        );
        if !ok_boot {
            self.fails += 1;
        }
        for r in from_s1.iter().chain(&fresh).chain([&rb, &r0]) {
            if !r.verdict.starts_with("AGENT-E2E: OK") {
                println!("  FAIL: {} verdict {}", r.tag, r.verdict);
                self.fails += 1;
            }
        }
    }
}

fn main() {
    t0();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: agent_boot_check hvx <hvx_explore> <hvx-run|-> <bzImage> <initramfs> <disk.img> <outdir> [rb]\n       agent_boot_check hvm <hvmx> <Image> <image .cpio.gz> <outdir> [rb]";
    let (engine, spec, rb): (Box<dyn ControlledEngine>, BootSpec, String) =
        match a.first().map(String::as_str) {
            Some("hvx") if a.len() >= 7 => {
                let out = PathBuf::from(&a[6]);
                std::fs::create_dir_all(out.join("snap")).expect("outdir");
                let e = Hvx {
                    engine: PathBuf::from(&a[1]),
                    hvx_run: (a[2] != "-").then(|| PathBuf::from(&a[2])),
                    extra_env: Vec::new(),
                    env: Vec::new(),
                    config: hydo_config::Env::process(),
                };
                let spec = BootSpec {
                    image: PathBuf::from(&a[5]),
                    ram_mib: 1024,
                    vcpus: 1,
                    cpu: CpuLevel::V2,
                    cpu_cache: false,
                    kernel: PathBuf::from(&a[3]),
                    initrd: Some(PathBuf::from(&a[4])),
                    outdir: out.join("consoles"),
                    snapshots: SnapshotStore::Memory,
                };
                (Box::new(e), spec, a.get(7).cloned().unwrap_or("16".into()))
            }
            Some("hvm") if a.len() >= 5 => {
                let out = PathBuf::from(&a[4]);
                std::fs::create_dir_all(out.join("snap")).expect("outdir");
                let e = Hvm {
                    hvmx: PathBuf::from(&a[1]),
                    env: vec![(
                        "HVM_SNAP_DIR".into(),
                        out.join("snap").display().to_string(),
                    )],
                    config: hydo_config::Env::process(),
                };
                let spec = BootSpec {
                    image: PathBuf::from(&a[3]),
                    ram_mib: 1024,
                    vcpus: 1,
                    cpu: CpuLevel::V2,
                    cpu_cache: false,
                    kernel: PathBuf::from(&a[2]),
                    initrd: None,
                    outdir: out.join("consoles"),
                    snapshots: SnapshotStore::Files,
                };
                (Box::new(e), spec, a.get(5).cloned().unwrap_or("64".into()))
            }
            _ => {
                eprintln!("{usage}");
                std::process::exit(2);
            }
        };
    match hydo_engine::image::control(&spec.image) {
        Ok(proto) => println!("image {}: agent protocol {proto}", spec.image.display()),
        Err(e @ hydo_engine::image::ImageError::ShellImage(_)) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
        Err(e) => eprintln!("warning: cannot read /etc/hydo/control: {e}"),
    }
    let out = spec.outdir.parent().expect("outdir").to_path_buf();
    std::fs::create_dir_all(&spec.outdir).expect("consoles dir");
    let sched = Schedule::parse(&format!("disk=none rb={rb} | run")).expect("schedule");
    let mut c = Check {
        engine,
        spec,
        sched,
        out,
        fails: 0,
    };
    c.run();
    if c.fails == 0 {
        println!("agent_boot_check: ALL PASS");
    } else {
        println!("agent_boot_check: {} FAILED", c.fails);
        std::process::exit(1);
    }
}
