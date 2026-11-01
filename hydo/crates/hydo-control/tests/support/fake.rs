//! A scripted agent and engine: a [`Link`] whose guest answers every agent command
//! deterministically (like hydo-explore's `tests/fake_engine.rs` for the explorer). It reads the
//! host's lines strictly, as the real agent does (`HostLine::decode`), and keeps a small world:
//! files, processes under handles, the clock. Every answer is a pure function of the world's salt
//! (from the schedule), the command and the world's state; times are virtual (one unit per event)
//! and the engine stops every `cadence` units.
//!
//! The world is made to reach the rare paths of the shell init: a service that dies on its own,
//! one that never gets ready, a failed setup or provisioning, a knob whose `set` fails, a data
//! disk that does not mount, invariants that hold, fail or cannot be evaluated, a workload call
//! that times out, outcomes that failed or stayed unknown.
//!
//! For the features of M122 it also keeps a running realtime clock and the guest's uptime (one
//! unit of virtual time is 25 ms), refuses a later clock change now and then (and any change to
//! a time before 1970), and answers an `eventually` check (a run under `timeout`) the way a
//! system that settles does: it fails for the first few attempts, a few never succeed, some
//! attempts succeed only after the deadline, and some hang until the timeout kills them.

use hydo_agent_proto::frame::frame;
use hydo_agent_proto::line::{AgentLine, ErrorLine, HostLine};
use hydo_agent_proto::msg::*;
use hydo_control::{Ending, EngineAction, Link, LinkError, LinkNext, StopInfo, Why};
use std::collections::BTreeMap;

mod m123; // the processes, logs and reads of the feature controllers of M123

pub fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

pub fn hash(parts: &[&str]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for p in parts {
        for b in p.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h = mix(h ^ 0xff);
    }
    h
}

/// An agent line before it gets its number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Msg {
    Reply(u64, Reply),
    Refused(u64, String, ErrCode, String),
    Event(Event),
    Kernel(String),
}

/// What the fake delivered, by stop: the harness merges it with the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivered {
    pub n: u64,
    pub msg: Msg,
}

#[derive(Clone, Debug)]
struct Proc {
    spawn: u64,
    running: bool,
    dying: bool,
    stopped: bool,
    starts: u64,
}

#[derive(Clone, Debug, Default)]
struct World {
    files: BTreeMap<String, String>,
    procs: BTreeMap<String, Proc>,
    evals: BTreeMap<String, u64>,
    clock: i64,
    calls: u64,
    ext: m123::Ext,
    /// The virtual time at which the clock was last set, and how many times it was set.
    clock_t: u64,
    clock_sets: u64,
    /// The fake outside world (hydo-world), see `fake/world.rs`.
    fw: world::FakeWorld,
    /// qdiscs, the armed freeze (`fake/net.rs`)
    net: net::NetWorld,
}

// tc, ip, iptables, qlen and the packet trigger (M124)
mod net;
mod world;

#[derive(Clone, Debug)]
struct Pending {
    at: u64,
    ord: u64,
    msg: Msg,
}

/// The guest at a snapshot.
#[derive(Clone, Debug)]
pub struct VmState {
    t: u64,
    seq: u64,
    ord: u64,
    last_reply: u64,
    world: World,
    pending: Vec<Pending>,
}

pub struct Fake {
    pub t: u64,
    n: u64,
    cadence: u64,
    seq: u64,
    ord: u64,
    last_reply: u64,
    world: World,
    pending: Vec<Pending>,
    salt: String,
    first: Option<Why>,
    next_send: u64,
    pub delivered: Vec<Delivered>,
    pub snaps: BTreeMap<String, VmState>,
    pub engine_actions: Vec<(u64, EngineAction)>,
    pub ended: Option<Ending>,
    /// Refusals for lines the real agent would refuse (bad fields, unknown ops, old ids).
    pub bad_lines: Vec<String>,
    max_stops: u64,
}

impl Fake {
    /// A fresh boot: the hello (and a kernel line) come at the first units.
    pub fn boot(salt: &str, cadence: u64) -> Fake {
        let mut f = Fake {
            t: 0,
            n: 0,
            cadence,
            seq: 0,
            ord: 0,
            last_reply: 0,
            world: World::default(),
            pending: Vec::new(),
            salt: salt.into(),
            first: Some(Why::Start),
            next_send: 0,
            delivered: Vec::new(),
            snaps: BTreeMap::new(),
            engine_actions: Vec::new(),
            ended: None,
            bad_lines: Vec::new(),
            max_stops: 400_000,
        };
        f.emit(
            1,
            Msg::Kernel("[    0.000000] Linux version 6.6.35 (fake)".into()),
        );
        f.emit(
            2,
            Msg::Event(Event::Hello(Hello {
                proto: 1,
                agent: "0.1.0".into(),
                ops: Command::NAMES.iter().map(|s| s.to_string()).collect(),
                features: vec!["freeze".into()],
                pid1: true,
            })),
        );
        f.emit(3, Msg::Kernel("random: crng init done".into()));
        f
    }

    /// A conversation from a snapshot, with a new salt (the schedule's).
    pub fn resume(st: &VmState, salt: &str, cadence: u64) -> Fake {
        Fake {
            t: st.t,
            n: 0,
            cadence,
            seq: st.seq,
            ord: st.ord,
            last_reply: st.last_reply,
            world: st.world.clone(),
            pending: st.pending.clone(),
            salt: salt.into(),
            first: Some(Why::Resume),
            next_send: 0,
            delivered: Vec::new(),
            snaps: BTreeMap::new(),
            engine_actions: Vec::new(),
            ended: None,
            bad_lines: Vec::new(),
            max_stops: 400_000,
        }
    }

    fn h(&self, parts: &[&str]) -> u64 {
        let mut v = vec![self.salt.as_str()];
        v.extend_from_slice(parts);
        hash(&v)
    }

    fn emit(&mut self, at: u64, msg: Msg) {
        self.ord += 1;
        self.pending.push(Pending {
            at,
            ord: self.ord,
            msg,
        });
    }

    fn reply_at(&mut self, salt: u64) -> u64 {
        let at = (self.t + 1 + salt % 3).max(self.last_reply);
        self.last_reply = at;
        at
    }

    fn line_of(&mut self, m: &Msg) -> Vec<u8> {
        let l = match m {
            Msg::Kernel(t) => return t.clone().into_bytes(),
            Msg::Reply(re, body) => AgentLine::Reply {
                v: 1,
                seq: self.seq,
                re: *re,
                body: body.clone(),
            },
            Msg::Refused(re, op, err, msg) => AgentLine::Error(ErrorLine {
                v: 1,
                seq: self.seq,
                re: Some(*re),
                op: Some(op.clone()),
                err: err.as_str().into(),
                msg: msg.clone(),
                errno: None,
            }),
            Msg::Event(e) => AgentLine::Event {
                v: 1,
                seq: self.seq,
                ev: e.clone(),
            },
        };
        self.seq += 1;
        frame(&l.encode()).trim_end().as_bytes().to_vec()
    }

    fn exit_ev(h: &str, id: u64, status: u32, sig: Option<u32>) -> Msg {
        Msg::Event(Event::Exit(Exit {
            h: h.into(),
            id,
            status,
            sig,
            timed_out: None,
        }))
    }

    fn env_of(env: &Option<Vec<String>>, k: &str) -> Option<String> {
        env.as_ref()?.iter().find_map(|e| {
            e.strip_prefix(k)
                .and_then(|r| r.strip_prefix('='))
                .map(str::to_string)
        })
    }

    fn any_down(&self) -> bool {
        self.world
            .procs
            .iter()
            .filter(|(h, _)| {
                !h.contains(':') && !["workload", "retry", "provision"].contains(&h.as_str())
            })
            .any(|(_, p)| !p.running || p.dying || p.stopped)
    }

    /// The workload's effect: ids [from, to) get outcomes, the ok ones are acknowledged.
    fn workload(&mut self, from: u64, to: u64, retry: bool) {
        let acked = "/tmp/hydo/acked".to_string();
        let outc = "/tmp/hydo/outcomes".to_string();
        let down = self.any_down();
        let ids: Vec<u64> = if retry {
            // the ids whose last outcome is not ok
            let mut last: BTreeMap<u64, String> = BTreeMap::new();
            for l in self
                .world
                .files
                .get(&outc)
                .cloned()
                .unwrap_or_default()
                .lines()
            {
                let mut w = l.split(' ');
                if let (Some(i), Some(o)) = (w.next(), w.next()) {
                    if let Ok(i) = i.parse() {
                        last.insert(i, o.to_string());
                    }
                }
            }
            last.into_iter()
                .filter(|(_, o)| o != "ok")
                .map(|(i, _)| i)
                .collect()
        } else {
            (from..to).collect()
        };
        for i in ids {
            let r = self.h(&["outcome", &i.to_string(), &retry.to_string()]) % 10;
            let o = if down && r < 6 {
                "failed"
            } else if r == 0 {
                "unknown"
            } else if r == 1 && !retry {
                "failed"
            } else {
                "ok"
            };
            self.world
                .files
                .entry(outc.clone())
                .or_default()
                .push_str(&format!("{i} {o}\n"));
            if o == "ok" {
                self.world
                    .files
                    .entry(acked.clone())
                    .or_default()
                    .push_str(&format!("{i}\n"));
            }
        }
    }

    fn command(&mut self, bytes: &[u8]) {
        let bytes = bytes.strip_prefix(&[0x18u8][..]).unwrap_or(bytes);
        let text = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let hl = match HostLine::decode(text, &[1]) {
            Ok(h) => h,
            Err(r) => {
                self.bad_lines.push(format!(
                    "{} {}: {}",
                    r.err.as_str(),
                    r.msg,
                    String::from_utf8_lossy(text)
                ));
                return;
            }
        };
        let id = hl.id;
        let op = hl.cmd.name().to_string();
        let salt = self.h(&[&id.to_string(), &op]);
        let at = self.reply_at(salt);
        let t = self.t;
        let refuse = |f: &mut Fake, code: ErrCode, msg: &str| {
            f.emit(at, Msg::Refused(id, op.clone(), code, msg.into()));
        };
        match hl.cmd {
            Command::Sync(s) => {
                self.emit(at, Msg::Reply(id, Reply::Sync(SyncOk { nonce: s.nonce })))
            }
            Command::Mkdir(_) => self.emit(at, Msg::Reply(id, Reply::Mkdir(MkdirOk {}))),
            Command::Write(w) => {
                let e = self.world.files.entry(w.path).or_default();
                if w.append != Some(true) {
                    e.clear();
                }
                e.push_str(&w.data);
                self.emit(
                    at,
                    Msg::Reply(
                        id,
                        Reply::Write(WriteOk {
                            bytes: w.data.len() as u64,
                        }),
                    ),
                );
            }
            Command::Read(r) => match self.world.files.get(&r.path).cloned() {
                None => refuse(self, ErrCode::OsError, "open: ENOENT (errno 2)"),
                Some(c) => match Fake::m123_read(&c, &r) {
                    Ok(ok) => self.emit(at, Msg::Reply(id, Reply::Read(ok))),
                    Err((code, msg)) => refuse(self, code, &msg),
                },
            },
            Command::Mount(_) => {
                if self.h(&["mount"]).is_multiple_of(61) {
                    refuse(self, ErrCode::OsError, "mount: EINVAL (errno 22)");
                } else {
                    self.emit(at, Msg::Reply(id, Reply::Mount(MountOk {})));
                }
            }
            Command::Syncfs(_) => self.emit(at, Msg::Reply(id, Reply::Syncfs(SyncfsOk {}))),
            Command::ClockSet(c) => {
                let want = c.sec.unwrap_or(self.realtime() + c.step.unwrap_or(0));
                if want < 0 {
                    return refuse(self, ErrCode::OsError, "clock_settime: EINVAL (errno 22)");
                }
                // the first change of a boot always works; a later one now and then not
                if self.world.clock_sets > 0
                    && self.h(&["clockset", &id.to_string()]).is_multiple_of(11)
                {
                    return refuse(self, ErrCode::OsError, "clock_settime: EPERM (errno 1)");
                }
                self.world.clock = want;
                self.world.clock_t = t;
                self.world.clock_sets += 1;
                self.emit(
                    at,
                    Msg::Reply(id, Reply::ClockSet(ClockSetOk { sec: want })),
                );
            }
            Command::ClockGet(_) => {
                let ms = t * 25;
                self.emit(
                    at,
                    Msg::Reply(
                        id,
                        Reply::ClockGet(ClockGetOk {
                            sec: self.realtime(),
                            nsec: ((ms % 1000) * 1_000_000) as u32,
                            mono_ms: ms,
                        }),
                    ),
                );
            }
            Command::Run(r) => {
                self.emit(at, Msg::Reply(id, Reply::Run(RunOk {})));
                let a1 = r.argv.get(1).cloned().unwrap_or_default();
                let script = r.argv.get(3).cloned().unwrap_or_default();
                let (dur, status, out) = if r.argv.first().is_some_and(|a| a == world::BIN) {
                    self.world_ctl(&r.argv)
                } else if let Some(x) = self.net_run(id, &r) {
                    x
                } else if let Some(x) = self.m123_run(&r) {
                    x
                } else if a1 == "sleep" {
                    let ms: f64 = r.argv.get(2).and_then(|x| x.parse().ok()).unwrap_or(1.0);
                    ((ms * 40.0) as u64 + 3, 0, String::new())
                } else if a1 == "pgrep" {
                    let busy = ["workload", "retry"].iter().any(|h| {
                        self.world
                            .procs
                            .get(*h)
                            .is_some_and(|p| p.running && !p.dying)
                    });
                    (3, if busy { 0 } else { 1 }, String::new())
                } else if a1 == "timeout" {
                    self.live_attempt(&r.argv)
                } else if a1 == "ifconfig" || a1 == "ip" || script.contains(".log.old") {
                    (2, 0, String::new())
                } else if Self::env_of(&r.env, "HYDO_VALUE").is_some() {
                    let bad = self.h(&["knob", &script]).is_multiple_of(37);
                    (4, if bad { 1 } else { 0 }, String::new())
                } else {
                    // an invariant: its k-th evaluation
                    let k = self.world.evals.entry(script.clone()).or_default();
                    *k += 1;
                    let k = *k;
                    let r = self.h(&["inv", &script, &k.to_string()]) % 100;
                    let rc = if r < 4 {
                        2
                    } else if r < 16 {
                        1
                    } else {
                        0
                    };
                    let form = self.h(&["form", &script, &rc.to_string()]) % 4;
                    let tag = self.h(&["tag", &script]) % 1000;
                    let out = match (rc, form) {
                        (0, 0) => format!("checked {tag} keys\nHYDO-ORACLE: OK {tag} keys hold\n"),
                        (0, 1) => format!("HYDO-ASSERT {{\"x\":1}}\nall {tag} fine\n\n"),
                        (0, _) => "HYDO-ORACLE: OK\n".to_string(),
                        (1, 0) => format!("HYDO-ORACLE: VIOLATION lost key k{tag}\n"),
                        (1, 1) => format!("missing \"k{tag}\"\tback\\slash\n   \n"),
                        (1, _) => format!(
                            "noise\nHYDO-ORACLE:   VIOLATION   {tag} wrong HYDO-ORACLE: OK inner\n"
                        ),
                        (_, 0) => "cannot connect\n".to_string(),
                        (_, _) => String::new(),
                    };
                    (5, rc, out)
                };
                let kept: String = out
                    .chars()
                    .take(r.capture.unwrap_or(2048) as usize)
                    .collect();
                self.emit(
                    t + 2 + dur,
                    Msg::Event(Event::Done(Done {
                        id,
                        status,
                        sig: None,
                        timed_out: None,
                        out: kept,
                        out_len: out.len() as u64,
                    })),
                );
            }
            Command::Probe(p) => {
                self.emit(at, Msg::Reply(id, Reply::Probe(ProbeOk {})));
                let alive = p
                    .alive
                    .as_ref()
                    .map(|h| {
                        self.world
                            .procs
                            .get(h)
                            .is_some_and(|x| x.running && !x.dying)
                    })
                    .unwrap_or(true);
                let r = self.h(&[
                    "probe",
                    &format!("{:?}", p.argv),
                    &self.world.calls.to_string(),
                ]);
                self.world.calls += 1;
                let max_tries = (p.deadline_ms / p.every_ms.max(1)).max(1);
                let (ok, why, tries) = if !alive {
                    (false, ProbeEnd::Died, 1)
                } else if r.is_multiple_of(53) {
                    (false, ProbeEnd::Deadline, max_tries)
                } else {
                    (true, ProbeEnd::Ok, 1 + r % 3)
                };
                self.emit(
                    t + 3 + 4 * tries,
                    Msg::Event(Event::Probe(ProbeDone {
                        id,
                        ok,
                        why,
                        tries,
                        status: Some(if ok { 0 } else { 1 }),
                    })),
                );
            }
            Command::Spawn(s) => {
                if self.world.procs.get(&s.h).is_some_and(|p| p.running) {
                    return refuse(self, ErrCode::HandleBusy, "still running");
                }
                let starts = self
                    .world
                    .procs
                    .get(&s.h)
                    .map(|p| p.starts + 1)
                    .unwrap_or(1);
                self.world.procs.insert(
                    s.h.clone(),
                    Proc {
                        spawn: id,
                        running: true,
                        dying: false,
                        stopped: false,
                        starts,
                    },
                );
                self.emit(at, Msg::Reply(id, Reply::Spawn(SpawnOk {})));
                if self.m123_spawn(&s, id, t) {
                    return;
                }
                let h = s.h.clone();
                let from = Self::env_of(&s.env, "HYDO_FROM").and_then(|x| x.parse().ok());
                let to = Self::env_of(&s.env, "HYDO_TO").and_then(|x| x.parse().ok());
                if h.starts_with("fakes:") {
                    self.world_spawn(&h, id, &s);
                } else if h == "workload" || h == "retry" {
                    let (f, t2) = (from.unwrap_or(0u64), to.unwrap_or(0u64));
                    let r = self.h(&[&h, &f.to_string()]) % 29;
                    let status = match r {
                        0 => 143,
                        1 => 1,
                        _ => 0,
                    };
                    self.workload(f, t2, h == "retry");
                    let dur = 10 + 30 * t2.saturating_sub(f);
                    self.emit(t + dur, Self::exit_ev(&h, id, status, None));
                } else if h.starts_with("setup:") || h == "provision" {
                    let bad = self.h(&[&h, "fails"]).is_multiple_of(41);
                    self.emit(t + 6, Self::exit_ev(&h, id, if bad { 1 } else { 0 }, None));
                } else if self
                    .h(&[&h, "crash", &starts.to_string()])
                    .is_multiple_of(23)
                {
                    // a service that dies on its own a little later
                    self.emit(t + 40, Self::exit_ev(&h, id, 1, None));
                }
            }
            Command::Signal(sg) => {
                let Some(p) = self.world.procs.get(&sg.h).cloned() else {
                    return refuse(self, ErrCode::NoSuchHandle, "no such handle");
                };
                if !p.running {
                    return refuse(self, ErrCode::NotRunning, "not running");
                }
                match sg.sig {
                    Sig::Kill => self.finish(&sg.h, p.spawn, t + 2, 137, Some(9)),
                    Sig::Stop => self.world.procs.get_mut(&sg.h).unwrap().stopped = true,
                    Sig::Cont => self.world.procs.get_mut(&sg.h).unwrap().stopped = false,
                    _ => {}
                }
                self.emit(
                    at,
                    Msg::Reply(
                        id,
                        Reply::Signal(SignalOk {
                            procs: 1,
                            stopped: None,
                        }),
                    ),
                );
            }
            Command::Qlen(q) => {
                let packets = self.qlen(&q.dev);
                self.emit(at, Msg::Reply(id, Reply::Qlen(QlenOk { packets })));
            }
            Command::Freeze(f) => self.freeze(id, at, f),
            Command::Unfreeze(_) => self.unfreeze(id, at),
            other => refuse(
                self,
                ErrCode::UnknownOp,
                &format!("the fake does not do {}", other.name()),
            ),
        }
    }

    /// The realtime clock now: the value it was set to, plus the virtual time since.
    fn realtime(&self) -> i64 {
        self.world.clock + (self.t.saturating_sub(self.world.clock_t) * 25 / 1000) as i64
    }

    /// One attempt of an `eventually` check (`$BB timeout <within+1> $BB sh -c "<check>"`):
    /// (duration, exit status, output). A check needs a few attempts before it holds; one in
    /// seven never holds; an attempt now and then succeeds only after the deadline or hangs
    /// until busybox timeout ends it (status 143).
    fn live_attempt(&mut self, argv: &[String]) -> (u64, u32, String) {
        let limit: u64 = argv.get(2).and_then(|x| x.parse().ok()).unwrap_or(1);
        let script = argv.get(6).cloned().unwrap_or_default();
        let k = self
            .world
            .evals
            .entry(format!("live:{script}"))
            .or_default();
        *k += 1;
        let k = *k;
        let need = self.h(&["need", &script]) % 7;
        let r = self.h(&["live", &script, &k.to_string()]);
        let ok = need != 6 && k > need;
        let mut dur = 4 + r % 30;
        let mut status = if ok {
            0
        } else if r.is_multiple_of(5) {
            2
        } else {
            1
        };
        if ok && self.h(&["late", &script, &k.to_string()]).is_multiple_of(9) {
            dur = (limit - 1) * 40 + 20;
        }
        if r.is_multiple_of(31) {
            dur = limit * 40 + 100;
        }
        if dur > limit * 40 {
            dur = limit * 40;
            status = 143;
        }
        let pending = if status == 0 { 0 } else { k % 5 + 1 };
        let out = match self.h(&["lform", &script, &status.to_string()]) % 4 {
            0 => format!("pending={pending}\n"),
            1 => format!("checked \"k\"\tpending={pending}"),
            2 if status == 0 => "HYDO-ORACLE: OK all resolved\n".to_string(),
            2 => format!("HYDO-ORACLE: VIOLATION {pending} pending\n"),
            _ => String::new(),
        };
        (dur, status, out)
    }

    /// The process under `h` (spawn `id`) exits at `at`: it is dying from now (no longer alive
    /// for a probe or pgrep), and it is reaped -- its handle free again -- when its exit line is
    /// written, as in the real agent.
    fn finish(&mut self, h: &str, id: u64, at: u64, status: u32, sig: Option<u32>) {
        if let Some(p) = self.world.procs.get_mut(h) {
            if p.spawn == id {
                if p.dying {
                    return;
                }
                p.dying = true;
            }
        }
        // a process already on its way out (a crash or the end of a call) exits once
        let exiting = self
            .pending
            .iter()
            .any(|p| matches!(&p.msg, Msg::Event(Event::Exit(e)) if e.h == h && e.id == id));
        if !exiting {
            self.emit(at.max(self.t + 1), Self::exit_ev(h, id, status, sig));
        }
    }

    fn reap(&mut self, m: &Msg) {
        self.net_deliver(m);
        if let Msg::Event(Event::Exit(e)) = m {
            if let Some(p) = self.world.procs.get_mut(&e.h) {
                if p.spawn == e.id {
                    p.running = false;
                }
            }
        }
    }

    pub fn state(&self) -> VmState {
        VmState {
            t: self.t,
            seq: self.seq,
            ord: self.ord,
            last_reply: self.last_reply,
            world: self.world.clone(),
            pending: self.pending.clone(),
        }
    }
}

impl Link for Fake {
    fn next_stop(&mut self) -> Result<LinkNext, LinkError> {
        let why = match self.first.take() {
            Some(w) => w,
            None => {
                self.t += self.cadence;
                Why::Cadence
            }
        };
        self.n += 1;
        if self.n > self.max_stops {
            return Err(LinkError(format!(
                "the fake stopped {} times: a hang",
                self.n
            )));
        }
        self.pending.sort_by_key(|p| (p.at, p.ord));
        let k = self.pending.iter().take_while(|p| p.at <= self.t).count();
        let due: Vec<Pending> = self.pending.drain(..k).collect();
        let mut lines = Vec::new();
        for p in due {
            self.reap(&p.msg);
            lines.push(self.line_of(&p.msg));
            self.delivered.push(Delivered {
                n: self.n,
                msg: p.msg,
            });
        }
        Ok(LinkNext::Stop {
            stop: StopInfo {
                n: self.n,
                at: self.t,
                why,
                seen: 0,
            },
            lines,
        })
    }

    fn send(&mut self, at: u64, bytes: &[u8]) -> Result<u64, LinkError> {
        if at > self.t {
            return Err(LinkError("the fake types every send at once".into()));
        }
        self.next_send += 1;
        self.command(bytes);
        Ok(self.next_send)
    }

    fn ack(&mut self, send: u64) -> Result<(), LinkError> {
        if send == 0 || send > self.next_send {
            return Err(LinkError(format!("ack of an unknown send {send}")));
        }
        Ok(())
    }

    fn snap(&mut self, name: &str) -> Result<(), LinkError> {
        let s = self.state();
        self.snaps.insert(name.into(), s);
        Ok(())
    }

    fn engine(&mut self, a: &EngineAction) -> Result<(), LinkError> {
        self.engine_actions.push((self.n, a.clone()));
        Ok(())
    }

    fn cont(&mut self) -> Result<(), LinkError> {
        Ok(())
    }

    fn end(&mut self, e: &Ending) -> Result<String, LinkError> {
        self.ended = Some(e.clone());
        Ok(format!("RESULT fake at={} stops={}", self.t, self.n))
    }
}
