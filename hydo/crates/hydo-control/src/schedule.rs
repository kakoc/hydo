//! `ScheduleController`: today's schedule language and the core project model driven through
//! hydo-agent's commands (docs/design/guest-control.md, step B1).
//!
//! It does what `hydo/onboard/hydo-init.sh` does, in the same order, with the shell's blocking
//! steps turned into commands and the answers awaited: one function per init function, under the
//! same name, written with [`crate::flow`] so that the code reads like the shell. The background
//! jobs of the shell (`periodic_loop`, `pause_timer`) are threads of the flow; their order against
//! the main sequence depends on guest timing, as it does in the shell.
//!
//! A run is a set of conversations, as the serve protocol is today (S0, S1, `run`, `oracle`):
//! - [`ScheduleController::boot`]: a fresh boot up to S1 (the shell's `provision` command):
//!   the run directory, `lo`, the assertion declarations, snapshot S0, the start clock, the data
//!   disk, the services with their readiness checks, provisioning, `sync`, snapshot S1;
//! - [`ScheduleController::run`]: from S1, the schedule's guest part (the shell's `run`
//!   command): the knobs, the step sequence with its kills, timed kills and pauses, the power cut
//!   or the heal, restart and retry, the disk faults switched off, the final invariants and the
//!   verdict;
//! - [`ScheduleController::recovery`]: the recovery boot from S0 after a power cut (the shell's
//!   `oracle` command): the acknowledged and failed ids, the data disk, the services that
//!   recover, the final invariants;
//! - [`ScheduleController::solo`]: the shell's solo mode (no host interaction there): provision,
//!   every step, the retry, the final invariants.
//!
//! The report lines (assertion declarations and records, `HYDO-SETUP-COMPLETE`, the acknowledged
//! and failed ids, the verdict) are the shell's console lines byte for byte; the notes are its
//! `hydo-ob:` log lines without the uptime it printed. The places where the shell's behaviour is
//! ambiguous, and what the controller does there, are listed in the crate's HANDOFF.md
//! ("Decisions where the shell is ambiguous"); each is also marked `AMBIGUOUS:` below.
//!
//! The features of the init extensions are seams ([`crate::seams`]), marked `SEAM(<name>)` below.
//! The driven ones are child modules in `src/features/` (they use this module's helpers):
//! liveness, clock, buggify and the recovery inputs (M122).

use crate::control::{Controller, Ending, EngineAction, Out, Seen, StopInfo};
use crate::flow::{Facts, Flow, Fx, Memo, Step};
use crate::plan::Plan;
use crate::seams::{Seam, Unsupported};
use hydo_agent_proto::msg::{
    ClockSet, Command, Event, Mkdir, Mount, MountFlag, Probe, ProbeEnd, Read, Reply, Run, Sig,
    Signal, Spawn, Syncfs, Write,
};
use hydo_schedule::{GuestToken, Pause, Schedule};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

// The feature controllers of M123 (seams history, error-logs, composer).
pub mod composer;
pub mod errlog;
pub mod history;
mod text;
// The feature controllers of wave 3 (docs/design/guest-control.md step B2), each a module of its
// own, called from its seam below.
#[path = "features/buggify.rs"]
mod buggify;
#[path = "features/clock.rs"]
mod clock;
#[path = "features/liveness.rs"]
mod liveness;
#[path = "features/recovery.rs"]
mod recovery;
pub use recovery::RecoveryInputs;
mod fakes;
mod freeze;
mod netfault;
mod netns;

/// What the host knows about the guest at a snapshot, carried into the conversations that resume
/// from it (the design: host state at a snapshot is the schedule, the seeds and the events so far).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuestState {
    /// Services running at the snapshot: handle -> the id of the spawn that started the process.
    pub running: BTreeMap<String, u64>,
    /// Services started at least once in this boot (their log files exist).
    pub started: BTreeSet<String>,
    /// What the init exported after its start-up environment (HYDO_SVC and HYDO_LOG of the last
    /// service it started).
    pub env: BTreeMap<String, String>,
}

/// Which conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conversation {
    Boot,
    Run,
    Recovery {
        acked: Vec<String>,
        failed: Vec<String>,
    },
    Solo,
}

impl Conversation {
    pub fn name(&self) -> &'static str {
        match self {
            Conversation::Boot => "boot",
            Conversation::Run => "run",
            Conversation::Recovery { .. } => "recovery",
            Conversation::Solo => "solo",
        }
    }
}

/// The pause token, resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PauseCfg {
    Steps {
        svc: String,
        at: i64,
        up: i64,
    },
    Timer {
        svc: String,
        at: i64,
        off: u64,
        dur: u64,
    },
}

/// The schedule's guest part, as explore_run reads it (the last token of a letter wins, as the
/// shell's `case` assignments do; knobs are applied in token order).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RunCfg {
    guest_text: String,
    knobs: Vec<(String, String)>,
    k: Option<(String, i64, i64)>,
    c: Option<i64>,
    p: Option<PauseCfg>,
    x: Option<(String, i64, u64)>,
    /// T=: (at, delta seconds).
    t: Option<(i64, i64)>,
    /// The values of N= and O= as written (NET-FAULTS reads them as the shell does).
    n: Option<String>,
    o: Option<String>,
}

#[derive(Debug)]
struct Ctx {
    plan: Plan,
    conv: Conversation,
    run: RunCfg,
    mount_flags: Vec<MountFlag>,
    mount_data: Option<String>,
    /// What the recovery conversation was given besides the ids.
    carried: recovery::Carried,
}

#[derive(Clone, Debug)]
struct Svc {
    spawn: u64,
    killed: bool,
}

/// The host state the threads share (the shell's files in $H and its variables).
#[derive(Clone, Debug)]
struct Host {
    ctx: Arc<Ctx>,
    services: BTreeMap<String, Svc>,
    started: BTreeSet<String>,
    env: BTreeMap<String, String>,
    /// $H/violated: the key of the first invariant that failed.
    violated: Option<String>,
    /// $H/error: the last "could not be evaluated" verdict.
    error: Option<String>,
    /// $H/asserted.<inv>.<ok>
    asserted: BTreeSet<(String, bool)>,
    /// $H/inv.<inv>.out: the output of the invariant's latest evaluation.
    last_out: BTreeMap<String, String>,
    /// The periodic checkers' threads.
    periodic: Vec<u64>,
    snaps: BTreeMap<String, GuestState>,
    // SEAM(Composer), SEAM(ErrorLogs), SEAM(History): their files in $H
    compose: composer::State,
    errlog: errlog::State,
    hist: history::State,
    /// The liveness extension's files.
    live: liveness::Live,
    /// What a power cut hands to the recovery conversation.
    cut: Option<RecoveryInputs>,
    /// The fake outside world's state (SEAM(Fakes): schedule/fakes.rs).
    pf: fakes::Pf,
    /// The shell's marker files of the feature controllers ($H/fc.decl, ...).
    marks: BTreeSet<String>,
    /// FREEZE: the id of the armed `freeze` command (the shell's $fc_pid: the helper runs).
    fc_armed: Option<u64>,
}

/// One thread of the controller.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Main,
    /// periodic_loop for the invariant with this index.
    Every(usize),
    PauseTimer {
        svc: String,
        off: u64,
        dur: u64,
    },
    /// The stack's kill_timer (X=), see `explore_run`.
    KillTimer {
        svc: String,
        off: u64,
    },
    /// SEAM(Composer): the composer's daemon and one thread per launched command.
    Compose(composer::Job),
    /// live_monitor for an eventually invariant.
    Live(liveness::Monitor),
    /// FREEZE: hydo_fc's console lines, from the armed freeze's events.
    FreezeWatch {
        id: u64,
        dur: String,
    },
}

fn label(p: &Part) -> String {
    match p {
        Part::Main => "main".into(),
        Part::Every(i) => format!("every:i{i}"),
        Part::PauseTimer { .. } => "pause-timer".into(),
        Part::KillTimer { .. } => "kill-timer".into(),
        Part::Compose(j) => composer::label(j),
        Part::Live(m) => liveness::label(m),
        Part::FreezeWatch { .. } => "freeze".into(),
    }
}

type F<'a> = Fx<'a, Host, Part>;

/// The controller.
#[derive(Clone, Debug)]
pub struct ScheduleController {
    flow: Flow<Host, Part>,
    facts: Facts,
    conv: Conversation,
    waiting_hello: bool,
    started: bool,
    /// A fresh boot (no runner sync to wait for).
    fresh: bool,
}

/// The fault assertions explore_run declares, in its order (type, id, message).
pub const FAULT_DECLS: [(&str, &str, &str); 6] = [
    (
        "reachable",
        "fault.paused",
        "a service was paused (SIGSTOP)",
    ),
    (
        "sometimes",
        "fault.pause_mid_request",
        "a pause began while a client request was in flight",
    ),
    (
        "reachable",
        "fault.clock_back",
        "the realtime clock was stepped backwards",
    ),
    (
        "reachable",
        "fault.clock_fwd",
        "the realtime clock was stepped forwards",
    ),
    (
        "reachable",
        "fault.oneway_req",
        "a one-way network fault hit the req direction",
    ),
    (
        "reachable",
        "fault.oneway_resp",
        "a one-way network fault hit the resp direction",
    ),
];

/// The declaration of the timed kill's assertion (only for a schedule with X=).
pub const KILL_TIMER_DECL: (&str, &str, &str) = (
    "reachable",
    "fault.killed_timer",
    "a service was killed by a timer while the workload ran",
);

impl ScheduleController {
    fn new(
        plan: Plan,
        conv: Conversation,
        run: RunCfg,
        at: Option<&GuestState>,
    ) -> Result<ScheduleController, Unsupported> {
        let (mount_flags, mount_data) = mount_opts(&plan.data_opts)?;
        let fresh = at.is_none();
        let at = at.cloned().unwrap_or_default();
        let ctx = Arc::new(Ctx {
            plan,
            conv: conv.clone(),
            run,
            mount_flags,
            mount_data,
            carried: recovery::Carried::default(),
        });
        let host = Host {
            ctx,
            services: at
                .running
                .iter()
                .map(|(h, id)| {
                    (
                        h.clone(),
                        Svc {
                            spawn: *id,
                            killed: false,
                        },
                    )
                })
                .collect(),
            started: at.started,
            env: at.env,
            violated: None,
            error: None,
            asserted: BTreeSet::new(),
            last_out: BTreeMap::new(),
            periodic: Vec::new(),
            snaps: BTreeMap::new(),
            compose: Default::default(),
            errlog: Default::default(),
            hist: Default::default(),
            live: liveness::Live::default(),
            cut: None,
            pf: fakes::Pf::default(),
            marks: BTreeSet::new(),
            fc_armed: None,
        };
        Ok(ScheduleController {
            flow: Flow::new(host, Part::Main),
            facts: Facts::default(),
            conv,
            waiting_hello: fresh,
            started: false,
            fresh,
        })
    }

    /// A fresh boot up to S1 (it waits for the agent's hello).
    pub fn boot(plan: Plan) -> Result<ScheduleController, Unsupported> {
        Self::new(plan, Conversation::Boot, RunCfg::default(), None)
    }

    /// The shell's solo mode on a fresh boot.
    pub fn solo(plan: Plan) -> Result<ScheduleController, Unsupported> {
        Self::new(plan, Conversation::Solo, RunCfg::default(), None)
    }

    /// The schedule's guest part, from the snapshot S1 whose guest state is `s1`. Host tokens
    /// are the engine's and are not read here.
    pub fn run(
        plan: Plan,
        schedule: &Schedule,
        s1: &GuestState,
    ) -> Result<ScheduleController, Unsupported> {
        let cfg = run_cfg(&plan, schedule)?;
        Self::new(plan, Conversation::Run, cfg, Some(s1))
    }

    /// The recovery boot after a power cut, from the snapshot S0 whose guest state is `s0`,
    /// given only the ids (nothing carried: with guest.clock_after_cut the clock starts from
    /// guest.clock_start, as for a missing `clock=` token).
    pub fn recovery(
        plan: Plan,
        acked: Vec<String>,
        failed: Vec<String>,
        s0: &GuestState,
    ) -> Result<ScheduleController, Unsupported> {
        Self::recovery_from(
            plan,
            &RecoveryInputs {
                acked,
                failed,
                ..RecoveryInputs::default()
            },
            s0,
        )
    }

    /// The recovery boot after a power cut, given what the run handed over
    /// ([`ScheduleController::recovery_inputs`]), from the snapshot S0 whose guest state is `s0`.
    pub fn recovery_from(
        plan: Plan,
        inputs: &RecoveryInputs,
        s0: &GuestState,
    ) -> Result<ScheduleController, Unsupported> {
        let mut c = Self::new(
            plan,
            Conversation::Recovery {
                acked: inputs.acked.clone(),
                failed: inputs.failed.clone(),
            },
            RunCfg::default(),
            Some(s0),
        )?;
        // SEAM(RecoveryInputs): the values carried across the cut
        Arc::get_mut(&mut c.flow.shared.ctx)
            .expect("a new controller")
            .carried = recovery::Carried {
            clock: inputs.clock,
        };
        Ok(c.with_world(&inputs.world))
    }

    /// After a run that ended with a power cut: what the recovery conversation is given.
    pub fn recovery_inputs(&self) -> Option<&RecoveryInputs> {
        self.flow.shared.cut.as_ref()
    }

    /// The fake outside world the first boot carried across the power cut
    /// ([`Ending::PowerCut`]'s `world`), for a recovery conversation of a plan with `fakes:` or
    /// `proxies:` (it starts the world from them). Without it the recovery's world starts from
    /// the records among the acknowledged and failed ids only, as the shell's would from an
    /// oracle line without them.
    pub fn with_world(mut self, world: &[String]) -> ScheduleController {
        self.flow.shared.pf.carried = world.to_vec();
        self
    }

    pub fn conversation(&self) -> &Conversation {
        &self.conv
    }

    /// The guest state at each snapshot this conversation took (S0, S1).
    pub fn snapshots(&self) -> &BTreeMap<String, GuestState> {
        &self.flow.shared.snaps
    }

    /// Whether the conversation has ended.
    pub fn ended(&self) -> bool {
        self.flow.ended()
    }
}

/// Reads the guest part of a schedule for `plan`, refusing the tokens that are seams.
fn run_cfg(plan: &Plan, s: &Schedule) -> Result<RunCfg, Unsupported> {
    let mut c = RunCfg {
        guest_text: s.guest_text(),
        ..RunCfg::default()
    };
    // (no driven token depends on the plan since T= and B= are; the remaining seams may)
    let _ = plan;
    for t in &s.guest {
        let text = t.to_string();
        let value = text.split_once('=').map(|x| x.1).unwrap_or("").to_string();
        match t {
            GuestToken::Kill { svc, at, down } => {
                c.k = Some((svc.clone(), *at as i64, (*at + *down) as i64));
            }
            GuestToken::Pause(Pause::Steps { svc, at, steps }) => {
                c.p = Some(PauseCfg::Steps {
                    svc: svc.clone(),
                    at: *at as i64,
                    up: (*at + *steps) as i64,
                });
            }
            GuestToken::Pause(Pause::Timer {
                svc,
                at,
                off_ms,
                dur_ms,
            }) => {
                c.p = Some(PauseCfg::Timer {
                    svc: svc.clone(),
                    at: *at as i64,
                    off: *off_ms,
                    dur: *dur_ms,
                });
            }
            GuestToken::Cut(at) => c.c = Some(*at as i64),
            GuestToken::TimedKill { svc, at, off_ms } => {
                c.x = Some((svc.clone(), *at as i64, *off_ms));
            }
            // SEAM(NetFaults): netfault.rs
            GuestToken::Net { .. } => c.n = Some(value),
            GuestToken::OneWay { .. } => c.o = Some(value),
            GuestToken::Freeze(_) if plan.freeze && plan.bridge => {
                // SEAM(Freeze): freeze.rs. With network: bridge the helper runs in the frozen
                // service's namespace; the agent's `freeze` has no namespace field.
                return Err(Unsupported::seam(
                    Seam::Freeze,
                    format!("{text} with network: bridge (hydo-agent-proto's freeze has no netns)"),
                ));
            }
            // SEAM(Clock): T=, driven by features/clock.rs
            GuestToken::Clock { at, delta } => c.t = Some((*at as i64, *delta)),
            // Every other letter is a knob to the shell (`?=*) set_knob`): B= (set_knob's BUGGIFY
            // branch with `buggify: true`, else an unknown knob), and H=, W=, Q= (core plans have
            // no fakes or composer; Q= is the stack's) are knobs the manifest does not have, so
            // they are ignored. F= is a knob too: with `faults: [freeze]` set_knob reads it
            // (hydo-init-freeze.sh), else ignored.
            _ => c.knobs.push((t.letter().to_string(), value)),
        }
    }
    Ok(c)
}

/// busybox mount's `-o`: flags (set or cleared in order) and the file system's own options.
fn mount_opts(opts: &str) -> Result<(Vec<MountFlag>, Option<String>), Unsupported> {
    let mut flags: Vec<MountFlag> = Vec::new();
    let mut data: Vec<&str> = Vec::new();
    let set = |flags: &mut Vec<MountFlag>, f: MountFlag, on: bool| {
        flags.retain(|x| *x != f);
        if on {
            flags.push(f);
        }
    };
    for o in opts.split(',').filter(|o| !o.is_empty()) {
        match o {
            "ro" => set(&mut flags, MountFlag::Ro, true),
            "rw" => set(&mut flags, MountFlag::Ro, false),
            "nosuid" => set(&mut flags, MountFlag::Nosuid, true),
            "suid" => set(&mut flags, MountFlag::Nosuid, false),
            "nodev" => set(&mut flags, MountFlag::Nodev, true),
            "dev" => set(&mut flags, MountFlag::Nodev, false),
            "noexec" => set(&mut flags, MountFlag::Noexec, true),
            "exec" => set(&mut flags, MountFlag::Noexec, false),
            "noatime" => set(&mut flags, MountFlag::Noatime, true),
            "atime" => set(&mut flags, MountFlag::Noatime, false),
            "relatime" => set(&mut flags, MountFlag::Relatime, true),
            "norelatime" => set(&mut flags, MountFlag::Relatime, false),
            "sync" => set(&mut flags, MountFlag::Sync, true),
            "async" => set(&mut flags, MountFlag::Sync, false),
            "remount" => set(&mut flags, MountFlag::Remount, true),
            "bind" => set(&mut flags, MountFlag::Bind, true),
            "defaults" => {}
            "dirsync" | "mand" | "nomand" | "diratime" | "nodiratime" | "strictatime"
            | "nostrictatime" | "lazytime" | "nolazytime" | "silent" | "loud" | "move"
            | "rbind" | "shared" | "slave" | "private" | "unbindable" | "rshared" | "rslave"
            | "rprivate" | "runbindable" | "loop" => {
                return Err(Unsupported::other(format!(
                "data.mount: the option {o} has no agent mount flag (hydo-agent-proto MountFlag)"
            )))
            }
            other => data.push(other),
        }
    }
    let data = (!data.is_empty()).then(|| data.join(","));
    Ok((flags, data))
}

// ------------------------------------------------------------------ Controller

impl Controller for ScheduleController {
    fn name(&self) -> &str {
        "schedule"
    }

    fn step(&mut self, _stop: &StopInfo, seen: &[Seen], out: &mut Out) {
        if self.flow.ended() {
            return;
        }
        let mut new = false;
        for s in seen {
            match s {
                Seen::Hello(_) => {
                    if self.waiting_hello {
                        self.waiting_hello = false;
                        new = true;
                    }
                }
                Seen::Reply { id, body } => {
                    self.facts.replies.insert(*id, Ok(body.clone()));
                    new = true;
                }
                Seen::Refused { id, error } => {
                    self.facts.replies.insert(*id, Err(error.clone()));
                    new = true;
                }
                Seen::Event(Event::Exit(e)) => {
                    self.facts.exits.insert(e.id, e.clone());
                    new = true;
                }
                Seen::Event(Event::Done(d)) => {
                    self.facts.dones.insert(d.id, d.clone());
                    new = true;
                }
                Seen::Event(Event::Probe(p)) => {
                    self.facts.probes.insert(p.id, p.clone());
                    new = true;
                }
                // line, froze, resumed, dropped, warn: used by the seams (tail, freeze)
                Seen::Event(e) => {
                    self.facts.events.push(e.clone());
                    new = true;
                }
                Seen::UnknownEvent(_) | Seen::Console(_) => {}
                Seen::Broken(why) => {
                    let line = format!("HYDO-ORACLE: ERROR agent protocol: {why}");
                    out.report("main", line.clone());
                    out.end(Ending::Verdict(line));
                    self.flow.end_now();
                    return;
                }
            }
        }
        if self.waiting_hello || (self.started && !new) {
            return;
        }
        if !self.started && !self.fresh && out.next_id() > 1 {
            // a resumed conversation began with the runner's sync (the id before ours)
            self.flow.expect_answer(out.next_id() - 1);
        }
        self.started = true;
        self.flow.pump(&self.facts, out, label_of, body);
    }
}

fn label_of(p: &Part) -> String {
    label(p)
}

/// One thread's function.
fn body(fx: &mut F<'_>, part: &Part) -> Step<()> {
    let ctx = fx.shared.ctx.clone();
    match part {
        Part::Main => match &ctx.conv {
            Conversation::Boot => main_boot(fx, &ctx),
            Conversation::Run => explore_run(fx, &ctx),
            Conversation::Recovery { acked, failed } => main_recovery(fx, &ctx, acked, failed),
            Conversation::Solo => main_solo(fx, &ctx),
        },
        Part::Every(i) => periodic_loop(fx, &ctx, *i),
        Part::PauseTimer { svc, off, dur } => pause_timer(fx, &ctx, svc, *off, *dur),
        Part::KillTimer { svc, off } => kill_timer(fx, &ctx, svc, *off),
        Part::Compose(j) => composer::job(fx, &ctx, j),
        Part::Live(m) => liveness::live_monitor(fx, &ctx, m),
        Part::FreezeWatch { id, dur } => freeze::watch(fx, *id, dur),
    }
}

// ------------------------------------------------------------------ commands

fn bb(ctx: &Ctx) -> String {
    ctx.plan.layout.busybox.clone()
}

/// `$BB sh -c "<text>"`.
fn sh(ctx: &Ctx, text: String) -> Vec<String> {
    vec![bb(ctx), "sh".into(), "-c".into(), text]
}

/// RUN(cmd): `$BB sh -c ". /etc/hydo/prelude.sh; <cmd>"`.
fn run_argv(ctx: &Ctx, cmd: &str) -> Vec<String> {
    sh(ctx, format!(". {}; {cmd}", ctx.plan.layout.prelude))
}

/// The workload's call: `$BB timeout <T> $BB sh -c ". prelude; <cmd>"`.
fn timeout_argv(ctx: &Ctx, cmd: &str) -> Vec<String> {
    let mut a = vec![
        bb(ctx),
        "timeout".into(),
        ctx.plan.workload_timeout.to_string(),
    ];
    a.extend(run_argv(ctx, cmd));
    a
}

/// The init's environment (AMBIGUOUS: the kernel also passes unknown command-line words such as
/// hydo_phase=serve to init; they are not reproduced): what the kernel gives init, what the init
/// exports, what it exported later (HYDO_SVC, HYDO_LOG), and the command's own variables. Sorted.
fn env(fx: &F<'_>, ctx: &Ctx, extra: &[(&str, String)]) -> Vec<String> {
    let l = &ctx.plan.layout;
    let mut m: BTreeMap<String, String> = BTreeMap::new();
    m.insert("HOME".into(), "/".into());
    m.insert("TERM".into(), "linux".into());
    m.insert("HYDO_STEPS".into(), ctx.plan.steps.to_string());
    m.insert("HYDO_ACKED".into(), l.acked());
    m.insert("HYDO_OUTCOMES".into(), l.outcomes());
    m.insert("HYDO_FAILED".into(), l.failed());
    for (k, v) in &fx.shared.env {
        m.insert(k.clone(), v.clone());
    }
    for (k, v) in extra {
        m.insert(k.to_string(), v.clone());
    }
    m.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

fn run_cmd(argv: Vec<String>, env: Vec<String>, capture: Option<u32>) -> Command {
    Command::Run(Run {
        argv,
        env: Some(env),
        cwd: None,
        uid: None,
        gid: None,
        netns: None,
        kill_after_ms: None,
        capture,
    })
}

fn spawn_cmd(h: &str, argv: Vec<String>, env: Vec<String>, out: &str, append: bool) -> Command {
    Command::Spawn(Spawn {
        h: h.into(),
        argv,
        env: Some(env),
        cwd: None,
        uid: None,
        gid: None,
        netns: None,
        stdout: Some(out.into()),
        stderr: Some(out.into()),
        append: append.then_some(true),
        kill_after_ms: None,
    })
}

fn signal_cmd(h: &str, sig: Sig) -> Command {
    Command::Signal(Signal {
        h: h.into(),
        sig,
        tree: true,
        wait_stopped: None,
    })
}

fn write_cmd(path: &str, data: &str, append: bool) -> Command {
    Command::Write(Write {
        path: path.into(),
        data: data.into(),
        append: append.then_some(true),
        mode: None,
    })
}

/// The longest file chunk read or written by one command (the agent's limit for `read`, and a
/// host line must stay under 4,000 bytes with its JSON around it).
pub const CHUNK: usize = 3000;

/// Send a command whose answer the shell ignores; the agent carries out commands in order, so the
/// next command still comes after it.
fn post(fx: &mut F<'_>, cmd: Command) -> Step<()> {
    fx.send(cmd)?;
    Ok(())
}

/// Send and wait for the reply.
fn call(fx: &mut F<'_>, cmd: Command) -> Step<Result<Reply, String>> {
    let id = fx.send(cmd)?;
    Ok(fx.reply(id)?.map_err(|e| e.msg))
}

/// What a `run` gave: its exit status and the output kept.
struct Ran {
    status: u32,
    out: String,
}

/// Run a command to its end (the shell's synchronous command). A refused command counts as
/// exit status 127 (the shell's "not found"), with the refusal as its output.
fn run(fx: &mut F<'_>, argv: Vec<String>, env: Vec<String>, capture: Option<u32>) -> Step<Ran> {
    run_in(fx, argv, env, capture, None)
}

/// `run` inside a network namespace (the shell's `$BB nsenter -n<ns> <cmd>`).
fn run_in(
    fx: &mut F<'_>,
    argv: Vec<String>,
    env: Vec<String>,
    capture: Option<u32>,
    netns: Option<String>,
) -> Step<Ran> {
    let mut cmd = run_cmd(argv, env, capture);
    if let Command::Run(r) = &mut cmd {
        r.netns = netns;
    }
    let id = fx.send(cmd)?;
    match fx.reply(id)? {
        Err(e) => Ok(Ran {
            status: 127,
            out: e.msg,
        }),
        Ok(_) => {
            let d = fx.done(id)?;
            Ok(Ran {
                status: d.status,
                out: d.out,
            })
        }
    }
}

/// Start a process with its output in a file and wait for its exit (a shell command with `>`
/// or `>>`); its exit status (127 when the agent refused it).
fn spawn_wait(
    fx: &mut F<'_>,
    h: &str,
    argv: Vec<String>,
    env: Vec<String>,
    out: &str,
    append: bool,
) -> Step<u32> {
    spawn_wait_in(fx, h, argv, env, out, append, None)
}

/// `spawn_wait` inside a network namespace.
fn spawn_wait_in(
    fx: &mut F<'_>,
    h: &str,
    argv: Vec<String>,
    env: Vec<String>,
    out: &str,
    append: bool,
    netns: Option<String>,
) -> Step<u32> {
    let id = fx.send(netns::put(spawn_cmd(h, argv, env, out, append), &netns))?;
    match fx.reply(id)? {
        Err(_) => Ok(127),
        Ok(_) => Ok(fx.exit(id)?.status),
    }
}

/// A whole file (chunked reads; a missing or unreadable file reads as empty, as the shell's
/// `< file` with its error on stderr does).
fn read_all(fx: &mut F<'_>, path: &str) -> Step<String> {
    let mut text = String::new();
    loop {
        let r = call(
            fx,
            Command::Read(Read {
                path: path.into(),
                offset: (!text.is_empty()).then_some(text.len() as u64),
                max: CHUNK as u32,
                hex: None,
            }),
        )?;
        match r {
            Ok(Reply::Read(r)) => {
                let done = r.eof || r.data.is_empty();
                text.push_str(&r.data);
                if done {
                    return Ok(text);
                }
            }
            _ => return Ok(text),
        }
    }
}

/// Text cut into pieces of at most CHUNK bytes, each ending at a line end when it can.
fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        if !cur.is_empty() && cur.len() + line.len() > CHUNK {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(line);
        while cur.len() > CHUNK {
            let mut i = CHUNK;
            while !cur.is_char_boundary(i) {
                i -= 1;
            }
            let rest = cur.split_off(i);
            out.push(std::mem::replace(&mut cur, rest));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn log(fx: &mut F<'_>, line: impl AsRef<str>) {
    fx.note(format!("hydo-ob: {}", line.as_ref()))
}

/// Whether the service's current process is running, as far as the host knows: it was spawned
/// (the reply may not be there yet), the agent did not refuse the spawn, and no exit was seen.
fn running(h: &Host, f: &Facts, svc: &str) -> bool {
    match h.services.get(svc) {
        None => false,
        Some(s) => {
            !matches!(f.replies.get(&s.spawn), Some(Err(_))) && !f.exits.contains_key(&s.spawn)
        }
    }
}

fn guest_state(h: &Host, f: &Facts) -> GuestState {
    GuestState {
        running: h
            .services
            .iter()
            .filter(|(n, _)| running(h, f, n))
            .map(|(n, s)| (n.clone(), s.spawn))
            .collect(),
        started: h.started.clone(),
        env: h.env.clone(),
    }
}

fn snap(fx: &mut F<'_>, name: &str) -> Step<()> {
    fx.settle()?;
    fx.effect(|h, f| {
        let g = guest_state(h, f);
        h.snaps.insert(name.to_string(), g);
        Memo::Unit
    });
    fx.snapshot(name);
    Ok(())
}

// ------------------------------------------------------------------ text, as the shell writes it

/// jesc: `tr -d '\000-\037' | sed 's/\\/\\\\/g; s/"/\\"/g'`.
pub fn jesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{0}'..='\u{1f}' => {}
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            c => o.push(c),
        }
    }
    o
}

/// The lines grep sees.
fn lines(s: &str) -> Vec<&str> {
    let mut v: Vec<&str> = s.split('\n').collect();
    if s.ends_with('\n') || s.is_empty() {
        v.pop();
    }
    v
}

/// inv_detail: the invariant's own HYDO-ORACLE line (prefix and OK/VIOLATION stripped), or its
/// last line that is not blank and not an assertion record.
pub fn inv_detail(out: &str) -> String {
    let ls = lines(out);
    if let Some(l) = ls.iter().rev().find(|l| l.contains("HYDO-ORACLE:")) {
        let i = l.rfind("HYDO-ORACLE:").unwrap_or(0);
        let mut d = l[i + "HYDO-ORACLE:".len()..].trim_start_matches(' ');
        if let Some(r) = d.strip_prefix("OK") {
            d = r.trim_start_matches(' ');
        }
        if let Some(r) = d.strip_prefix("VIOLATION") {
            d = r.trim_start_matches(' ');
        }
        if !d.is_empty() {
            return d.to_string();
        }
    }
    ls.iter()
        .rev()
        .find(|l| !l.chars().all(|c| c == ' ') && !l.starts_with("HYDO-ASSERT"))
        .map(|l| l.to_string())
        .unwrap_or_default()
}

/// failed_ids' awk and `sort -n`: the ids whose last outcome is `failed`, one per line.
pub fn failed_of(outcomes: &str) -> String {
    let mut o: BTreeMap<String, String> = BTreeMap::new();
    for l in lines(outcomes) {
        let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
        o.insert(
            f.first().copied().unwrap_or("").to_string(),
            f.get(1).copied().unwrap_or("").to_string(),
        );
    }
    let mut ids: Vec<String> = o
        .into_iter()
        .filter(|(_, v)| v == "failed")
        .map(|(k, _)| k)
        .collect();
    ids.sort_by(|a, b| {
        num_key(a)
            .partial_cmp(&num_key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
    ids.iter().map(|i| format!("{i}\n")).collect()
}

/// sort -n's key: the number at the start of the line (blanks skipped), 0 when there is none.
fn num_key(s: &str) -> f64 {
    let t = s.trim_start_matches([' ', '\t']);
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && b[i] == b'-' {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    t[..i].parse().unwrap_or(0.0)
}

/// `date -u +%Y-%m-%dT%H:%M:%SZ` of an epoch.
pub fn utc(sec: i64) -> String {
    let days = sec.div_euclid(86400);
    let s = sec.rem_euclid(86400);
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// `$BB sleep` of ms as the shell's awk prints it (`%.6f` or `%.3f` of ms/1000).
pub fn secs(ms: u64, places: u32) -> String {
    match places {
        6 => format!("{}.{:06}", ms / 1000, (ms % 1000) * 1000),
        _ => format!("{}.{:03}", ms / 1000, ms % 1000),
    }
}

fn decl(t: &str, id: &str, msg: &str) -> String {
    format!("HYDO-ASSERT-DECL {{\"t\":\"{t}\",\"id\":\"{id}\",\"msg\":\"{msg}\"}}")
}

/// xassert: `HYDO-ASSERT {"t":..,"id":..,"ok":..,"msg":..}` (not escaped, as the shell).
fn xassert(fx: &mut F<'_>, t: &str, id: &str, msg: &str) {
    fx.report(format!(
        "HYDO-ASSERT {{\"t\":\"{t}\",\"id\":\"{id}\",\"ok\":true,\"msg\":\"{msg}\"}}"
    ));
}

// ------------------------------------------------------------------ the init's functions

/// die_oracle: stop the checkers, the verdict line, the end.
fn die_oracle<T>(fx: &mut F<'_>, line: String) -> Step<T> {
    let ctx = fx.shared.ctx.clone();
    errlog::report(fx, &ctx)?; // SEAM(ErrorLogs): the records before the verdict
                               // SEAM(Freeze): stop_periodic's wrapper first stops the freeze helper (fc_kill)
    freeze::kill(fx)?;
    stop_periodic(fx);
    fx.report(line.clone());
    fx.settle()?;
    fx.end(Ending::Verdict(line))
}

/// The start of every boot: `mkdir -p $H`, the empty acked/outcomes/failed files, `lo` up, the
/// phase line, declare_all. (The agent mounted /dev, /proc, /sys, /tmp and ran hvx_seedrng.)
fn boot_prelude(fx: &mut F<'_>, ctx: &Ctx, phase: &str) -> Step<()> {
    let l = &ctx.plan.layout;
    post(
        fx,
        Command::Mkdir(Mkdir {
            path: l.dir.clone(),
            mode: None,
        }),
    )?;
    for f in [l.acked(), l.outcomes(), l.failed()] {
        post(fx, write_cmd(&f, "", false))?;
    }
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![
            bb(ctx),
            "ifconfig".into(),
            "lo".into(),
            "127.0.0.1".into(),
            "netmask".into(),
            "255.0.0.0".into(),
            "up".into(),
        ],
        e.clone(),
        None,
    )?;
    run(
        fx,
        vec![
            bb(ctx),
            "ip".into(),
            "link".into(),
            "set".into(),
            "lo".into(),
            "up".into(),
        ],
        e,
        None,
    )?;
    let names: Vec<&str> = ctx.plan.services.iter().map(|s| s.name.as_str()).collect();
    log(
        fx,
        format!(
            "phase={phase} services=[{}] steps={}",
            names.join(" "),
            ctx.plan.steps
        ),
    );
    // SEAM(NetNamespaces): hydo-init-netns.sh sets the service network up where /sbin/init
    // reaches it, after the init's functions and before main (declare_all, S0)
    netns::setup(fx, ctx)?;
    declare_all(fx, ctx);
    Ok(())
}

fn declare_all(fx: &mut F<'_>, ctx: &Ctx) {
    for i in &ctx.plan.invariants {
        fx.report(decl(&i.kind, &i.id, &jesc(&i.msg)));
    }
}

/// `$BB date -s @$HYDO_CLOCK_START`, with its log line in serve mode.
fn clock_start(fx: &mut F<'_>, ctx: &Ctx, with_log: bool) -> Step<()> {
    let r = call(
        fx,
        Command::ClockSet(ClockSet {
            sec: Some(ctx.plan.clock_start),
            step: None,
        }),
    )?;
    if let (true, Ok(Reply::ClockSet(c))) = (with_log, r) {
        log(fx, format!("realtime clock set to {}", utc(c.sec)));
    }
    Ok(())
}

fn mount_data(fx: &mut F<'_>, ctx: &Ctx, verdict: &str) -> Step<()> {
    let l = &ctx.plan.layout;
    let id = fx.send(Command::Mount(Mount {
        src: l.data_dev.clone(),
        dst: l.data_dir.clone(),
        fstype: l.data_fstype.clone(),
        flags: (!ctx.mount_flags.is_empty()).then(|| ctx.mount_flags.clone()),
        data: ctx.mount_data.clone(),
    }))?;
    if let Err(e) = fx.reply(id)? {
        // The shell puts busybox mount's own error text here (M127): for an errno that musl's
        // strerror has a text for, the same words; otherwise (AMBIGUOUS) the agent's refusal,
        // which names the system call and its errno.
        let text = e
            .errno
            .and_then(|n| busybox_mount_error(&l.data_dev, &l.data_dir, n))
            .unwrap_or(e.msg);
        return die_oracle(fx, format!("{verdict} ({text})"));
    }
    for d in &ctx.plan.data_dirs {
        post(
            fx,
            Command::Mkdir(Mkdir {
                path: format!("{}/{d}", l.data_dir),
                mode: None,
            }),
        )?;
    }
    log(
        fx,
        format!(
            "mounted {} on {} ({})",
            l.data_dev, l.data_dir, ctx.plan.data_opts
        ),
    );
    errlog::after_mount(fx, ctx) // SEAM(ErrorLogs): the data-disk files' line counts
}

/// What the guest's busybox `mount` (static, musl) prints on stderr when mount(2) fails with
/// `errno` (busybox `bb_perror_msg("mounting %s on %s failed")` with musl's `strerror` text): the
/// text hydo-init.sh's mount_data puts in its verdict. M127 found the difference on the Redis
/// example's lying-disk schedules (`Data consistency error`, EUCLEAN). None for an errno that is
/// not in this table (the Linux errno numbers of x86_64 and aarch64).
fn busybox_mount_error(src: &str, dst: &str, errno: i64) -> Option<String> {
    // musl 1.2's src/errno/__strerror.h, for the errors a mount of a block device can give
    let text = match errno {
        1 => "Operation not permitted",
        2 => "No such file or directory",
        5 => "I/O error",
        6 => "No such device or address",
        12 => "Out of memory",
        13 => "Permission denied",
        15 => "Block device required",
        16 => "Resource busy",
        19 => "No such device",
        20 => "Not a directory",
        22 => "Invalid argument",
        24 => "No file descriptors available",
        28 => "No space left on device",
        30 => "Read-only file system",
        74 => "Bad message",
        117 => "Data consistency error",
        _ => return None,
    };
    Some(format!("mount: mounting {src} on {dst} failed: {text}"))
}

/// start_svc: the old log appended to .log.old, setup, the service, its readiness. Ok(false) when
/// it did not come up.
fn start_svc(fx: &mut F<'_>, ctx: &Ctx, s: &str) -> Step<bool> {
    let l = &ctx.plan.layout;
    let sv = ctx.plan.service(s).expect("a service of the plan");
    let log_file = l.log_of(s);
    // SEAM(NetNamespaces): with network: bridge, setup, the command and the readiness check run
    // in the service's namespace (hydo-init-netns.sh's start_svc puts nsenter into $BB)
    let ns = netns::of(ctx, s);
    let again = fx.effect_bool(|h, _| !h.started.insert(s.to_string()));
    if again {
        // `[ -f $L ] && cat $L >> $H/$s.log.old && : > $L` (the spawn below truncates $L)
        let e = env(fx, ctx, &[]);
        run_in(
            fx,
            sh(ctx, format!("{} cat {log_file} >> {log_file}.old", bb(ctx))),
            e,
            None,
            ns.clone(),
        )?;
    }
    let lf = log_file.clone();
    fx.effect(|h, _| {
        h.env.insert("HYDO_SVC".into(), s.to_string());
        h.env.insert("HYDO_LOG".into(), lf);
        Memo::Unit
    });
    if !sv.setup.is_empty() {
        let e = env(fx, ctx, &[]);
        let st = spawn_wait_in(
            fx,
            &format!("setup:{s}"),
            run_argv(ctx, &sv.setup),
            e,
            &format!("{}/{s}.setup.log", l.dir),
            true,
            ns.clone(),
        )?;
        if st != 0 {
            // AMBIGUOUS: the shell also prints the last 8 lines of the setup log here.
            log(fx, format!("{s} setup FAILED"));
            return Ok(false);
        }
    }
    // AMBIGUOUS: the shell starts the new process at once; the agent refuses a handle whose
    // process has not exited, so the controller waits for the exit of a killed service first.
    fx.until(|h, f| !running(h, f, s))?;
    let e = env(fx, ctx, &[]);
    let id = fx.send(netns::put(
        spawn_cmd(
            s,
            sh(ctx, format!(". {}; exec {}", l.prelude, sv.cmd)),
            e.clone(),
            &log_file,
            false,
        ),
        &ns,
    ))?;
    fx.effect(|h, _| {
        h.services.insert(
            s.to_string(),
            Svc {
                spawn: id,
                killed: false,
            },
        );
        Memo::Unit
    });
    if fx.reply(id)?.is_err() {
        log(fx, format!("{s} DIED"));
        return Ok(false);
    }
    // the readiness loop: `ready` once a second, at most `timeout` times, while the service
    // lives (AMBIGUOUS: the shell starts poll k one second after poll k-1 ENDED; the agent at k
    // seconds after the start, or when poll k-1 ended if later)
    let pid = fx.send(Command::Probe(Probe {
        argv: run_argv(ctx, &sv.ready),
        env: Some(e),
        cwd: None,
        uid: None,
        gid: None,
        netns: ns,
        every_ms: 1000,
        deadline_ms: (sv.timeout.max(0) as u64) * 1000,
        alive: Some(s.to_string()),
    }))?;
    if fx.reply(pid)?.is_err() {
        log(fx, format!("{s} NOT ready after {}s", sv.timeout));
        return Ok(false);
    }
    let p = fx.probe_done(pid)?;
    match p.why {
        ProbeEnd::Ok => {
            // AMBIGUOUS: the shell also prints its uptime here; the host has no such clock.
            log(
                fx,
                format!("{s} ready after {} polls", p.tries.saturating_sub(1)),
            );
            Ok(true)
        }
        ProbeEnd::Died => {
            log(fx, format!("{s} DIED"));
            Ok(false)
        }
        ProbeEnd::Deadline => {
            log(fx, format!("{s} NOT ready after {}s", sv.timeout));
            Ok(false)
        }
    }
}

fn start_all(fx: &mut F<'_>, ctx: &Ctx, verdict: &str, recovery: bool) -> Step<()> {
    fakes::start_all(fx, ctx)?; // SEAM(Fakes): the world starts before the services
                                // SEAM(Liveness): the recovery boot reaches both anchors before its services start
    liveness::before_start_all(fx, ctx, recovery)?;
    for s in &ctx.plan.services {
        if recovery && !s.recovery {
            continue;
        }
        if !start_svc(fx, ctx, &s.name)? {
            return die_oracle(fx, format!("{verdict} ({})", s.name));
        }
    }
    Ok(())
}

fn restart_dead(fx: &mut F<'_>, ctx: &Ctx, verdict: &str) -> Step<()> {
    fakes::restart_dead(fx, ctx)?; // SEAM(Fakes): the protocol faults healed, once
    freeze::heal(fx, ctx)?; // SEAM(Freeze): fc_heal, after the fakes heal (only its first call acts)
                            // AMBIGUOUS: the shell's `kill -0` may still see a process it just killed (a zombie not yet
                            // reaped). The controller first waits until every service it killed has reported its exit,
                            // so a killed service always counts as down here.
    fx.until(|h, f| {
        h.services.values().filter(|s| s.killed).all(|s| {
            f.exits.contains_key(&s.spawn) || matches!(f.replies.get(&s.spawn), Some(Err(_)))
        })
    })?;
    for s in &ctx.plan.services {
        let alive = fx.effect_bool(|h, f| running(h, f, &s.name));
        if alive {
            continue;
        }
        log(fx, format!("{} is down; restarting it", s.name));
        if !start_svc(fx, ctx, &s.name)? {
            return die_oracle(fx, format!("{verdict} ({})", s.name));
        }
    }
    Ok(())
}

/// Send a signal to a service's whole tree; the answer is ignored (`kill ... 2>/dev/null`).
fn svc_sig(fx: &mut F<'_>, s: &str, sig: Sig) -> Step<()> {
    post(fx, signal_cmd(s, sig))?;
    if sig == Sig::Kill {
        fx.effect(|h, _| {
            if let Some(v) = h.services.get_mut(s) {
                v.killed = true;
            }
            Memo::Unit
        });
    }
    Ok(())
}

/// assert_once: one HYDO-ASSERT record per (invariant, outcome).
fn assert_once(fx: &mut F<'_>, ctx: &Ctx, i: usize, ok: bool, details: &str) {
    let inv = &ctx.plan.invariants[i];
    let key = inv.key.clone();
    if fx.effect_bool(|h, _| h.asserted.insert((key, ok))) {
        // SEAM(ErrorLogs): held back while final_verdict's output goes after the error records
        errlog::assert_line(fx, format!(
            "HYDO-ASSERT {{\"t\":\"{}\",\"id\":\"{}\",\"ok\":{ok},\"msg\":\"{}\",\"details\":{{\"out\":\"{}\"}}}}",
            inv.kind,
            inv.id,
            jesc(&inv.msg),
            jesc(details)
        ));
    }
}

/// eval_inv: 0 holds, 1 violated, 2 could not be evaluated; also its output.
fn eval_inv(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<(u32, String)> {
    let inv = &ctx.plan.invariants[i];
    let e = env(fx, ctx, &[]);
    // `RUN cmd > $H/inv.$i.out 2>&1`; the output is kept host-side (AMBIGUOUS: an output longer
    // than 3,000 bytes is cut to its first 3,000 here; the shell's file keeps all of it)
    // SEAM(Composer): compose.sh verdict | end IDX | check IDX
    let r = match composer::eval(fx, ctx, &inv.cmd)? {
        Some(r) => r,
        None => run(fx, run_argv(ctx, &inv.cmd), e, Some(CHUNK as u32))?,
    };
    let key = inv.key.clone();
    let out = r.out.clone();
    fx.effect(|h, _| {
        h.last_out.insert(key, out);
        Memo::Unit
    });
    let d = inv_detail(&r.out);
    match r.status {
        0 => {
            assert_once(fx, ctx, i, true, &d);
            Ok((0, r.out))
        }
        2 => Ok((2, r.out)),
        _ => {
            assert_once(fx, ctx, i, false, &d);
            let key = inv.key.clone();
            fx.effect(|h, _| {
                if h.violated.is_none() {
                    h.violated = Some(key);
                }
                Memo::Unit
            });
            Ok((1, r.out))
        }
    }
}

/// periodic_loop: every `every` seconds of guest time while the workload runs.
fn periodic_loop(fx: &mut F<'_>, ctx: &Ctx, i: usize) -> Step<()> {
    let every = ctx.plan.invariants[i].every.clone();
    loop {
        let e = env(fx, ctx, &[]);
        run(fx, vec![bb(ctx), "sleep".into(), every.clone()], e, None)?;
        eval_inv(fx, ctx, i)?;
    }
}

fn start_periodic(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let mut ts = Vec::new();
    for (i, inv) in ctx.plan.invariants.iter().enumerate() {
        if inv.every.is_empty() {
            continue;
        }
        ts.push(fx.start(Part::Every(i))? as u64);
    }
    fx.effect(|h, _| {
        h.periodic.extend(ts);
        Memo::Unit
    });
    // SEAM(Liveness): the `start` anchor (hydo-init-history.sh wraps liveness's start_periodic)
    liveness::after_start_periodic(fx, ctx)?;
    history::declare(fx, ctx); // SEAM(History): the HYDO-CHECK lines
    Ok(())
}

/// stop_periodic: the checkers stop (an evaluation the host has already seen still counts).
fn stop_periodic(fx: &mut F<'_>) {
    let ts = fx.effect_text(|h, _| {
        h.periodic
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(",")
    });
    for t in ts.split(',').filter(|t| !t.is_empty()) {
        fx.cancel(t.parse().expect("a thread number"));
    }
}

/// final_verdict: every `final` invariant in manifest order, then one HYDO-ORACLE line.
fn final_verdict(fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
    fakes::final_verdict(fx, ctx)?; // SEAM(Fakes): what the outside world saw
    let mut nheld = 0;
    let mut first = String::new();
    for (i, inv) in ctx.plan.invariants.iter().enumerate() {
        if !inv.final_ {
            continue;
        }
        let (rc, out) = eval_inv(fx, ctx, i)?;
        match rc {
            0 => {
                nheld += 1;
                if first.is_empty() {
                    first = format!("{}: {}", inv.id, inv_detail(&out));
                }
            }
            2 => {
                let line = format!(
                    "HYDO-ORACLE: ERROR invariant {} could not be evaluated: {}",
                    inv.id,
                    inv_detail(&out)
                );
                fx.effect(|h, _| {
                    if h.violated.is_none() {
                        h.error = Some(line);
                    }
                    Memo::Unit
                });
            }
            _ => {}
        }
    }
    Ok(fx.effect_text(|h, _| {
        if let Some(v) = &h.violated {
            let id = h
                .ctx
                .plan
                .invariants
                .iter()
                .find(|x| x.key == *v)
                .map(|x| x.id.clone())
                .unwrap_or_default();
            let out = h.last_out.get(v).cloned().unwrap_or_default();
            format!("HYDO-ORACLE: VIOLATION [{id}] {}", inv_detail(&out))
        } else if let Some(e) = &h.error {
            e.clone()
        } else {
            format!("HYDO-ORACLE: OK invariants={nheld} held ({first})")
        }
    }))
}

/// finish: the final invariants, the verdict, the end of the conversation.
fn finish(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    // SEAM(History): the final: read-backs (history's finish wraps liveness's)
    history::finals(fx, ctx)?;
    // SEAM(Liveness): every eventually invariant is resolved first
    liveness::before_finish(fx, ctx)?;
    let v = errlog::final_verdict(fx, ctx)?; // SEAM(ErrorLogs): wraps final_verdict
    fx.report(v.clone());
    fx.settle()?;
    fx.end(Ending::Verdict(v))
}

/// step_range: workload steps [a, b), one call bounded by workload.timeout.
fn step_range(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    // SEAM(ErrorLogs): el_mark <first step>, once around the fakes' split
    errlog::mark(fx, ctx, &a.to_string())?;
    // SEAM(Fakes): split at the H= windows' boundaries, switching the rules there; each piece
    // goes through the wrappers spliced before hydo-init-fakes.sh (freeze's)
    if let Some(r) = fakes::step_range(fx, ctx, a, b, step_range_fc) {
        return r;
    }
    step_range_fc(fx, ctx, a, b)
}

/// hydo-init-freeze.sh's step_range around hydo-init.sh's own.
fn step_range_fc(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    if ctx.plan.freeze {
        // SEAM(Freeze): the declaration, the arming
        return freeze::step_range(fx, ctx, a, b);
    }
    step_range_core(fx, ctx, a, b)
}

/// hydo-init.sh's own step_range.
fn step_range_core(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    if a < b {
        step_range_run(fx, ctx, a, b)?;
    }
    // SEAM(Liveness): the `heal` anchor after the last step (also for an empty range)
    liveness::after_step_range(fx, ctx, b)
}

fn step_range_run(fx: &mut F<'_>, ctx: &Ctx, a: i64, b: i64) -> Step<()> {
    let st = if ctx.plan.compose.is_some() {
        composer::step(fx, ctx, a, b)? // SEAM(Composer): compose.sh step FROM TO
    } else {
        let e = env(
            fx,
            ctx,
            &[("HYDO_FROM", a.to_string()), ("HYDO_TO", b.to_string())],
        );
        spawn_wait(
            fx,
            "workload",
            timeout_argv(ctx, &ctx.plan.workload_run),
            e,
            &format!("{}/workload.log", ctx.plan.layout.dir),
            true,
        )?
    };
    if st == 143 {
        log(
            fx,
            format!(
                "workload steps {a}..{} TIMED OUT after {}s (killed)",
                b - 1,
                ctx.plan.workload_timeout
            ),
        );
    }
    log(fx, format!("workload steps {a}..{} rc={st}", b - 1));
    Ok(())
}

fn retry_workload(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    errlog::mark(fx, ctx, "end")?; // SEAM(ErrorLogs): el_mark end
    if ctx.plan.workload_retry.is_empty() {
        return Ok(());
    }
    let e = env(fx, ctx, &[]);
    let st = spawn_wait(
        fx,
        "retry",
        timeout_argv(ctx, &ctx.plan.workload_retry),
        e,
        &format!("{}/workload.log", ctx.plan.layout.dir),
        true,
    )?;
    log(fx, format!("workload retry rc={st}"));
    Ok(())
}

/// failed_ids: the ids whose last outcome is `failed`, written to $HYDO_FAILED; its text.
fn failed_ids(fx: &mut F<'_>, ctx: &Ctx) -> Step<String> {
    let l = &ctx.plan.layout;
    let outcomes = read_all(fx, &l.outcomes())?;
    let failed = failed_of(&outcomes);
    // `... | sort -n > $HYDO_FAILED`
    let parts = chunks(&failed);
    post(
        fx,
        write_cmd(
            &l.failed(),
            parts.first().map(String::as_str).unwrap_or(""),
            false,
        ),
    )?;
    for p in parts.iter().skip(1) {
        post(fx, write_cmd(&l.failed(), p, true))?;
    }
    Ok(failed)
}

/// provision: the provisioning command, sync, the setup marker.
fn provision(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    if !ctx.plan.provision.is_empty() {
        // SEAM(Composer): `( own ) && compose.sh first`
        let st = match composer::provision(fx, ctx)? {
            Some(st) => st,
            None => {
                let e = env(fx, ctx, &[]);
                spawn_wait(
                    fx,
                    "provision",
                    run_argv(ctx, &ctx.plan.provision),
                    e,
                    &format!("{}/provision.log", ctx.plan.layout.dir),
                    false,
                )?
            }
        };
        if st != 0 {
            // AMBIGUOUS: the shell also prints the last 8 lines of provision.log here.
            return die_oracle(fx, "HYDO-ORACLE: ERROR provision failed".into());
        }
    }
    let _ = call(fx, Command::Syncfs(Syncfs {}))?;
    log(fx, "provisioning synced");
    fx.report("HYDO-SETUP-COMPLETE");
    Ok(())
}

/// set_knob: a manifest knob's `set` command with HYDO_VALUE, unless the value is the default.
fn set_knob(fx: &mut F<'_>, ctx: &Ctx, letter: &str, value: &str) -> Step<()> {
    if letter == "W" && ctx.plan.compose.is_some() {
        return composer::set_knob(fx, ctx, value); // SEAM(Composer): compose.sh setw
    }
    if fakes::set_knob(fx, ctx, letter, value)? {
        return Ok(()); // SEAM(Fakes): H= is pf_add
    }
    // SEAM(Buggify): hydo-init-buggify.sh's set_knob
    if letter == "B" && ctx.plan.buggify {
        return buggify::set_buggify(fx, value);
    }
    if letter == "F" && ctx.plan.freeze {
        // SEAM(Freeze): hydo-init-freeze.sh's set_knob reads F=
        return freeze::parse_knob(fx, value);
    }
    let k = ctx.plan.knob(letter).filter(|k| !k.default.is_empty());
    let Some(k) = k else {
        log(fx, format!("knob {letter} not in the manifest, ignored"));
        return Ok(());
    };
    if value == k.default {
        return Ok(());
    }
    let e = env(fx, ctx, &[("HYDO_VALUE", value.to_string())]);
    let r = run(fx, run_argv(ctx, &k.set), e, None)?;
    if r.status != 0 {
        return die_oracle(
            fx,
            format!("HYDO-ORACLE: ERROR knob {letter}={value} failed"),
        );
    }
    log(fx, format!("knob {letter}={value} ({})", k.name));
    Ok(())
}

/// `pgrep -f "$HYDO_WORKLOAD_PGREP"`: whether a client request is in flight.
fn in_flight(fx: &mut F<'_>, ctx: &Ctx, e: Vec<String>) -> Step<bool> {
    if ctx.plan.compose.is_some() {
        // SEAM(Composer): `compose.sh run1` processes, i.e. composed commands running
        return Ok(composer::in_flight(fx));
    }
    let argv = vec![
        bb(ctx),
        "pgrep".into(),
        "-f".into(),
        ctx.plan.workload_pgrep.clone(),
    ];
    Ok(run(fx, argv, e, None)?.status == 0)
}

/// pause_timer: after off ms, stop the service (asserting whether a request was in flight),
/// for dur ms.
fn pause_timer(fx: &mut F<'_>, ctx: &Ctx, svc: &str, off: u64, dur: u64) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(off, 6)],
        e.clone(),
        None,
    )?;
    let mid = in_flight(fx, ctx, e.clone())?;
    svc_sig(fx, svc, Sig::Stop)?;
    log(fx, format!("PAUSED {svc} (timer, mid_request={mid})"));
    xassert(
        fx,
        "reachable",
        "fault.paused",
        "a service was paused (SIGSTOP)",
    );
    if mid {
        xassert(
            fx,
            "sometimes",
            "fault.pause_mid_request",
            "a pause began while a client request was in flight",
        );
    }
    run(fx, vec![bb(ctx), "sleep".into(), secs(dur, 3)], e, None)?;
    svc_sig(fx, svc, Sig::Cont)?;
    log(fx, format!("RESUMED {svc} (timer)"));
    Ok(())
}

/// The stack's kill_timer (hydo/explore/stack-explore.sh) for X=: after off ms, kill the
/// service's tree (asserting whether a request was in flight). hydo-init.sh has no X= (it reads
/// it as an unknown knob and the explorer never draws it for a manifest); the controller gives
/// it the stack's meaning, and the service is restarted by restart_dead at the end.
fn kill_timer(fx: &mut F<'_>, ctx: &Ctx, svc: &str, off: u64) -> Step<()> {
    let e = env(fx, ctx, &[]);
    run(
        fx,
        vec![bb(ctx), "sleep".into(), secs(off, 3)],
        e.clone(),
        None,
    )?;
    let mid = in_flight(fx, ctx, e)?;
    svc_sig(fx, svc, Sig::Kill)?;
    log(fx, format!("KILLED {svc} (timer, mid_request={mid})"));
    xassert(fx, KILL_TIMER_DECL.0, KILL_TIMER_DECL.1, KILL_TIMER_DECL.2);
    Ok(())
}

// ------------------------------------------------------------------ the conversations

fn main_boot(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    boot_prelude(fx, ctx, "serve")?;
    snap(fx, "S0")?;
    log(fx, "command: provision");
    clock_start(fx, ctx, true)?;
    mount_data(fx, ctx, "HYDO-ORACLE: ERROR mount data")?;
    start_all(fx, ctx, "HYDO-ORACLE: ERROR service start", false)?;
    provision(fx, ctx)?;
    snap(fx, "S1")?;
    fx.end(Ending::AtSnapshot("S1".into()))
}

fn main_solo(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    boot_prelude(fx, ctx, "solo")?;
    clock_start(fx, ctx, false)?;
    mount_data(fx, ctx, "HYDO-ORACLE: ERROR mount data")?;
    start_all(fx, ctx, "HYDO-ORACLE: ERROR service start", false)?;
    provision(fx, ctx)?;
    start_periodic(fx, ctx)?;
    step_range(fx, ctx, 0, ctx.plan.steps)?;
    retry_workload(fx, ctx)?;
    stop_periodic(fx);
    failed_ids(fx, ctx)?;
    finish(fx, ctx)
}

fn main_recovery(fx: &mut F<'_>, ctx: &Ctx, acked: &[String], failed: &[String]) -> Step<()> {
    let l = &ctx.plan.layout;
    // AMBIGUOUS: the shell logs the oracle command line the engine typed; the engine's exact
    // text is not the controller's business, so the ids are logged in this form.
    let mut cmd = String::from("oracle");
    let mut words = fakes::recovery_words(fx, ctx, acked);
    // SEAM(Clock): the shell's oracle command carries the first boot's clock after the ids and
    // before the carried world's records (the order of the shell's acked file)
    if let Some(t) = clock::oracle_token(ctx) {
        words.insert(acked.len().min(words.len()), t);
    }
    for id in &words {
        cmd.push(' ');
        cmd.push_str(id);
    }
    if !failed.is_empty() {
        cmd.push_str(" failed");
        for id in failed {
            cmd.push(' ');
            cmd.push_str(id);
        }
    }
    log(fx, format!("command: {cmd}"));
    clock_start(fx, ctx, true)?;
    // SEAM(RecoveryInputs): the ids as files, as the oracle command's loop writes them
    // (`echo $id >> $dst`); the carried values are fields of RecoveryInputs, not ids, so no
    // `clock=` line is written (the net effect of hydo-init-clock.sh's `sed -i '/^clock=/d'`).
    for (file, ids) in [(l.acked(), acked), (l.failed(), failed)] {
        // SEAM(Fakes): the world records stay out of the id files (fakes::start_all carries them)
        let text: String = fakes::ids_of(ctx, ids)
            .iter()
            .map(|i| format!("{i}\n"))
            .collect();
        for c in chunks(&text) {
            post(fx, write_cmd(&file, &c, true))?;
        }
    }
    // SEAM(Clock): hydo-init-clock.sh's mount_data: the clock continues from the first boot's
    clock::recovery_clock(fx, ctx)?;
    mount_data(fx, ctx, "HYDO-ORACLE: VIOLATION data disk unmountable")?;
    fx.note("hydo-stack: recovering from log");
    start_all(
        fx,
        ctx,
        "HYDO-ORACLE: VIOLATION service cannot recover (committed data unrecoverable)",
        true,
    )?;
    finish(fx, ctx)
}

/// explore_run: the schedule's guest part from S1.
fn explore_run(fx: &mut F<'_>, ctx: &Ctx) -> Step<()> {
    let plan = &ctx.plan;
    let cfg = &ctx.run;
    let l = &plan.layout;
    let steps = plan.steps;
    log(fx, format!("command: {}", cfg.guest_text));
    for (letter, value) in &cfg.knobs {
        set_knob(fx, ctx, letter, value)?;
    }
    for (t, id, msg) in FAULT_DECLS {
        fx.report(decl(t, id, msg));
    }
    if cfg.x.is_some() {
        let (t, id, msg) = KILL_TIMER_DECL;
        fx.report(decl(t, id, msg));
    }
    // a token naming a service the manifest does not have is ignored
    let known = |s: &str| plan.service(s).is_some();
    let (mut ksvc, mut kat, mut kup) = (String::new(), -1i64, -1i64);
    if let Some((s, at, up)) = &cfg.k {
        ksvc = s.clone();
        (kat, kup) = (*at, *up);
        if !known(s) {
            log(fx, format!("K: no service {s}, ignored"));
            (kat, kup) = (-1, -1);
        }
    }
    // SEAM(NetFaults): N= is read here (nsvc, nfrom, nto), with its "N: no service" line.
    let mut net = netfault::Net::default();
    net.read_n(fx, ctx, cfg.n.as_deref());
    let cat = cfg.c.unwrap_or(-1);
    let (mut psvc, mut pat, mut pup, mut ptime) = (String::new(), -1i64, -1i64, None);
    match &cfg.p {
        Some(PauseCfg::Steps { svc, at, up }) => {
            psvc = svc.clone();
            (pat, pup) = (*at, *up);
        }
        Some(PauseCfg::Timer { svc, at, off, dur }) => {
            psvc = svc.clone();
            pat = *at;
            ptime = Some((*off, *dur));
        }
        None => {}
    }
    if cfg.p.is_some() && !known(&psvc) {
        log(fx, format!("P: no service {psvc}, ignored"));
        (pat, pup) = (-1, -1);
    }
    // SEAM(Clock): T= (tat, tdelta). SEAM(NetFaults): O= (osvc, ofrom, oto, odir).
    let (tat, tdelta) = cfg.t.unwrap_or((-1, 0));
    net.read_o(fx, ctx, cfg.o.as_deref());
    let (mut xsvc, mut xat, mut xoff) = (String::new(), -1i64, 0u64);
    if let Some((s, at, off)) = &cfg.x {
        xsvc = s.clone();
        (xat, xoff) = (*at, *off);
        if !known(s) {
            log(fx, format!("X: no service {s}, ignored"));
            xat = -1;
        }
    }
    let mut pts: Vec<i64> = [0, kat, kup, cat, pat, pup, tat, xat, steps]
        .into_iter()
        .chain(net.points())
        .filter(|p| *p >= 0 && *p <= steps)
        .collect();
    pts.sort();
    pts.dedup();
    for f in [l.acked(), l.outcomes(), format!("{}/workload.log", l.dir)] {
        post(fx, write_cmd(&f, "", false))?;
    }
    start_periodic(fx, ctx)?;
    let (mut down, mut cut, mut paused) = (false, false, false);
    let (mut ptimer, mut xtimer): (Option<usize>, Option<usize>) = (None, None);
    let mut prev: Option<i64> = None;
    for &p in &pts {
        if let Some(a) = prev {
            if a < p {
                step_range(fx, ctx, a, p)?;
            }
        }
        if p == cat {
            cut = true;
            break;
        }
        // SEAM(NetFaults): `p == nto && netup`: net_off, "network healed before step p".
        net.heal_n_at(fx, ctx, p)?;
        if p == kup && down {
            log(fx, format!("restarting {ksvc} before step {p}"));
            if !start_svc(fx, ctx, &ksvc)? {
                return die_oracle(fx, format!("HYDO-ORACLE: ERROR {ksvc} restart failed"));
            }
            down = false;
        }
        if p == xat {
            // the stack's order: the kill timer is armed after the restart, before the kill
            xtimer = Some(fx.start(Part::KillTimer {
                svc: xsvc.clone(),
                off: xoff,
            })?);
            log(
                fx,
                format!("kill timer for {xsvc} armed before step {p} (after {xoff}ms)"),
            );
        }
        if p == kat {
            svc_sig(fx, &ksvc, Sig::Kill)?;
            down = true;
            log(fx, format!("KILLED {ksvc} before step {p}"));
        }
        // SEAM(NetFaults): `p == nfrom`: net_on; `p == oto && oneup`: heal; `p == ofrom`: one-way on.
        net.at(fx, ctx, p)?;
        if p == pup && paused {
            svc_sig(fx, &psvc, Sig::Cont)?;
            paused = false;
            log(fx, format!("RESUMED {psvc} before step {p}"));
        }
        if p == pat {
            if let Some((off, dur)) = ptime {
                ptimer = Some(fx.start(Part::PauseTimer {
                    svc: psvc.clone(),
                    off,
                    dur,
                })?);
                log(
                    fx,
                    format!(
                        "pause timer for {psvc} armed before step {p} (after {off}ms, for {dur}ms)"
                    ),
                );
            } else {
                svc_sig(fx, &psvc, Sig::Stop)?;
                paused = true;
                log(fx, format!("PAUSED {psvc} before step {p}"));
                xassert(
                    fx,
                    "reachable",
                    "fault.paused",
                    "a service was paused (SIGSTOP)",
                );
            }
        }
        // SEAM(Clock): `p == tat`: the clock step, its 5 s check, fault.clock_back/fwd.
        if p == tat {
            clock::clock_jump(fx, p, tdelta)?;
        }
        prev = Some(p);
    }
    if cut {
        if let Some(t) = ptimer {
            fx.cancel(t); // no SIGCONT may follow the freeze
        }
        if let Some(t) = xtimer {
            fx.cancel(t);
        }
        // SEAM(Freeze): stop_periodic's wrapper: the helper must not continue the service
        freeze::kill(fx)?;
        stop_periodic(fx);
        let failed = failed_ids(fx, ctx)?;
        // SEAM(Clock): hydo-init-clock.sh's failed_ids appends clock=<epoch> to the acked file
        let at_cut = clock::at_cut(fx, ctx)?;
        // SEAM(Fakes): the world frozen, dumped (fakes wraps the clock's failed_ids, so its
        // records come after clock= in the shell's acked file)
        let world = fakes::failed_ids_at_cut(fx, ctx)?;
        // SEAM(ErrorLogs): the error logs wrap the fakes' failed_ids (services stopped, records)
        errlog::at_cut(fx, ctx)?;
        let acked = read_all(fx, &l.acked())?;
        fx.report(format!(
            "hydo-explore-acked: {}",
            format!("{acked}{}{world}", clock::acked_token(at_cut)).replace('\n', " ")
        ));
        let acked = acked + &world;
        fx.report(format!(
            "hydo-explore-failed: {}",
            failed.replace('\n', " ")
        ));
        // SEAM(RecoveryInputs): what the recovery conversation is given
        let ending = fakes::power_cut(ctx, &acked, &failed);
        let Ending::PowerCut {
            acked: a,
            failed: f,
            world: w,
        } = &ending
        else {
            unreachable!("fakes::power_cut gives a power cut")
        };
        let inputs = RecoveryInputs {
            acked: a.clone(),
            failed: f.clone(),
            clock: at_cut.flatten(),
            world: w.clone(),
        };
        fx.effect(|h, _| {
            h.cut = Some(inputs);
            Memo::Unit
        });
        // NO sync: it would flush the volatile write cache and close the window under test.
        for s in &plan.services {
            svc_sig(fx, &s.name, Sig::Stop)?;
        }
        log(
            fx,
            format!("POWER CUT before step {cat} (all services frozen)"),
        );
        fx.report("hydo-explore-workload-done cut");
        fx.settle()?;
        return fx.end(ending);
    }
    // no cut: faults are transient -- heal, bring everything back, let the workload retry
    // SEAM(NetFaults): `netup`/`oneup`: net_off, "healed at the end".
    net.heal_at_end(fx, ctx)?;
    if let Some(t) = ptimer {
        fx.join(t)?;
        svc_sig(fx, &psvc, Sig::Cont)?;
    }
    if paused {
        svc_sig(fx, &psvc, Sig::Cont)?;
        log(fx, format!("RESUMED {psvc} at the end"));
    }
    if let Some(t) = xtimer {
        fx.join(t)?;
    }
    restart_dead(fx, ctx, "HYDO-ORACLE: ERROR restart after the faults")?;
    retry_workload(fx, ctx)?;
    freeze::kill(fx)?;
    stop_periodic(fx);
    let failed = failed_ids(fx, ctx)?;
    let acked = read_all(fx, &l.acked())?;
    fx.report(format!("hydo-explore-acked: {}", acked.replace('\n', " ")));
    fx.report(format!(
        "hydo-explore-failed: {}",
        failed.replace('\n', " ")
    ));
    fx.report("hydo-explore-workload-done");
    // explore_read: "go" -- the host switches the disk faults off
    fx.engine(EngineAction::DiskFaultsOff);
    log(fx, "command: go");
    restart_dead(
        fx,
        ctx,
        "HYDO-ORACLE: VIOLATION service cannot recover after the faults",
    )?;
    finish(fx, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_error_is_busybox_text() {
        // the shell image's verdict on the Redis example's lying-disk seed 4 (M127)
        assert_eq!(
            busybox_mount_error("/dev/vda2", "/data", 117).as_deref(),
            Some("mount: mounting /dev/vda2 on /data failed: Data consistency error")
        );
        assert_eq!(
            busybox_mount_error("/dev/vda2", "/data", 5).as_deref(),
            Some("mount: mounting /dev/vda2 on /data failed: I/O error")
        );
        assert_eq!(busybox_mount_error("/dev/vda2", "/data", 9999), None);
    }

    #[test]
    fn shell_text_helpers() {
        assert_eq!(utc(1767225600), "2026-01-01T00:00:00Z");
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(951782400), "2000-02-29T00:00:00Z");
        assert_eq!(secs(250, 6), "0.250000");
        assert_eq!(secs(1500, 3), "1.500");
        assert_eq!(secs(12, 3), "0.012");
        assert_eq!(jesc("a\"b\\c\nd\te"), "a\\\"b\\\\cde");
        assert_eq!(inv_detail("x\nHYDO-ORACLE: OK 3 keys\n"), "3 keys");
        assert_eq!(
            inv_detail("HYDO-ORACLE: VIOLATION  lost 2\nmore\n"),
            "lost 2"
        );
        assert_eq!(inv_detail("HYDO-ORACLE: OK\n"), "HYDO-ORACLE: OK");
        assert_eq!(inv_detail("a\nHYDO-ASSERT {}\n  \n"), "a");
        assert_eq!(inv_detail(""), "");
        assert_eq!(
            failed_of("3 failed\n1 ok\n10 failed\n2 failed\n2 ok\n"),
            "3\n10\n"
        );
        assert_eq!(failed_of(""), "");
        assert_eq!(
            mount_opts("rw,data=journal").unwrap(),
            (vec![], Some("data=journal".into()))
        );
        assert_eq!(
            mount_opts("ro,noatime").unwrap(),
            (vec![MountFlag::Ro, MountFlag::Noatime], None)
        );
        assert!(mount_opts("rw,dirsync").is_err());
        let t = "1\n2\n".repeat(2000);
        let c = chunks(&t);
        assert!(c.iter().all(|x| x.len() <= CHUNK && x.ends_with('\n')));
        assert_eq!(c.concat(), t);
    }
}
