//! The agent's server: read host lines, carry out the primitives, write replies and events.
//!
//! One thread, no timers and no randomness of its own. It sleeps in one poll(2) over the input,
//! a signalfd for SIGCHLD, an inotify instance (tails), the netfilter queue (an armed freeze) and
//! the output pipes of `run` commands. The only timeouts are deadlines the host's commands gave
//! (kill_after_ms, a probe's schedule and deadline, a freeze's duration). With nothing armed the
//! poll has no timeout, so an idle agent costs no exit and no guest time.
//!
//! Every round handles what is ready in a fixed order: the netfilter queue first (a freeze must
//! happen before the frozen service runs on), then due deadlines (by time, then kind, then id),
//! then the output pipes, then exited children (in the order waitpid gives them), then tails (by
//! name), then the host's input (its lines in order). No pid, wall time or host path appears in
//! what the agent writes.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read as _, Seek, SeekFrom, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command as Proc, Stdio};

use hydo_agent_proto::frame::{frame, CAN};
use hydo_agent_proto::msg::*;
use hydo_agent_proto::{AgentLine, ErrorLine, HostLine};

use crate::nfq::{self, Counter, Nfq};
use crate::{sys, tree};

/// The agent's version (in the hello).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const MAX_CAPTURE: u32 = 3000;
const DEFAULT_CAPTURE: u32 = 2048;
const MAX_READ: u32 = 3000;
const MAX_LINE: usize = 2000;

/// A refusal or failure of a command, before it is numbered.
struct Fail {
    err: ErrCode,
    msg: String,
    errno: Option<i64>,
}

fn fail(err: ErrCode, msg: impl Into<String>) -> Fail {
    Fail {
        err,
        msg: msg.into(),
        errno: None,
    }
}

fn os(what: &str, e: &std::io::Error) -> Fail {
    let n = sys::io_errno(e);
    Fail {
        err: ErrCode::OsError,
        msg: format!("{what}: {}", sys::errno_name(n)),
        errno: Some(n as i64),
    }
}

/// Who a child process belongs to.
#[derive(Clone, Debug)]
enum Owner {
    Spawn(String),
    Run(u64),
    Probe(u64),
}

struct Service {
    pid: i32,
    id: u64,
    running: bool,
    kill_at: Option<u64>,
    timed_out: bool,
}

struct RunState {
    pid: i32,
    pipe: Option<OwnedFd>,
    kept: Vec<u8>,
    cap: usize,
    total: u64,
    kill_at: Option<u64>,
    timed_out: bool,
}

struct ProbeState {
    spec: Probe,
    start: u64,
    deadline: u64,
    tries: u64,
    attempt: Option<i32>,
    next_at: Option<u64>,
    expired: bool,
    last: Option<u32>,
}

struct TailState {
    path: String,
    offset: u64,
    partial: Vec<u8>,
    cut: bool,
    lineno: u64,
}

struct FreezeState {
    id: u64,
    h: String,
    nfq: Nfq,
    counter: Counter,
    dur: u64,
    log: Option<File>,
    nlog: u32,
    fired: bool,
    resumed: bool,
    resume_at: u64,
    frozen: Vec<i32>,
}

/// Where the agent reads and writes, and how it maps paths.
pub struct Io {
    pub input: RawFd,
    pub output: RawFd,
    /// Paths the agent opens itself are taken under this directory (native tests).
    pub root: Option<PathBuf>,
    pub pid1: bool,
}

pub struct Agent {
    io: Io,
    seq: u64,
    inbuf: Vec<u8>,
    too_long: bool,
    last_id: Option<u64>,
    sigfd: RawFd,
    inotify: Option<RawFd>,
    services: BTreeMap<String, Service>,
    owners: BTreeMap<i32, Owner>,
    runs: BTreeMap<u64, RunState>,
    probes: BTreeMap<u64, ProbeState>,
    tails: BTreeMap<String, TailState>,
    freeze: Option<FreezeState>,
}

fn sig_num(s: Sig) -> i32 {
    match s {
        Sig::Kill => libc::SIGKILL,
        Sig::Term => libc::SIGTERM,
        Sig::Stop => libc::SIGSTOP,
        Sig::Cont => libc::SIGCONT,
        Sig::Int => libc::SIGINT,
        Sig::Hup => libc::SIGHUP,
        Sig::Quit => libc::SIGQUIT,
        Sig::Usr1 => libc::SIGUSR1,
        Sig::Usr2 => libc::SIGUSR2,
    }
}

fn mount_flag(f: MountFlag) -> libc::c_ulong {
    match f {
        MountFlag::Ro => libc::MS_RDONLY,
        MountFlag::Nosuid => libc::MS_NOSUID,
        MountFlag::Nodev => libc::MS_NODEV,
        MountFlag::Noexec => libc::MS_NOEXEC,
        MountFlag::Noatime => libc::MS_NOATIME,
        MountFlag::Relatime => libc::MS_RELATIME,
        MountFlag::Sync => libc::MS_SYNCHRONOUS,
        MountFlag::Remount => libc::MS_REMOUNT,
        MountFlag::Bind => libc::MS_BIND,
    }
}

/// What a child needs, from spawn, run and probe.
struct ChildSpec<'a> {
    argv: &'a [String],
    env: &'a Option<Vec<String>>,
    cwd: &'a Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    netns: &'a Option<String>,
}

impl Agent {
    pub fn new(io: Io) -> std::io::Result<Agent> {
        let sigfd = sys::sigchld_fd()?;
        sys::set_nonblock(io.input);
        Ok(Agent {
            io,
            seq: 0,
            inbuf: Vec::new(),
            too_long: false,
            last_id: None,
            sigfd,
            inotify: None,
            services: BTreeMap::new(),
            owners: BTreeMap::new(),
            runs: BTreeMap::new(),
            probes: BTreeMap::new(),
            tails: BTreeMap::new(),
            freeze: None,
        })
    }

    /// A path the agent opens itself, under the root of a native test.
    fn path(&self, p: &str) -> PathBuf {
        match &self.io.root {
            Some(r) if p.starts_with('/') => r.join(p.trim_start_matches('/')),
            Some(r) => r.join(p),
            None => PathBuf::from(p),
        }
    }

    fn path_str(&self, p: &str) -> String {
        self.path(p).to_string_lossy().into_owned()
    }

    // ------------------------------------------------------------------ output

    fn emit_line(&mut self, mut line: AgentLine) {
        match &mut line {
            AgentLine::Reply { seq, .. }
            | AgentLine::Event { seq, .. }
            | AgentLine::UnknownEvent { seq, .. } => *seq = self.seq,
            AgentLine::Error(e) => e.seq = self.seq,
        }
        self.seq += 1;
        let f = frame(&line.encode());
        let _ = sys::write_all(self.io.output, f.as_bytes());
    }

    fn event(&mut self, ev: Event) {
        self.emit_line(AgentLine::Event {
            v: PROTO,
            seq: 0,
            ev,
        });
    }

    fn reply(&mut self, re: u64, body: Reply) {
        self.emit_line(AgentLine::Reply {
            v: PROTO,
            seq: 0,
            re,
            body,
        });
    }

    fn refuse(&mut self, re: Option<u64>, op: Option<String>, f: Fail) {
        self.emit_line(AgentLine::Error(ErrorLine {
            v: PROTO,
            seq: 0,
            re,
            op,
            err: f.err.as_str().into(),
            msg: f.msg,
            errno: f.errno,
        }));
    }

    pub fn hello(&mut self) {
        let features = FEATURES.iter().map(|s| s.to_string()).collect();
        let ops = Command::NAMES.iter().map(|s| s.to_string()).collect();
        let pid1 = self.io.pid1;
        self.event(Event::Hello(Hello {
            proto: PROTO,
            agent: VERSION.into(),
            ops,
            features,
            pid1,
        }));
    }

    // ------------------------------------------------------------------ the loop

    /// Serves until the input ends (never, on a console).
    pub fn serve(&mut self) {
        loop {
            let now = sys::now_ms();
            let timeout = match self.next_deadline() {
                None => -1,
                Some(t) => t.saturating_sub(now).min(i32::MAX as u64) as i32,
            };
            let mut fds = vec![
                libc::pollfd {
                    fd: self.io.input,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.sigfd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            if let Some(i) = self.inotify {
                fds.push(libc::pollfd {
                    fd: i,
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            let nfq_at = self.freeze.as_ref().map(|f| {
                fds.push(libc::pollfd {
                    fd: f.nfq.fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
                fds.len() - 1
            });
            let pipes_from = fds.len();
            let pipe_ids: Vec<u64> = self
                .runs
                .iter()
                .filter(|(_, r)| r.pipe.is_some())
                .map(|(id, _)| *id)
                .collect();
            for id in &pipe_ids {
                let fd = self.runs[id]
                    .pipe
                    .as_ref()
                    .map(|p| p.as_raw_fd())
                    .unwrap_or(-1);
                fds.push(libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
            sys::poll(&mut fds, timeout);
            let ready = |i: usize| fds[i].revents != 0;
            if let Some(i) = nfq_at {
                if ready(i) {
                    self.on_packets();
                }
            }
            self.on_deadlines();
            for (k, id) in pipe_ids.iter().enumerate() {
                if ready(pipes_from + k) {
                    self.read_pipe(*id);
                }
            }
            if ready(1) {
                sys::drain_signalfd(self.sigfd);
            }
            self.reap();
            if self.inotify.is_some() && ready(2) {
                self.drain_inotify();
                self.check_tails();
            }
            if ready(0) && !self.read_input() {
                return;
            }
        }
    }

    fn next_deadline(&self) -> Option<u64> {
        let mut t: Option<u64> = None;
        let mut at = |x: Option<u64>| {
            if let Some(x) = x {
                t = Some(t.map_or(x, |t| t.min(x)));
            }
        };
        for s in self.services.values() {
            at(s.kill_at.filter(|_| s.running && !s.timed_out));
        }
        for r in self.runs.values() {
            at(r.kill_at.filter(|_| !r.timed_out));
        }
        for p in self.probes.values() {
            at(p.next_at);
            if !p.expired {
                at(Some(p.deadline));
            }
        }
        if let Some(f) = &self.freeze {
            at(Some(f.resume_at).filter(|_| f.fired && !f.resumed));
        }
        t
    }

    /// Reads the input; false at its end.
    fn read_input(&mut self) -> bool {
        let mut buf = [0u8; 4096];
        let n = match sys::read(self.io.input, &mut buf) {
            Ok(0) => return false,
            Ok(n) => n,
            Err(_) => return true,
        };
        for &b in &buf[..n] {
            match b {
                CAN => {
                    if !self.inbuf.is_empty() || self.too_long {
                        let bytes = self.inbuf.len() as u64;
                        self.inbuf.clear();
                        self.too_long = false;
                        self.event(Event::Dropped(Dropped { bytes }));
                    }
                }
                b'\n' => {
                    let line = std::mem::take(&mut self.inbuf);
                    if std::mem::take(&mut self.too_long) {
                        self.refuse(
                            None,
                            None,
                            fail(
                                ErrCode::TooLong,
                                format!(
                                    "an input line longer than {MAX_HOST_LINE} bytes was discarded"
                                ),
                            ),
                        );
                    } else if !line.iter().all(|c| matches!(c, b' ' | b'\t' | b'\r')) {
                        self.on_line(&line);
                    }
                }
                _ if self.too_long => {}
                _ => {
                    self.inbuf.push(b);
                    if self.inbuf.len() > MAX_HOST_LINE {
                        self.inbuf.clear();
                        self.too_long = true;
                    }
                }
            }
        }
        true
    }

    fn on_line(&mut self, line: &[u8]) {
        let h = match HostLine::decode(line, &[PROTO]) {
            Ok(h) => h,
            Err(r) => {
                if let Some(id) = r.re {
                    if self.last_id.is_some_and(|l| id <= l) {
                        return self.refuse(Some(id), r.op, self.dup(id));
                    }
                    self.last_id = Some(id);
                }
                return self.refuse(
                    r.re,
                    r.op,
                    Fail {
                        err: r.err,
                        msg: r.msg,
                        errno: None,
                    },
                );
            }
        };
        let (id, op) = (h.id, h.cmd.name());
        if self.last_id.is_some_and(|l| id <= l) {
            return self.refuse(Some(id), Some(op.into()), self.dup(id));
        }
        self.last_id = Some(id);
        match self.execute(id, h.cmd) {
            Ok(Some(body)) => self.reply(id, body),
            Ok(None) => {}
            Err(f) => self.refuse(Some(id), Some(op.into()), f),
        }
    }

    fn dup(&self, id: u64) -> Fail {
        fail(
            ErrCode::DuplicateId,
            format!(
                "id {id} is not above the last id {}; not executed again",
                self.last_id.unwrap_or(0)
            ),
        )
    }

    // ------------------------------------------------------------------ commands

    fn execute(&mut self, id: u64, cmd: Command) -> Result<Option<Reply>, Fail> {
        Ok(Some(match cmd {
            Command::Sync(s) => Reply::Sync(SyncOk { nonce: s.nonce }),
            Command::Spawn(s) => self.spawn(id, s)?,
            Command::Signal(s) => self.signal(s)?,
            Command::Run(r) => self.run(id, r)?,
            Command::Probe(p) => self.probe(id, p)?,
            Command::Write(w) => self.write(w)?,
            Command::Read(r) => self.read(r)?,
            Command::Mkdir(m) => {
                DirBuilder::new()
                    .recursive(true)
                    .mode(m.mode.unwrap_or(0o755))
                    .create(self.path(&m.path))
                    .map_err(|e| os(&format!("mkdir {}", m.path), &e))?;
                Reply::Mkdir(MkdirOk {})
            }
            Command::Mount(m) => {
                let dst = self.path_str(&m.dst);
                let _ = DirBuilder::new().recursive(true).mode(0o755).create(&dst);
                let flags = m.flags.iter().flatten().fold(0, |a, f| a | mount_flag(*f));
                sys::mount(&m.src, &dst, &m.fstype, flags, m.data.as_deref())
                    .map_err(|e| os(&format!("mount {} on {}", m.src, m.dst), &e))?;
                Reply::Mount(MountOk {})
            }
            Command::Syncfs(_) => {
                sys::sync();
                Reply::Syncfs(SyncfsOk {})
            }
            Command::ClockSet(c) => {
                let (sec, nsec) = match (c.sec, c.step) {
                    (Some(s), None) => (s, 0),
                    (None, Some(d)) => {
                        let (s, n) = sys::realtime();
                        (s.saturating_add(d), n)
                    }
                    _ => return Err(fail(ErrCode::BadField, "exactly one of sec and step")),
                };
                sys::set_realtime(sec, nsec).map_err(|e| os("clock_settime", &e))?;
                Reply::ClockSet(ClockSetOk {
                    sec: sys::realtime().0,
                })
            }
            Command::ClockGet(_) => {
                let (sec, nsec) = sys::realtime();
                Reply::ClockGet(ClockGetOk {
                    sec,
                    nsec,
                    mono_ms: sys::now_ms(),
                })
            }
            Command::Qlen(q) => self.qlen(q)?,
            Command::Tail(t) => self.tail(t)?,
            Command::Untail(u) => {
                let t = self
                    .tails
                    .remove(&u.name)
                    .ok_or_else(|| fail(ErrCode::NotFound, format!("no tail {}", u.name)))?;
                Reply::Untail(UntailOk { lines: t.lineno })
            }
            Command::Freeze(f) => self.arm_freeze(id, f)?,
            Command::Unfreeze(_) => {
                let f = self
                    .freeze
                    .take()
                    .ok_or_else(|| fail(ErrCode::NotFound, "no freeze is armed"))?;
                let _ = sys::set_fifo(0);
                Reply::Unfreeze(UnfreezeOk {
                    fired: f.fired,
                    resumed: f.resumed,
                    counted: f.counter.counted,
                })
            }
        }))
    }

    fn start_child(
        &self,
        c: &ChildSpec<'_>,
        stdout: Stdio,
        stderr: Stdio,
        what: &str,
    ) -> Result<i32, Fail> {
        let Some(prog) = c.argv.first() else {
            return Err(fail(ErrCode::BadField, "argv: empty"));
        };
        let mut cmd = Proc::new(prog);
        cmd.args(&c.argv[1..])
            .env_clear()
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        for kv in c.env.iter().flatten() {
            let Some((k, v)) = kv.split_once('=') else {
                return Err(fail(
                    ErrCode::BadField,
                    format!("env: {kv:?} is not NAME=VALUE"),
                ));
            };
            cmd.env(k, v);
        }
        cmd.current_dir(self.path(c.cwd.as_deref().unwrap_or("/")));
        if let Some(u) = c.uid {
            cmd.uid(u);
        }
        if let Some(g) = c.gid {
            cmd.gid(g);
        }
        let ns = match c.netns {
            Some(p) => Some(File::open(self.path(p)).map_err(|e| os(&format!("open {p}"), &e))?),
            None => None,
        };
        let nsfd = ns.as_ref().map(|f| f.as_raw_fd());
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                if let Some(fd) = nsfd {
                    if libc::setns(fd, libc::CLONE_NEWNET) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| os(&format!("{what} {prog}"), &e))?;
        Ok(child.id() as i32)
    }

    fn out_file(&self, p: &Option<String>, append: bool) -> Result<File, Fail> {
        let p = p.as_deref().unwrap_or("/dev/null");
        let path = if p == "/dev/null" {
            PathBuf::from(p)
        } else {
            self.path(p)
        };
        let mut o = OpenOptions::new();
        o.write(true).create(true).mode(0o644);
        if append {
            o.append(true);
        } else {
            o.truncate(true);
        }
        o.open(&path).map_err(|e| os(&format!("open {p}"), &e))
    }

    fn spawn(&mut self, id: u64, s: Spawn) -> Result<Reply, Fail> {
        if self.services.get(&s.h).is_some_and(|x| x.running) {
            return Err(fail(
                ErrCode::HandleBusy,
                format!("{} is still running", s.h),
            ));
        }
        let append = s.append.unwrap_or(false);
        let out = self.out_file(&s.stdout, append)?;
        let err = if s.stderr.is_some() && s.stderr == s.stdout {
            out.try_clone().map_err(|e| os("dup", &e))?
        } else {
            self.out_file(&s.stderr, append)?
        };
        let c = ChildSpec {
            argv: &s.argv,
            env: &s.env,
            cwd: &s.cwd,
            uid: s.uid,
            gid: s.gid,
            netns: &s.netns,
        };
        let pid = self.start_child(&c, out.into(), err.into(), "spawn")?;
        let kill_at = s.kill_after_ms.map(|ms| sys::now_ms() + ms);
        self.owners.insert(pid, Owner::Spawn(s.h.clone()));
        self.services.insert(
            s.h,
            Service {
                pid,
                id,
                running: true,
                kill_at,
                timed_out: false,
            },
        );
        Ok(Reply::Spawn(SpawnOk {}))
    }

    fn signal(&mut self, s: Signal) -> Result<Reply, Fail> {
        let svc = self
            .services
            .get(&s.h)
            .ok_or_else(|| fail(ErrCode::NoSuchHandle, format!("no process {}", s.h)))?;
        if !svc.running {
            return Err(fail(ErrCode::NotRunning, format!("{} has exited", s.h)));
        }
        let pids = if s.tree {
            tree::collect(svc.pid)
        } else {
            vec![svc.pid]
        };
        let n = sig_num(s.sig);
        let procs = pids.iter().filter(|&&p| sys::kill(p, n)).count() as u32;
        let stopped = s
            .wait_stopped
            .unwrap_or(false)
            .then(|| tree::wait_stopped(&pids));
        Ok(Reply::Signal(SignalOk { procs, stopped }))
    }

    fn run(&mut self, id: u64, r: Run) -> Result<Reply, Fail> {
        let mut fds = [0 as RawFd; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(os("pipe", &std::io::Error::last_os_error()));
        }
        let (rd, wr) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let wr2 = wr.try_clone().map_err(|e| os("dup", &e))?;
        let c = ChildSpec {
            argv: &r.argv,
            env: &r.env,
            cwd: &r.cwd,
            uid: r.uid,
            gid: r.gid,
            netns: &r.netns,
        };
        let pid = self.start_child(&c, Stdio::from(wr), Stdio::from(wr2), "run")?;
        sys::set_nonblock(rd.as_raw_fd());
        let cap = r.capture.unwrap_or(DEFAULT_CAPTURE).min(MAX_CAPTURE) as usize;
        let kill_at = r.kill_after_ms.map(|ms| sys::now_ms() + ms);
        self.owners.insert(pid, Owner::Run(id));
        self.runs.insert(
            id,
            RunState {
                pid,
                pipe: Some(rd),
                kept: Vec::new(),
                cap,
                total: 0,
                kill_at,
                timed_out: false,
            },
        );
        Ok(Reply::Run(RunOk {}))
    }

    fn read_pipe(&mut self, id: u64) {
        let Some(r) = self.runs.get_mut(&id) else {
            return;
        };
        let Some(p) = &r.pipe else { return };
        let fd = p.as_raw_fd();
        let mut b = [0u8; 4096];
        loop {
            match sys::read(fd, &mut b) {
                Ok(0) => {
                    r.pipe = None;
                    break;
                }
                Ok(n) => {
                    r.total += n as u64;
                    let room = r.cap.saturating_sub(r.kept.len());
                    r.kept.extend_from_slice(&b[..n.min(room)]);
                }
                Err(_) => break,
            }
        }
    }

    fn probe(&mut self, id: u64, p: Probe) -> Result<Reply, Fail> {
        if p.argv.is_empty() {
            return Err(fail(ErrCode::BadField, "argv: empty"));
        }
        let start = sys::now_ms();
        let deadline = start + p.deadline_ms;
        self.probes.insert(
            id,
            ProbeState {
                spec: p,
                start,
                deadline,
                tries: 0,
                attempt: None,
                next_at: Some(start),
                expired: false,
                last: None,
            },
        );
        // the first attempt starts in this round, after the reply
        Ok(Reply::Probe(ProbeOk {}))
    }

    fn probe_attempt(&mut self, id: u64) {
        let Some(p) = self.probes.get(&id) else {
            return;
        };
        let s = &p.spec;
        let c = ChildSpec {
            argv: &s.argv,
            env: &s.env,
            cwd: &s.cwd,
            uid: s.uid,
            gid: s.gid,
            netns: &s.netns,
        };
        let r = self.start_child(&c, Stdio::null(), Stdio::null(), "probe");
        let p = self.probes.get_mut(&id).expect("probe");
        p.next_at = None;
        p.tries += 1;
        match r {
            Ok(pid) => {
                p.attempt = Some(pid);
                self.owners.insert(pid, Owner::Probe(id));
            }
            Err(_) => {
                // a command that cannot start counts as a failed attempt (status 127, as a shell)
                p.last = Some(127);
                self.probe_next(id);
            }
        }
    }

    /// After a failed attempt: end the probe or schedule the next attempt.
    fn probe_next(&mut self, id: u64) {
        let Some(p) = self.probes.get(&id) else {
            return;
        };
        if p.expired {
            return self.probe_end(id, ProbeEnd::Deadline);
        }
        if let Some(h) = &p.spec.alive {
            if !self.services.get(h).is_some_and(|s| s.running) {
                return self.probe_end(id, ProbeEnd::Died);
            }
        }
        let at = (p.start + p.tries * p.spec.every_ms).max(sys::now_ms());
        if at > p.deadline {
            return self.probe_end(id, ProbeEnd::Deadline);
        }
        self.probes.get_mut(&id).expect("probe").next_at = Some(at);
    }

    fn probe_end(&mut self, id: u64, why: ProbeEnd) {
        let Some(p) = self.probes.remove(&id) else {
            return;
        };
        self.event(Event::Probe(ProbeDone {
            id,
            ok: why == ProbeEnd::Ok,
            why,
            tries: p.tries,
            status: p.last,
        }));
    }

    fn write(&mut self, w: Write) -> Result<Reply, Fail> {
        let path = self.path(&w.path);
        if let Some(d) = path.parent() {
            let _ = DirBuilder::new().recursive(true).mode(0o755).create(d);
        }
        let mut o = OpenOptions::new();
        o.write(true).create(true).mode(w.mode.unwrap_or(0o644));
        if w.append.unwrap_or(false) {
            o.append(true);
        } else {
            o.truncate(true);
        }
        let mut f = o
            .open(&path)
            .map_err(|e| os(&format!("open {}", w.path), &e))?;
        f.write_all(w.data.as_bytes())
            .map_err(|e| os(&format!("write {}", w.path), &e))?;
        Ok(Reply::Write(WriteOk {
            bytes: w.data.len() as u64,
        }))
    }

    fn read(&mut self, r: Read) -> Result<Reply, Fail> {
        let max = r.max.min(MAX_READ) as usize;
        let mut f =
            File::open(self.path(&r.path)).map_err(|e| os(&format!("open {}", r.path), &e))?;
        let size = f
            .metadata()
            .map_err(|e| os(&format!("stat {}", r.path), &e))?
            .len();
        let off = r.offset.unwrap_or(0);
        f.seek(SeekFrom::Start(off))
            .map_err(|e| os(&format!("seek {}", r.path), &e))?;
        let mut b = Vec::with_capacity(max);
        f.take(max as u64)
            .read_to_end(&mut b)
            .map_err(|e| os(&format!("read {}", r.path), &e))?;
        let eof = off + b.len() as u64 >= size;
        let data = if r.hex.unwrap_or(false) {
            b.iter().map(|c| format!("{c:02x}")).collect()
        } else {
            String::from_utf8(b).map_err(|_| {
                fail(
                    ErrCode::NotUtf8,
                    format!("{} holds bytes that are not UTF-8 there", r.path),
                )
            })?
        };
        Ok(Reply::Read(ReadOk { data, size, eof }))
    }

    fn qlen(&mut self, q: Qlen) -> Result<Reply, Fail> {
        let out = Proc::new(&q.tc)
            .args(["-s", "qdisc", "show", "dev", &q.dev])
            .env_clear()
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| os(&format!("run {}", q.tc), &e))?;
        Ok(Reply::Qlen(QlenOk {
            packets: backlog(&String::from_utf8_lossy(&out.stdout), &q.handle),
        }))
    }

    fn tail(&mut self, t: Tail) -> Result<Reply, Fail> {
        if self.tails.contains_key(&t.name) {
            return Err(fail(ErrCode::Busy, format!("a tail {} exists", t.name)));
        }
        let path = self.path_str(&t.path);
        let dir = match path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(i) => path[..i].to_string(),
            None => ".".to_string(),
        };
        let ino = match self.inotify {
            Some(i) => i,
            None => {
                let i = sys::inotify().map_err(|e| os("inotify", &e))?;
                self.inotify = Some(i);
                i
            }
        };
        sys::inotify_watch(ino, &dir)
            .map_err(|e| os(&format!("watch the directory of {}", t.path), &e))?;
        let offset = if t.from_start.unwrap_or(false) {
            0
        } else {
            fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
        };
        self.tails.insert(
            t.name.clone(),
            TailState {
                path,
                offset,
                partial: Vec::new(),
                cut: false,
                lineno: 0,
            },
        );
        Ok(Reply::Tail(TailOk {}))
    }

    fn drain_inotify(&mut self) {
        let Some(i) = self.inotify else { return };
        let mut b = [0u8; 4096];
        while matches!(sys::read(i, &mut b), Ok(n) if n > 0) {}
    }

    fn check_tails(&mut self) {
        let names: Vec<String> = self.tails.keys().cloned().collect();
        for name in names {
            let mut lines = Vec::new();
            let mut warn = None;
            {
                let t = self.tails.get_mut(&name).expect("tail");
                match File::open(&t.path) {
                    Err(_) => continue,
                    Ok(mut f) => {
                        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        if size < t.offset {
                            t.offset = 0;
                            t.partial.clear();
                            t.cut = false;
                        }
                        if size == t.offset {
                            continue;
                        }
                        let mut b = Vec::new();
                        let r = f
                            .seek(SeekFrom::Start(t.offset))
                            .and_then(|_| (&mut f).take(size - t.offset).read_to_end(&mut b));
                        if let Err(e) = r {
                            warn = Some(format!("{name}: {}", sys::errno_name(sys::io_errno(&e))));
                        }
                        t.offset += b.len() as u64;
                        for &c in &b {
                            if c == b'\n' {
                                t.lineno += 1;
                                lines.push((
                                    t.lineno,
                                    String::from_utf8_lossy(&t.partial).into_owned(),
                                    t.cut,
                                ));
                                t.partial.clear();
                                t.cut = false;
                            } else if t.partial.len() < MAX_LINE {
                                t.partial.push(c);
                            } else {
                                t.cut = true;
                            }
                        }
                    }
                }
            }
            for (n, text, cut) in lines {
                self.event(Event::Line(Line {
                    name: name.clone(),
                    n,
                    text,
                    cut: cut.then_some(true),
                }));
            }
            if let Some(msg) = warn {
                self.event(Event::Warn(Warn {
                    code: WarnCode::TailFailed,
                    msg,
                }));
            }
        }
    }

    /// Runs `f` inside the network namespace `ns` (a file such as /tmp/hydo/ns/redis), then
    /// returns to the agent's own namespace; a socket made inside stays in that namespace (the
    /// netfilter queue of a freeze with `netns`). The agent has one thread, so nothing else runs
    /// in the other namespace meanwhile.
    fn in_netns<T>(
        &self,
        ns: Option<&str>,
        f: impl FnOnce() -> Result<T, Fail>,
    ) -> Result<T, Fail> {
        let Some(p) = ns else {
            return f();
        };
        let own = File::open("/proc/self/ns/net")
            .map_err(|e| os("open the agent's network namespace", &e))?;
        let target = File::open(self.path(p)).map_err(|e| os(&format!("open {p}"), &e))?;
        if unsafe { libc::setns(target.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
            return Err(os(&format!("setns {p}"), &std::io::Error::last_os_error()));
        }
        let r = f();
        if unsafe { libc::setns(own.as_raw_fd(), libc::CLONE_NEWNET) } != 0 {
            // Every later child would start in the wrong namespace: the agent cannot go on.
            let e = std::io::Error::last_os_error();
            eprintln!("hydo-agent: setns back to its own network namespace: {e}");
            std::process::abort();
        }
        r
    }

    fn arm_freeze(&mut self, id: u64, f: Freeze) -> Result<Reply, Fail> {
        if self.freeze.is_some() {
            return Err(fail(ErrCode::Busy, "a freeze is already armed"));
        }
        if !self.services.contains_key(&f.h) {
            return Err(fail(ErrCode::NoSuchHandle, format!("no process {}", f.h)));
        }
        let queue =
            u16::try_from(f.queue).map_err(|_| fail(ErrCode::BadField, "queue: above 65535"))?;
        let sip = match &f.sip {
            Some(s) => {
                let a = nfq::parse_ipv4(s);
                if a == [0; 4] {
                    return Err(fail(
                        ErrCode::BadField,
                        format!("sip: {s:?} is not an IPv4 address"),
                    ));
                }
                Some(a)
            }
            None => None,
        };
        let port = |p: Option<u32>, k: &str| {
            p.map(|x| {
                u16::try_from(x).map_err(|_| fail(ErrCode::BadField, format!("{k}: above 65535")))
            })
            .transpose()
        };
        let (port, own_port) = (port(f.port, "port")?, port(f.own_port, "own_port")?);
        if sip.is_none() && port.is_none() && own_port.is_none() {
            return Err(fail(ErrCode::BadField, "one of sip, port and own_port"));
        }
        let log = match &f.log {
            Some(l) => Some(File::create(self.path(l)).map_err(|e| os(&format!("open {l}"), &e))?),
            None => None,
        };
        let nfq = self.in_netns(f.netns.as_deref(), || {
            Nfq::bind(queue).map_err(|(what, e)| os(what, &e))
        })?;
        let _ = sys::set_fifo(50);
        let counter = Counter {
            sip,
            own: own_port.is_some() && port.is_none(),
            tport: port.or(own_port),
            want_req: f.dir == Dir::Req,
            pattern: f
                .pattern
                .as_deref()
                .map(nfq::unescape_match)
                .unwrap_or_default(),
            target: f.n,
            nreq: 0,
            nresp: 0,
            counted: 0,
        };
        self.freeze = Some(FreezeState {
            id,
            h: f.h,
            nfq,
            counter,
            dur: f.dur_ms,
            log,
            nlog: 0,
            fired: false,
            resumed: false,
            resume_at: 0,
            frozen: Vec::new(),
        });
        Ok(Reply::Freeze(FreezeOk {}))
    }

    fn on_packets(&mut self) {
        let Some(f) = self.freeze.as_mut() else {
            return;
        };
        let pkts = match f.nfq.recv() {
            Ok(p) => p,
            Err(e) => {
                if sys::io_errno(&e) == libc::ENOBUFS {
                    self.event(Event::Warn(Warn {
                        code: WarnCode::NetlinkEnobufs,
                        msg: "netlink ENOBUFS (packets dropped)".into(),
                    }));
                }
                return;
            }
        };
        for p in pkts {
            let f = self.freeze.as_mut().expect("freeze");
            let t = match nfq::tcp(&p.pkt) {
                Some(t) if !f.fired => t,
                _ => {
                    self.verdict(p.id);
                    continue;
                }
            };
            let s = f.counter.see(&t);
            if let Some(l) = f.log.as_mut() {
                if f.nlog < 200_000 {
                    let mut line = nfq::log_line(&s, f.counter.counted, &t, sys::now_ms());
                    line.truncate(399);
                    let _ = l.write_all(line.as_bytes());
                    f.nlog += 1;
                }
            }
            if !s.fire {
                self.verdict(p.id);
                continue;
            }
            let (h, id, want) = (f.h.clone(), f.id, f.counter.want_req);
            let count = f.counter.counted;
            let root = self.services.get(&h).filter(|s| s.running).map(|s| s.pid);
            match root {
                Some(root) if root > 1 => {
                    let frozen = tree::collect(root);
                    for &pid in &frozen {
                        sys::kill(pid, libc::SIGSTOP);
                    }
                    let all = tree::wait_stopped(&frozen);
                    let procs = frozen.len() as u32;
                    self.freeze.as_mut().expect("freeze").frozen = frozen;
                    let dir = |r: bool| if r { Dir::Req } else { Dir::Resp };
                    self.event(Event::Froze(Froze {
                        id,
                        h,
                        procs,
                        all_stopped: all,
                        dir: dir(want),
                        count,
                        kdir: dir(s.is_req),
                        k: s.k,
                        sport: t.sport as u32,
                        dport: t.dport as u32,
                        len: t.payload.len() as u32,
                        snippet: nfq::snippet(t.payload, 60),
                    }));
                }
                _ => self.event(Event::Warn(Warn {
                    code: WarnCode::FreezeNoProcess,
                    msg: format!("the n-th packet came, but {h} is not running; nothing frozen"),
                })),
            }
            let f = self.freeze.as_mut().expect("freeze");
            f.fired = true;
            f.resume_at = sys::now_ms() + f.dur;
            self.verdict(p.id);
        }
    }

    fn verdict(&mut self, id: u32) {
        let Some(f) = &self.freeze else { return };
        if let Err(e) = f.nfq.accept(id) {
            let msg = format!("verdict: {}", sys::errno_name(sys::io_errno(&e)));
            self.event(Event::Warn(Warn {
                code: WarnCode::VerdictFailed,
                msg,
            }));
        }
    }

    // ------------------------------------------------------------------ deadlines and exits

    fn on_deadlines(&mut self) {
        let now = sys::now_ms();
        // (time, kind, id): 0 freeze resume, 1 spawn kill, 2 run kill, 3 probe deadline, 4 probe attempt
        let mut due: Vec<(u64, u8, String)> = Vec::new();
        if let Some(f) = &self.freeze {
            if f.fired && !f.resumed && f.resume_at <= now {
                due.push((f.resume_at, 0, String::new()));
            }
        }
        for (h, s) in &self.services {
            if let Some(t) = s.kill_at.filter(|&t| t <= now && s.running && !s.timed_out) {
                due.push((t, 1, h.clone()));
            }
        }
        for (id, r) in &self.runs {
            if let Some(t) = r.kill_at.filter(|&t| t <= now && !r.timed_out) {
                due.push((t, 2, format!("{id:020}")));
            }
        }
        for (id, p) in &self.probes {
            if !p.expired && p.deadline <= now {
                due.push((p.deadline, 3, format!("{id:020}")));
            } else if let Some(t) = p.next_at.filter(|&t| t <= now) {
                due.push((t, 4, format!("{id:020}")));
            }
        }
        due.sort();
        for (_, kind, key) in due {
            match kind {
                0 => {
                    let f = self.freeze.as_mut().expect("freeze");
                    for &p in &f.frozen {
                        sys::kill(p, libc::SIGCONT);
                    }
                    f.resumed = true;
                    let (id, procs) = (f.id, f.frozen.len() as u32);
                    self.event(Event::Resumed(Resumed { id, procs }));
                }
                1 => {
                    let s = self.services.get_mut(&key).expect("service");
                    s.timed_out = true;
                    for p in tree::collect(s.pid) {
                        sys::kill(p, libc::SIGKILL);
                    }
                }
                2 => {
                    let id: u64 = key.parse().expect("id");
                    let r = self.runs.get_mut(&id).expect("run");
                    r.timed_out = true;
                    for p in tree::collect(r.pid) {
                        sys::kill(p, libc::SIGKILL);
                    }
                }
                3 => {
                    let id: u64 = key.parse().expect("id");
                    let p = self.probes.get_mut(&id).expect("probe");
                    p.expired = true;
                    p.next_at = None;
                    match p.attempt {
                        Some(pid) => {
                            for q in tree::collect(pid) {
                                sys::kill(q, libc::SIGKILL);
                            }
                        }
                        None => self.probe_end(id, ProbeEnd::Deadline),
                    }
                }
                _ => {
                    let id: u64 = key.parse().expect("id");
                    self.probe_attempt(id);
                }
            }
        }
    }

    fn reap(&mut self) {
        while let Some((pid, st)) = sys::reap_one() {
            let Some(owner) = self.owners.remove(&pid) else {
                continue;
            };
            let (status, sig) = sys::exit_status(st);
            match owner {
                Owner::Spawn(h) => {
                    let s = self.services.get_mut(&h).expect("service");
                    s.running = false;
                    let (id, timed_out) = (s.id, s.timed_out.then_some(true));
                    self.event(Event::Exit(Exit {
                        h: h.clone(),
                        id,
                        status,
                        sig,
                        timed_out,
                    }));
                    let dead: Vec<u64> = self
                        .probes
                        .iter()
                        .filter(|(_, p)| {
                            p.attempt.is_none() && p.spec.alive.as_deref() == Some(h.as_str())
                        })
                        .map(|(id, _)| *id)
                        .collect();
                    for id in dead {
                        self.probe_end(id, ProbeEnd::Died);
                    }
                }
                Owner::Run(id) => {
                    self.read_pipe(id);
                    let r = self.runs.remove(&id).expect("run");
                    let out = String::from_utf8_lossy(&r.kept).into_owned();
                    self.event(Event::Done(Done {
                        id,
                        status,
                        sig,
                        timed_out: r.timed_out.then_some(true),
                        out,
                        out_len: r.total,
                    }));
                }
                Owner::Probe(id) => {
                    let p = self.probes.get_mut(&id).expect("probe");
                    p.attempt = None;
                    p.last = Some(status);
                    if status == 0 && !p.expired {
                        self.probe_end(id, ProbeEnd::Ok);
                    } else {
                        self.probe_next(id);
                    }
                }
            }
        }
    }
}

/// The packets in the backlog of qdisc `handle` in `tc -s qdisc show` output.
pub fn backlog(out: &str, handle: &str) -> Option<u64> {
    let mut found = false;
    for l in out.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.first() == Some(&"qdisc") {
            found = w.get(2) == Some(&handle);
            continue;
        }
        if found && w.first() == Some(&"backlog") {
            return w.get(2).and_then(|p| p.trim_end_matches('p').parse().ok());
        }
    }
    None
}

/// Takes the agent's own console: raw mode, no echo, no controlling terminal.
pub fn open_console(path: &str) -> std::io::Result<RawFd> {
    let fd = sys::open(path, libc::O_RDWR | libc::O_NOCTTY, 0)?;
    sys::tty_raw(fd);
    Ok(fd)
}

/// PID 1's duties before it serves: the kernel file systems, then the CRNG seed.
pub fn pid1_setup() {
    if fs::metadata("/dev/null").is_err() {
        let _ = sys::mount("none", "/dev", "devtmpfs", 0, None);
    }
    let _ = sys::mount("none", "/proc", "proc", 0, None);
    let _ = sys::mount("none", "/sys", "sysfs", 0, None);
    let _ = sys::mount("none", "/tmp", "tmpfs", 0, None);
    if fs::metadata("/bin/hvx_seedrng").is_ok() {
        // its pinned bytes stay as they are; the agent only runs it (SIGCHLD is not blocked yet)
        let _ = Proc::new("/bin/hvx_seedrng")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The console the agent serves: `$HYDO_AGENT_TTY` (the kernel passes an unknown `NAME=value` of
/// its command line to PID 1 as environment; the PID-1 test uses it for a pty), else `hydo.tty=<path>`
/// on the kernel command line, else /dev/ttyS0.
pub fn console_path() -> String {
    if let Some(p) = std::env::var_os("HYDO_AGENT_TTY") {
        return p.to_string_lossy().into_owned();
    }
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    for tok in cmdline.split_whitespace() {
        if let Some(p) = tok.strip_prefix("hydo.tty=") {
            return p.to_string();
        }
    }
    "/dev/ttyS0".into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn backlog_of_a_handle() {
        let out = "qdisc prio 1: root refcnt 2 bands 4 priomap 1 1\n Sent 100 bytes 2 pkt (dropped 0, overlimits 0 requeues 0)\n backlog 0b 0p requeues 0\nqdisc netem 40: parent 1:4 limit 1000 delay 400ms\n Sent 5 bytes 1 pkt\n backlog 1514b 7p requeues 0\n";
        assert_eq!(super::backlog(out, "40:"), Some(7));
        assert_eq!(super::backlog(out, "1:"), Some(0));
        assert_eq!(super::backlog(out, "50:"), None);
    }
}
