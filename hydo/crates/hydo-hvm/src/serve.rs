//! hv/hvm/explore/serve.c (`hvmx`) in Rust: the explorer's run engine on the Mac. It boots an
//! agent image (every image `hydo build` makes since M136) under lock-step control from its first
//! instruction, the host taking the snapshots S0 and S1 where it decides; serves `CTL`, `RESUME`
//! and `QUIT`; runs `one` schedule from a fresh boot under the same control; and speaks hvx's
//! lock-step protocol (M119, M121, M126, M130); the protocol's lines are in hydo-engine-proto's
//! crate documentation. Every function here keeps its C name, and every output line its C text.
//! The C engine was deleted in Milestone 147 (`git show 145c456:hv/hvm/explore/serve.c`).
//!
//! The shell-init images' paths (a plain boot to the init's S0/S1 markers, `RUN` with the guest
//! part typed on the console and the init's markers, `--control one` from such an image's S1, and
//! the engine's own SPIN re-run of a RUN) were deleted in Milestone 145.
//!
//! # What is particular to the Mac engine (from serve.c's header)
//!
//! - **The clock is the exit count.** `at=` in every EVENT, SENT, STOP and SEND is `M.exits`,
//!   where hvx has the instruction clock. A queued send is typed at the first stop whose exit
//!   count is at least its `at`, before the guest runs again (exact to the exit). Kicks and the
//!   spin observer's own exits are not exits, so every stop is a pure function of the run.
//! - **The cadence** is the schedule's `rb=<n>` (exits from the phase's start), default 65,536.
//! - **Stops** (`why`): `start` before a phase runs, `resume` after a RESUME restore, `send` at
//!   the exit where a queued send is due, `cadence`, and `exit` when the guest stops (halt, idle,
//!   exit budget, SPIN) before the run's error RESULT.
//! - **SNAP** is refused while a typed send is not acknowledged unless `retype` is given: input
//!   still in the UART's receive queue is in no snapshot (M116), so RESUME types every such send
//!   again right after the restore; queued sends are part of the snapshot. SNAP is refused for
//!   schedules with tick jitter (`tj=`) or forced preemption points (`pct=`). At most 8 snapshots;
//!   a second SNAP with a name replaces the first.
//! - **The controlled boot** (M121): an agent image has hydo-agent as `/init` and no S0/S1
//!   markers; the host takes `SNAP S0` and `SNAP S1` in the boot phase, END prints
//!   `RESULT boot ...` and then READY. A CTL restores the host's S1, arms the host part, types
//!   nothing (guest part `run`) and its first stop is `why=resume` with S1's stop number. With
//!   `--control boot one` the host's `SNAP S1` arms the host part and the run goes on in
//!   `phase=workload` from that stop. The kernel command line gets ` quiet loglevel=1`.
//! - **DISKOFF and CUT** (M126): DISKOFF ends the workload (at the host's CONT the workload
//!   fingerprint `wfp` is taken, the disk faults and timing tokens go off, and the run goes on in
//!   `phase=check`); CUT is a power cut (the workload fingerprint is taken, only the flushed disk
//!   bytes survive, and the run goes on in a VM restored from the host's S0 in
//!   `phase=recovery`, queued input DROPped). END in the check or recovery phase fingerprints
//!   there (`fp = fnv(wfp, that)`).
//! - **The data disk** (`HVM_DISK=<NAME>-arm64-disk.img`, the virtio-blk device): `disk=none`,
//!   `ioerr:P`, `cut` and `lying:P` with `dseed=` arm hvx's seeded fault classes (the same seed
//!   gives the same per-request decisions). The disk boots with the flush-aware class armed at
//!   probability 0, so the guest negotiates FLUSH (write-back caching) as hvx's guests do.
//! - **Timing tokens counted in exits:** forced preemption points (`pct=<hex seed> pctn=<points>
//!   [pctk=<exits>, default 20000]`: that many seeded exits among the first `pctk` of the workload
//!   make the timer due at once; the RESULT gains `pctf=` and `pctw=`) and tick jitter (`tj=`,
//!   `tjx=`: timer interrupts delayed by a seeded number of exits; the RESULT gains `tjk=` and
//!   `tjn=`). Host tokens only hvx applies (`pp=`, `xt*`, `q*`, `cf=`, `cfn`) are refused.
//! - **Exit-free spin loops** ([`crate::spin`], hv/hvm/spin/RESEARCH.md): exact-repeat loops are
//!   resolved in every run; counter loops need the rewind mode (a copy-on-write checkpoint at
//!   every exit, several times slower), which the host chooses: a schedule with `spin=rewind` (or
//!   `HVM_SPIN_REWIND=1`) runs its workload, check and recovery phases in it, and a SNAP keeps the
//!   mode for its RESUME. The rewind mode keeps the stops pure (the checkpoint is taken at the
//!   loop top of every exit after the stop hook, and the guest makes no exit between it and the
//!   rewind). A SPIN under control is not re-run here: the host runs the schedule again with
//!   `spin=rewind` (hydo-control, M130). A schedule that completes plainly gives the same
//!   fingerprint with the rewind mode on.
//! - **Guest RAM:** `--ram-mib N` before the mode, else `HVM_MEM_MB`, else 1024 MiB; 256..8192.
//!   Snapshots keep RAM in an unlinked file in `HVM_SNAP_DIR` (default `/tmp/hvm-snap`).
//!
//! Since M142's typed calls the engine core takes typed values: requests and commands at a stop
//! come through a [`Port`] (`hydo_hvx::port`: hydo-engine's [`ChanPort`](hydo_hvx::port::ChanPort)
//! in process, or [`TextPort`](hydo_hvx::text::TextPort) reading hvmx's request lines in the
//! program), reports go out as [`EngineMsg`] values, and every schedule is a typed
//! [`Schedule`] (hydo-schedule; the C engine's own host-part parser is not ported). [`run`] is the
//! typed entry, [`main_with`] the program's `main` (its argv, and the text at the boundary).

use crate::fnv::{fnv, fnv_u64, mix64, OFFSET};
use crate::layout::*;
use crate::machine::{Mach, Pct, Tjit};
use crate::mem::Mem;
use crate::virtio::{self, F_DROP, F_ERROR, F_UNFLUSHED};
use crate::vmm::{now_s, VSnap, VmOpts, Vmm, NSR, SR};
use crate::{catch_fatal, fatal, say, Diag};
use hydo_engine_proto::{
    Answer, DiskOff, EngineMsg, Event, HostCmd, Phase, Ready, Request, RunResult, SendAnswer, Sent,
    Stop, StopWhy,
};
use hydo_hvx::cstr;
use hydo_hvx::port::{Cmd, Port, Req};
use hydo_schedule::{Disk, HostToken, Schedule};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

const CTL_MAX_SENDS: usize = 1024;
const CTL_MAX_SNAPS: usize = 8;
/// the UART receive queue
const CTL_SEND_MAX: usize = 4096;
/// the cadence without rb=: hvx's run calls return every 2^16 retired exits
const CTL_RB_DEFAULT: u64 = 65536;
const RAM_MIN_MIB: u64 = 256;
/// 8 GiB since Milestone 179 (4096 before): RAM from 1 GiB to 9 GiB, far below the 36-bit IPA
/// space `hv_vm_create(NULL)` gives (64 GiB).
const RAM_MAX_MIB: u64 = 8192;

/// Where the engine's lines go and where the host's come from.
pub struct Io {
    /// stdout: the protocol (each line flushed)
    pub out: Box<dyn Write + Send>,
    /// stdin: requests, and the host's lines in a controlled run
    pub input: Box<dyn BufRead + Send>,
    /// stderr: diagnostics
    pub diag: Box<dyn Write + Send>,
    /// where HVM_ECHO copies the console (the program: stdout)
    pub echo: Option<Box<dyn Write + Send>>,
}

/// What [`run`] boots and how (hvmx's command line as values): an agent image under lock-step
/// control from its first instruction (`--control boot`, M121).
pub struct Start {
    pub mode: Mode,
    pub kernel: std::path::PathBuf,
    pub initrd: std::path::PathBuf,
    pub outdir: std::path::PathBuf,
    /// `--ram-mib` (None: `HVM_MEM_MB`, else 1024).
    pub ram_mib: Option<u64>,
}

pub enum Mode {
    /// One schedule from a fresh boot (the host's `SNAP S1` in the boot starts its workload),
    /// then exit.
    One(Schedule),
    /// Boot under the host's control with this boot schedule's `rb=` as the cadence (None: an
    /// empty one), then serve requests.
    Serve { boot: Option<Schedule> },
}

/// The engine's options (hvm.c's and serve.c's environment variables, M144's typed group).
pub use hydo_config::hvm::Options as HvmOptions;

/// A run from the workload's start on: what run_rest needs, and what a SNAP keeps (`struct runst`).
#[derive(Clone, Default, Debug)]
struct RunSt {
    tag: String,
    t0: f64,
    from: usize,
    cut: bool,
    rewound: bool,
    pct_n: u32,
    wx0: u64,
    tjinfo: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cs {
    Pending,
    Typed,
    Acked,
    Retype,
}

#[derive(Clone, Debug)]
struct CtlSend {
    id: u64,
    at: u64,
    sent: u64,
    state: Cs,
    buf: Vec<u8>,
}

/// What a snapshot carries besides the VM (`struct ctl_book`).
#[derive(Clone, Default, Debug)]
struct Book {
    nstop: u64,
    nev: u64,
    next_id: u64,
    ntyped: u64,
    inh: u64,
    /// the exit count the cadence counts from
    cad0: u64,
    /// console offset up to which lines were reported as EVENTs
    evfrom: usize,
    s: Vec<CtlSend>,
}

/// A snapshot (`struct xsnap`): RAM in an unlinked file, mapped.
#[derive(Default)]
struct XSnap {
    v: VSnap,
    m: Mach,
    ram: Option<Mem>,
    vio: Option<virtio::Save>,
    con: Vec<u8>,
}

#[derive(Default)]
struct CtlSnap {
    name: String,
    x: XSnap,
    rs: RunSt,
    k: Book,
    rb: u64,
    used: bool,
    rw: bool,
}

/// The lock-step state (`C`).
struct Ctl {
    on: bool,
    /// 1 = the phase's start stop is due, 2 = a RESUME's
    start: i32,
    ended: i32,
    /// --control boot one: SNAP S1 taken in the boot phase (M121)
    boot_s1: bool,
    /// M126: DISKOFF/CUT given (1), then acted on (2)
    wdone: i32,
    cut: bool,
    tag: String,
    phase: Phase,
    at: u64,
    rb: u64,
    last_x: u64,
    k: Book,
    endv: String,
    rs: RunSt,
}

impl Default for Ctl {
    fn default() -> Ctl {
        Ctl {
            on: false,
            start: 0,
            ended: 0,
            boot_s1: false,
            wdone: 0,
            cut: false,
            tag: String::new(),
            phase: Phase::Workload,
            at: 0,
            rb: 0,
            last_x: 0,
            k: Book::default(),
            endv: String::new(),
            rs: RunSt::default(),
        }
    }
}

/// Settings read once from the environment.
#[derive(Clone, Debug)]
pub struct Opts {
    pub vm: VmOpts,
    pub ram_mib: u64,
    pub run_exits: u64,
    pub snap_dir: String,
    pub diag: bool,
    pub diag_chunks: Option<String>,
    pub check_restore: bool,
    pub no_mlock: bool,
}

/// Everything but the VM (so the run loop's stop hook can borrow it while the VM runs).
struct Host {
    port: Box<dyn Port>,
    diag: Diag,
    outdir: String,
    o: Opts,
    c: Ctl,
    snaps: Vec<CtlSnap>,
    g_ctl_one: bool,
    /// the host's S0 snapshot slot (the recovery VM's start after a CUT)
    g_s0: Option<usize>,
    spin_hit: bool,
}

/// The engine: one VM and the explorer's state around it.
pub struct Engine {
    vm: Vmm,
    h: Host,
}

/// C's `snprintf(buf, cap, ...)`: at most `cap - 1` bytes.
fn ccap(mut b: Vec<u8>, cap: usize) -> Vec<u8> {
    b.truncate(cap - 1);
    b
}
fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

impl Host {
    fn emit(&mut self, m: EngineMsg) {
        self.port.emit(m);
    }
    fn answer(&mut self, a: Answer) {
        self.port.emit(EngineMsg::Answer(a));
    }
    fn refuse(&mut self, why: String) {
        self.answer(Answer::Refused(why));
    }
    fn say(&self, s: &str) {
        say(&self.diag, s);
    }
    fn result_err(&mut self, tag: &str, v: &str) {
        if let Some(r) = RunResult::new(tag, "0", "0", "0", "", v) {
            self.emit(EngineMsg::Result(Box::new(r)));
        }
    }

    fn snap_find(&self, name: &str) -> Option<usize> {
        self.snaps.iter().position(|s| s.used && s.name == name)
    }

    // ---------------- lock-step control ----------------
    fn ctl_begin(&mut self, tag: &str) {
        self.c = Ctl {
            on: true,
            start: 1,
            tag: lossy(&ccap(tag.as_bytes().to_vec(), 64)),
            phase: Phase::Workload,
            rb: CTL_RB_DEFAULT,
            ..Ctl::default()
        };
    }
    fn ctl_finish(&mut self) {
        self.c.k = Book::default();
        self.c.on = false;
    }
    fn ctl_verdict(&self, vm: &Vmm, r: i32, from: usize) -> String {
        if r == -3 && !self.c.endv.is_empty() {
            self.c.endv.clone()
        } else {
            last_verdict(vm, from)
        }
    }
    fn ctl_type(&mut self, vm: &mut Vmm, i: usize, print: bool) {
        let retype = self.c.k.s[i].state == Cs::Retype;
        let room = vm.rx_room();
        let len = self.c.k.s[i].buf.len() as u32;
        let id = self.c.k.s[i].id;
        if len > room {
            self.say(&format!(
                "hvmx: {}: send {id}: {} of {len} bytes do not fit in the receive queue",
                self.c.tag,
                len - room
            ));
        }
        for j in 0..(len.min(room) as usize) {
            vm.rx_push(self.c.k.s[i].buf[j]);
        }
        self.c.k.s[i].state = Cs::Typed;
        if !retype {
            let at = self.c.at;
            self.c.k.s[i].sent = at;
            self.c.k.ntyped += 1;
            let base = if self.c.k.inh != 0 {
                self.c.k.inh
            } else {
                OFFSET
            };
            let mut h = fnv_u64(at, base);
            h = fnv(&len.to_le_bytes(), h);
            h = fnv(&self.c.k.s[i].buf, h);
            self.c.k.inh = h;
        }
        if print {
            let m = EngineMsg::Sent {
                tag: self.c.tag.clone(),
                sent: Sent {
                    id,
                    at: self.c.at,
                    retype,
                },
            };
            self.emit(m);
        }
    }
    /// every send due at this stop: retypes first (id order), then (at, id) order
    fn ctl_deliver(&mut self, vm: &mut Vmm) {
        for i in 0..self.c.k.s.len() {
            if self.c.k.s[i].state == Cs::Retype {
                self.ctl_type(vm, i, true);
            }
        }
        loop {
            let mut best: Option<usize> = None;
            for i in 0..self.c.k.s.len() {
                let s = &self.c.k.s[i];
                if s.state != Cs::Pending || s.at > self.c.at {
                    continue;
                }
                match best {
                    None => best = Some(i),
                    Some(b) => {
                        let bs = &self.c.k.s[b];
                        if s.at < bs.at || (s.at == bs.at && s.id < bs.id) {
                            best = Some(i);
                        }
                    }
                }
            }
            let Some(b) = best else { return };
            self.ctl_type(vm, b, true);
        }
    }
    fn ctl_due(&self) -> bool {
        self.c
            .k
            .s
            .iter()
            .any(|s| s.state == Cs::Retype || (s.state == Cs::Pending && s.at <= self.c.at))
    }
    fn ctl_drop_pending(&mut self) {
        for i in 0..self.c.k.s.len() {
            if matches!(self.c.k.s[i].state, Cs::Pending | Cs::Retype) {
                self.c.k.s[i].state = Cs::Acked;
                let m = EngineMsg::Drop {
                    tag: self.c.tag.clone(),
                    id: self.c.k.s[i].id,
                };
                self.emit(m);
            }
        }
    }
    fn ctl_snap_cmd(&mut self, vm: &mut Vmm, name: &str, retype: bool) {
        if self.c.phase != Phase::Workload && self.c.phase != Phase::Boot {
            self.refuse(format!("snap {name} only in the workload or boot phase"));
            return;
        }
        if vm.tj.on || self.c.rs.pct_n != 0 {
            self.refuse(format!("snap {name} not for tj/pct schedules"));
            return;
        }
        if self.c.wdone != 0 {
            self.refuse(format!("snap {name} after DISKOFF/CUT"));
            return;
        }
        let typed: Vec<u64> = self
            .c
            .k
            .s
            .iter()
            .filter(|s| s.state == Cs::Typed)
            .map(|s| s.id)
            .collect();
        if !typed.is_empty() && !retype {
            let ids = Answer::Snap {
                name: String::new(),
                at: 0,
                retype: typed.clone(),
            }
            .line();
            let ids = ids.rsplit("retype=").next().unwrap_or("").to_string();
            self.refuse(format!("snap {name} unacked={ids}"));
            return;
        }
        let slot = self
            .snap_find(name)
            .or_else(|| self.snaps.iter().position(|s| !s.used));
        let Some(slot) = slot else {
            self.refuse(format!("snap {name} no free slot ({CTL_MAX_SNAPS} names)"));
            return;
        };
        {
            let cs = &mut self.snaps[slot];
            if cs.used {
                // replaced: keep the RAM mapping, drop the rest
                cs.x.con.clear();
                cs.x.vio = None;
                cs.k = Book::default();
            }
        }
        let snap_dir = self.o.snap_dir.clone();
        xsnap_take(vm, &mut self.snaps[slot].x, "ctl", &snap_dir);
        let mut k = self.c.k.clone();
        for s in &mut k.s {
            if s.state == Cs::Typed {
                s.state = Cs::Retype;
            }
        }
        let cs = &mut self.snaps[slot];
        cs.k = k;
        cs.rs = self.c.rs.clone();
        cs.rb = self.c.rb;
        cs.used = true;
        cs.rw = vm.sp.rw_on;
        cs.name = lossy(&ccap(name.as_bytes().to_vec(), 64));
        let at = self.c.at;
        self.answer(Answer::Snap {
            name: name.to_string(),
            at,
            retype: typed,
        });
        if self.g_ctl_one && self.c.phase == Phase::Boot && name == "S1" {
            self.c.boot_s1 = true; // M121: the workload starts here once the host says CONT
        }
    }

    /// one stop: report, then serve host commands until CONT (false) or END/EOF (true)
    fn ctl_stop(&mut self, vm: &mut Vmm, why: StopWhy) -> bool {
        let mut p = self.c.k.evfrom;
        let len = vm.m.console_len as usize;
        let mut q = p;
        while q < len {
            if vm.console[q] == b'\n' {
                let mut e = q;
                if e > p && vm.console[e - 1] == b'\r' {
                    e -= 1;
                }
                self.c.k.nev += 1;
                let m = EngineMsg::Event {
                    tag: self.c.tag.clone(),
                    event: Event {
                        at: self.c.at,
                        n: self.c.k.nev,
                        text: vm.console[p..e].to_vec(),
                    },
                };
                self.emit(m);
                p = q + 1;
            }
            q += 1;
        }
        self.c.k.evfrom = p;
        self.ctl_deliver(vm);
        self.c.k.nstop += 1;
        let m = EngineMsg::Stop {
            tag: self.c.tag.clone(),
            stop: Stop {
                at: self.c.at,
                n: self.c.k.nstop,
                phase: self.c.phase,
                why,
            },
        };
        self.emit(m);
        loop {
            let cmd = match self.port.command() {
                Cmd::Closed => {
                    self.c.endv = "ERROR control channel closed".into();
                    return true;
                }
                Cmd::Bad(r) => {
                    self.refuse(r);
                    continue;
                }
                Cmd::Host(c) => c,
            };
            match cmd {
                HostCmd::Cont => return false,
                HostCmd::End(v) => {
                    self.c.endv = lossy(&ccap(v.unwrap_or_default().into_bytes(), 256));
                    return true;
                }
                HostCmd::Send { at, bytes } => {
                    let len = bytes.len();
                    if len == 0 || len > CTL_SEND_MAX {
                        self.refuse(format!("send length {len} not in [1, {CTL_SEND_MAX}]"));
                        continue;
                    }
                    if self.c.k.s.len() >= CTL_MAX_SENDS {
                        self.refuse(format!("send more than {CTL_MAX_SENDS} sends in one run"));
                        continue;
                    }
                    self.c.k.next_id += 1;
                    let id = self.c.k.next_id;
                    self.c.k.s.push(CtlSend {
                        id,
                        at,
                        sent: 0,
                        state: Cs::Pending,
                        buf: bytes,
                    });
                    if at <= self.c.at {
                        let i = self.c.k.s.len() - 1;
                        self.ctl_type(vm, i, false);
                        let at = self.c.at;
                        self.answer(Answer::Send(SendAnswer::Typed { id, at }));
                    } else {
                        self.answer(Answer::Send(SendAnswer::Queued { id, at }));
                    }
                }
                HostCmd::Ack(id) => {
                    let a = match self.c.k.s.iter_mut().find(|s| s.id == id) {
                        None => Answer::Refused(format!("ack {id} unknown")),
                        Some(s) if s.state == Cs::Pending => {
                            Answer::Refused(format!("ack {id} not typed yet"))
                        }
                        Some(s) => {
                            s.state = Cs::Acked;
                            Answer::Ack(id)
                        }
                    };
                    self.answer(a);
                }
                HostCmd::Snap { name, retype } => self.ctl_snap_cmd(vm, &name, retype),
                HostCmd::DiskOff | HostCmd::Cut => {
                    let cut = matches!(cmd, HostCmd::Cut);
                    let no = if self.c.phase != Phase::Workload {
                        Some("only in the workload phase")
                    } else if self.c.wdone != 0 {
                        Some("after DISKOFF/CUT")
                    } else if cut && self.snap_find("S0").is_none() {
                        Some("without a snapshot S0")
                    } else {
                        None
                    };
                    if let Some(no) = no {
                        self.refuse(format!("{} {no}", if cut { "cut" } else { "diskoff" }));
                        continue;
                    }
                    self.c.wdone = 1;
                    self.c.cut = cut;
                    let at = self.c.at;
                    if cut {
                        self.answer(Answer::Cut { at });
                        return false;
                    }
                    self.answer(Answer::DiskOff(DiskOff { at, cut: false }));
                }
            }
        }
    }

    /// hvm_stop_hook at every loop top of a lock-step phase
    fn ctl_hook(&mut self, vm: &mut Vmm) -> bool {
        let x = vm.m.exits;
        if x == self.c.last_x {
            return false; // a kick or an observer exit: not a new exit
        }
        self.c.last_x = x;
        self.c.at = x;
        let why = if self.ctl_due() {
            Some(StopWhy::Send)
        } else if x.wrapping_sub(self.c.k.cad0).is_multiple_of(self.c.rb) {
            Some(StopWhy::Cadence)
        } else {
            None
        };
        if let Some(w) = why {
            if self.ctl_stop(vm, w) {
                self.c.ended = 1;
                return true;
            }
        }
        if self.c.boot_s1 {
            self.c.ended = 2;
            return true;
        }
        if self.c.wdone == 1 {
            self.c.wdone = 2;
            self.c.ended = 3;
            return true;
        }
        false
    }
}

fn last_verdict(vm: &Vmm, from: usize) -> String {
    let len = vm.m.console_len as usize;
    let c = &vm.console[..len];
    let mut at: Option<usize> = None;
    let mut i = from;
    while i + 12 <= len {
        if &c[i..i + 12] == b"HYDO-ORACLE:" {
            at = Some(i);
        }
        i += 1;
    }
    let Some(at) = at else {
        return "ERROR no verdict".into();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while at + i < len && c[at + i] != b'\n' && c[at + i] != b'\r' && i + 1 < 300 {
        out.push(c[at + i]);
        i += 1;
    }
    lossy(&out)
}

fn stop_name(s: i32) -> &'static str {
    match s {
        STOP_HALT => "guest halted",
        STOP_IDLE => "guest idle with no timer",
        STOP_LIMIT => "exit budget exhausted",
        STOP_SPIN => "SPIN (guest loops without exits; run invalid)",
        _ => "?",
    }
}

fn tj_wanted(t: &Tjit) -> bool {
    t.n_explicit != 0 || (t.permille != 0 && t.max_delay != 0)
}

/// The host tokens hvx alone applies (the PMU clock, vCPU turns, the k-th-flush cut; `rb=` outside
/// a lock-step run): the first one's refusal, as the C engine words it.
fn host_token_problem(s: &Schedule) -> Option<String> {
    for t in &s.host {
        let hvx_only = matches!(
            t,
            HostToken::Rb(_)
                | HostToken::Pp(_)
                | HostToken::Pptsc(_)
                | HostToken::Xt(_)
                | HostToken::Xtr(_)
                | HostToken::Xtpm(_)
                | HostToken::Xtlo(_)
                | HostToken::Xthi(_)
                | HostToken::Xtsc(_)
                | HostToken::Xtx(_)
                | HostToken::Cf(_)
                | HostToken::Cfn
                | HostToken::Q(_)
                | HostToken::Qs(_)
                | HostToken::Qb { .. }
                | HostToken::Qbs(_)
                | HostToken::Qm(_)
        );
        if hvx_only {
            let w = t.to_string();
            let w = &w[..w.len().min(60)];
            let msg = format!("host token {w} needs hvx (the PMU clock, vCPU turns or a power cut at a disk FLUSH)");
            return Some(lossy(&ccap(msg.into_bytes(), 200)));
        }
    }
    None
}

/// The disk policy of the host part (serve.c parse_disk): (seed, classes, probability).
fn disk_policy(s: &Schedule) -> (u64, u32, u32) {
    let (c, p) = match s.disk() {
        Disk::None => (0, 0),
        Disk::Cut => (F_UNFLUSHED, 100),
        Disk::IoErr(p) => (F_ERROR, p as u32),
        Disk::Lying(p) => (F_DROP, p as u32),
    };
    (s.dseed(), c, p)
}

fn num(s: &Schedule, key: &str) -> Option<u64> {
    match s.host_token(key)? {
        HostToken::Tj(v)
        | HostToken::Tjpm(v)
        | HostToken::Tjmax(v)
        | HostToken::Tjlo(v)
        | HostToken::Tjhi(v)
        | HostToken::Pct(v)
        | HostToken::Pctn(v)
        | HostToken::Pctk(v)
        | HostToken::Rb(v) => Some(*v),
        _ => None,
    }
}

/// The tick jitter of the host part (serve.c parse_tj; left off: run_once turns it on). Its
/// delays are counted in exits (`tjmax=` default 1000).
fn tick_jitter(s: &Schedule) -> Tjit {
    let mut t = Tjit::default();
    if let Some(HostToken::Tjx(l)) = s.host_token("tjx") {
        for (tick, delay) in l.iter().take(TJ_MAX_EXPLICIT) {
            t.ek[t.n_explicit] = *tick;
            t.ed[t.n_explicit] = *delay as u32;
            t.n_explicit += 1;
        }
        return t;
    }
    let Some(seed) = num(s, "tj") else {
        return t;
    };
    t.seed = seed;
    t.permille = num(s, "tjpm").unwrap_or(10) as u32;
    t.max_delay = num(s, "tjmax").unwrap_or(1000) as u32;
    t.lo = num(s, "tjlo").unwrap_or(0);
    t.hi = num(s, "tjhi").unwrap_or(0);
    if t.permille == 0 || t.max_delay == 0 {
        t = Tjit::default();
    }
    t
}

/// `dst = src`, writing only the 16 KiB pages that differ (Milestone 179): the same bytes as a
/// plain copy, but a page the guest never touched stays a hole in the snapshot's file and is not
/// faulted into guest RAM, so an 8 GiB guest's snapshots take the disk and memory its guest used,
/// not 8 GiB each.
fn copy_changed(dst: &mut [u8], src: &[u8]) {
    assert_eq!(dst.len(), src.len());
    for (d, s) in dst.chunks_mut(PG as usize).zip(src.chunks(PG as usize)) {
        if d != s {
            d.copy_from_slice(s);
        }
    }
}

/// `xsnap_take`
fn xsnap_take(vm: &Vmm, s: &mut XSnap, name: &str, snap_dir: &str) {
    if s.ram.is_none() {
        match Mem::file_backed(snap_dir, name, vm.ram_size as usize) {
            Ok(m) => s.ram = Some(m),
            Err(e) => fatal(1, &format!("{e}")),
        }
    }
    s.v = vm.vtake();
    s.m = vm.m;
    copy_changed(s.ram.as_mut().unwrap().bytes_mut(), vm.ram.bytes());
    if vm.vio.attached() {
        s.vio = Some(vm.vio.save());
    }
    s.con = vm.console[..vm.m.console_len as usize].to_vec();
}

/// `xsnap_restore`: a fresh VM, not just a fresh vCPU.
fn xsnap_restore(vm: &mut Vmm, s: &XSnap, check: bool) {
    vm.fresh_vm();
    vm.spin_reset();
    copy_changed(
        vm.ram.bytes_mut(),
        s.ram.as_ref().expect("a snapshot").bytes(),
    );
    let (p, n) = (vm.ram.ptr(), vm.ram.len());
    crate::hv::icache_invalidate(p, n);
    vm.vput(&s.v);
    vm.m = s.m;
    let n = vm.m.console_len as usize;
    vm.console[..n].copy_from_slice(&s.con[..n]);
    if let Some(v) = &s.vio {
        vm.vio.restore(v);
    }
    vm.rx_head = 0;
    vm.rx_tail = 0;
    if check {
        let r = vm.vtake();
        for i in 0..31 {
            if r.x[i] != s.v.x[i] {
                vm.say(&format!(
                    "hvmx: restore: x{i} {:x} != {:x}",
                    r.x[i], s.v.x[i]
                ));
            }
        }
        if r.pc != s.v.pc || r.cpsr != s.v.cpsr || r.fpcr != s.v.fpcr || r.fpsr != s.v.fpsr {
            vm.say("hvmx: restore: pc/cpsr/fpcr/fpsr differ");
        }
        if r.q != s.v.q {
            vm.say("hvmx: restore: SIMD registers differ");
        }
        for i in 0..NSR {
            if r.ok[i] != s.v.ok[i] || r.sr[i] != s.v.sr[i] {
                vm.say(&format!(
                    "hvmx: restore: sysreg {i} (0x{:x}) ok {}/{} {:x} != {:x}",
                    SR[i], r.ok[i], s.v.ok[i], r.sr[i], s.v.sr[i]
                ));
            }
        }
    }
}

impl Engine {
    /// HVMX_DIAG=1: per-component hashes of the end state into <outdir>/<tag>.diag
    fn diag(&self, tag: &str, phase: &str) {
        if !self.h.o.diag {
            return;
        }
        let vm = &self.vm;
        let path = format!("{}/{tag}.diag", self.h.outdir);
        let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        else {
            return;
        };
        let s = vm.vtake();
        let mut t = String::new();
        use std::fmt::Write as _;
        let cl = vm.m.console_len;
        let _ = writeln!(
            t,
            "{phase} console {:016x} len {cl}",
            fnv(&vm.console[..cl as usize], OFFSET)
        );
        for i in 0..31 {
            let _ = writeln!(t, "{phase} x{i} {:016x}", s.x[i]);
        }
        let _ = writeln!(
            t,
            "{phase} pc {:016x} cpsr {:016x} fpcr {:x} fpsr {:x}",
            s.pc, s.cpsr, s.fpcr, s.fpsr
        );
        for i in 0..32 {
            let _ = writeln!(t, "{phase} q{i} {:016x}", fnv(&s.q[i].0, OFFSET));
        }
        for i in 0..NSR {
            let _ = writeln!(t, "{phase} sr{i} {} {:016x}", s.ok[i], s.sr[i]);
        }
        let _ = writeln!(
            t,
            "{phase} mach {:016x} exits {} vtime {}",
            fnv(&vm.m.without_stats().bytes(), OFFSET),
            vm.m.exits,
            vm.m.vtime
        );
        let ram = vm.ram.bytes();
        let mut o = 0usize;
        while (o as u64) < vm.ram_size {
            let _ = writeln!(
                t,
                "{phase} ram@{o:08x} {:016x}",
                mix64(&ram[o..o + (1 << 20)], OFFSET)
            );
            o += 1 << 20;
        }
        let _ = f.write_all(t.as_bytes());
        if let Some(ch) = &self.h.o.diag_chunks {
            for c in ch.split(',').filter(|c| !c.is_empty()) {
                let o = cstr::strtoull0(c.as_bytes());
                let p = format!("{}/{tag}.{phase}.{o:x}.bin", self.h.outdir);
                let o = o as usize;
                if o + (1 << 20) <= ram.len() {
                    let _ = std::fs::write(p, &ram[o..o + (1 << 20)]);
                }
            }
        }
    }

    fn save_log(&self, tag: &str, from: usize) {
        let path = format!("{}/{tag}.log", self.h.outdir);
        let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        else {
            return;
        };
        let len = self.vm.m.console_len as usize;
        if len > from {
            let _ = f.write_all(&self.vm.console[from..len]);
        }
    }

    /// run one phase under lock-step: the phase's start (or resume) stop, then stops at due sends
    /// and the cadence; -3 when the host ended the run (END), -4 (--control boot one) at the stop
    /// where the host took S1 and said CONT, -7 at the host's DISKOFF/CUT, -1 when the guest
    /// stopped (the reason in `why`)
    fn run_to(&mut self, why: &mut i32) -> i32 {
        if self.h.c.start != 0 {
            let w = if self.h.c.start == 2 {
                StopWhy::Resume
            } else {
                StopWhy::Start
            };
            if self.h.c.start == 2 {
                self.h.c.k.nstop = self.h.c.k.nstop.wrapping_sub(1); // the snapshot's own stop, again
            } else {
                self.h.c.k.cad0 = self.vm.m.exits;
            }
            self.h.c.start = 0;
            self.h.c.at = self.vm.m.exits;
            if self.h.ctl_stop(&mut self.vm, w) {
                return -3;
            }
            if self.h.c.boot_s1 {
                return -4;
            }
            if self.h.c.wdone == 1 {
                self.h.c.wdone = 2;
                return -7;
            }
        }
        self.h.c.last_x = self.vm.m.exits;
        self.h.c.ended = 0;
        self.vm.max_exits = self.vm.m.exits + self.h.o.run_exits;
        let h = &mut self.h;
        let s = self.vm.run_until(None, &mut |vm| h.ctl_hook(vm));
        match self.h.c.ended {
            2 => return -4,
            3 => return -7,
            0 => {}
            _ => return -3,
        }
        self.h.c.at = self.vm.m.exits;
        if self.h.ctl_stop(&mut self.vm, StopWhy::Exit) {
            return -3;
        }
        *why = s;
        -1
    }

    /// The lock-step part of a schedule: its `rb=` taken out (the cadence; 0 = none), and whether
    /// its run is in the rewind mode (`spin=rewind`, or HVM_SPIN_REWIND=1).
    fn ctl_sched(&self, s: &Schedule) -> (Schedule, u64, bool) {
        let rw = self.vm.sp.rw_on || s.host_token("spin").is_some();
        let rb = num(s, "rb").unwrap_or(0);
        let mut t = s.clone();
        t.host.retain(|h| !matches!(h, HostToken::Rb(_)));
        (t, rb, rw)
    }

    /// CTL <tag> <schedule>: restore the host's S1 (snapshot slot `cs`) and continue its book.
    fn ctl_request(&mut self, tag: &str, sched: &Schedule, cs: usize) {
        let base = self.vm.sp.rw_on;
        let (s, rb, rw) = self.ctl_sched(sched);
        let check = self.h.o.check_restore;
        xsnap_restore(&mut self.vm, &self.h.snaps[cs].x, check);
        self.h.ctl_begin(tag);
        self.h.c.k = self.h.snaps[cs].k.clone();
        self.h.c.start = 2; // the first stop is S1's own again
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.h.spin_hit = false;
        self.vm.sp.rw_on = rw; // M130: the run's phases in the rewind mode
        self.run_once(tag, &s, rw);
        self.vm.sp.rw_on = base;
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    /// the boot under control: -3 at the host's END, -4 (--control boot one) at the stop where
    /// the host took S1 and said CONT, -1 when the guest stopped (`why`)
    fn agent_boot(&mut self, tag: &str, rb: u64, why: &mut i32) -> i32 {
        self.h.ctl_begin(tag);
        self.h.c.phase = Phase::Boot;
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.h.c.rs = RunSt {
            tag: tag.to_string(),
            t0: now_s(),
            ..RunSt::default()
        };
        self.run_to(why)
    }
    fn agent_boot_result(&mut self, tag: &str, r: i32, why: i32, t0: f64) {
        let mut wfp = 0;
        let verdict = if r == -3 {
            wfp = self.vm.xfp();
            self.h.ctl_verdict(&self.vm, r, 0)
        } else {
            format!("ERROR boot: {}", stop_name(why))
        };
        self.save_log(tag, 0);
        let k = &self.h.c.k;
        let extras = format!(" stops={} typed={} inh={:016x}", k.nstop, k.ntyped, k.inh);
        self.result(tag, fnv_u64(wfp, wfp), wfp, now_s() - t0, &extras, &verdict);
        self.h.ctl_finish();
    }

    /// A RESULT: `fp`, `wfp`, the run's seconds (one decimal), the extras (` key=value...`).
    fn result(&mut self, tag: &str, fp: u64, wfp: u64, secs: f64, extras: &str, verdict: &str) {
        let r = RunResult::new(
            tag,
            &format!("{fp:016x}"),
            &format!("{wfp:016x}"),
            &format!("{secs:.1}"),
            extras,
            verdict,
        );
        match r {
            Some(r) => self.h.emit(EngineMsg::Result(Box::new(r))),
            None => self.h.say(&format!(
                "hvmx: {tag}: a RESULT that does not read back: {verdict}"
            )),
        }
    }
    /// RESUME <tag> <snapshot>: continue a controlled run from a SNAP in a fresh VM
    fn ctl_resume(&mut self, tag: &str, name: &str) {
        let Some(i) = self.h.snaps.iter().rposition(|s| s.used && s.name == name) else {
            self.h.result_err(tag, &format!("ERROR no snapshot {name}"));
            return;
        };
        let check = self.h.o.check_restore;
        xsnap_restore(&mut self.vm, &self.h.snaps[i].x, check);
        self.h.ctl_begin(tag);
        self.h.c.k = self.h.snaps[i].k.clone();
        self.h.c.start = 2;
        self.h.c.rb = self.h.snaps[i].rb;
        let mut rs = self.h.snaps[i].rs.clone();
        rs.tag = lossy(&ccap(tag.as_bytes().to_vec(), 64));
        rs.t0 = now_s();
        self.h.spin_hit = false;
        let base = self.vm.sp.rw_on;
        self.vm.sp.rw_on = self.h.snaps[i].rw;
        self.run_rest(rs);
        self.vm.sp.rw_on = base;
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    /// (the guest part of an agent image's schedule is empty: the guest's faults are the host
    /// controller's; serve_loop and engine_main refuse any other)
    fn unsupported(&self, s: &Schedule) -> Option<&'static str> {
        let att = self.vm.vio.attached();
        if !att && s.host_token("disk").is_some() && s.disk() != Disk::None {
            return Some("disk faults and power cuts need the data disk (HVM_DISK)");
        }
        None
    }

    /// One schedule: arm the host part and run it (`run_once`); the host types everything.
    fn run_once(&mut self, tag: &str, s: &Schedule, rewound: bool) {
        let t0 = now_s();
        if let Some(hw) = host_token_problem(s) {
            self.h.result_err(tag, &format!("UNSUPPORTED on hvm: {hw}"));
            return;
        }
        if let Some(u) = self.unsupported(s) {
            self.h.result_err(tag, &format!("UNSUPPORTED on hvm: {u}"));
            return;
        }
        let (dseed, dcl, dpr) = disk_policy(s);
        let tj = tick_jitter(s);
        let pct_seed = num(s, "pct").unwrap_or(0);
        let pct_n = if s.host_token("pct").is_some() {
            num(s, "pctn").unwrap_or(0) as u32
        } else {
            0
        };
        let pct_k = num(s, "pctk").unwrap_or(20000);
        let from = self.vm.m.console_len as usize;
        self.vm.trace_line(&format!("RUN {tag}"));
        self.vm.vio.set_fault(dseed, dcl, dpr);
        let mut rs = RunSt {
            tag: lossy(&ccap(tag.as_bytes().to_vec(), 64)),
            t0,
            from,
            rewound,
            pct_n,
            ..RunSt::default()
        };
        if tj_wanted(&tj) {
            // the jitter covers the workload only, like the disk fault
            self.vm.tj = tj;
            self.vm.tj.on = true;
            rs.tjinfo = format!(" tjk={}", self.vm.m.vtime / TJ_PERIOD);
        }
        rs.wx0 = self.vm.m.exits;
        if pct_n != 0 {
            let x = self.vm.m.exits;
            self.vm.pct = Pct::set(pct_seed, pct_n, x, x.wrapping_add(pct_k));
        }
        self.run_rest(rs);
    }

    /// the rest of a run, from the workload's start: a CTL goes on here, a RESUME starts here
    fn run_rest(&mut self, rs: RunSt) {
        let tag = rs.tag.clone();
        let mut tjinfo = rs.tjinfo.clone();
        let t0 = rs.t0;
        let (mut wfp, mut ffp) = (0u64, 0u64);
        let wx0 = rs.wx0;
        let pct_n = rs.pct_n;
        let mut cut = rs.cut;
        let rewound = rs.rewound;
        let mut why = 0;
        let mut from = rs.from;
        self.h.c.rs = rs; // lock-step: what a SNAP in this run carries
        let mut r = self.run_to(&mut why);
        if r == -7 {
            r = 0;
            cut |= self.h.c.cut; // M126: the host's DISKOFF/CUT is its workload marker
        }
        let cap = |s: &mut String| {
            if s.len() > 159 {
                s.truncate(159)
            }
        };
        if pct_n != 0 {
            tjinfo.push_str(&format!(
                " pctf={} pctw={}",
                self.vm.pct.fired,
                self.vm.m.exits.wrapping_sub(wx0)
            ));
            cap(&mut tjinfo);
            self.vm.pct = Pct::default();
        }
        if self.vm.tj.on {
            tjinfo.push_str(&format!(
                "-{} tjn={}",
                self.vm.m.vtime / TJ_PERIOD,
                self.vm.tj.applied
            ));
            cap(&mut tjinfo);
            self.vm.tj = Tjit::default();
        }
        let mut log_tag = tag.clone();
        let verdict: String;
        'out: {
            if r < 0 && r != -3 {
                self.h.spin_hit |= why == STOP_SPIN;
                verdict = format!("ERROR workload: {}", stop_name(why));
                break 'out;
            }
            wfp = self.vm.xfp();
            self.diag(&tag, "w");
            if r == -3 {
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                ffp = wfp;
                break 'out;
            }
            if !cut {
                // same boot: faults off, heal, check
                self.vm.vio.set_fault(0, 0, 0);
                if self.h.c.on {
                    self.h.c.phase = Phase::Check;
                    self.h.c.start = 1;
                }
                r = self.run_to(&mut why);
                if r < 0 && r != -3 {
                    self.h.spin_hit |= why == STOP_SPIN;
                    verdict = format!("ERROR check: {}", stop_name(why));
                    break 'out;
                }
                ffp = self.vm.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                break 'out;
            }
            // power cut: only the flushed disk bytes survive; recover from the host's S0
            let durable = self.vm.vio.disk.clone();
            self.save_log(&tag, from);
            let check = self.h.o.check_restore;
            let Some(s0) = self.h.g_s0 else {
                fatal(3, "hvmx: a power cut without the host's S0 snapshot");
            };
            xsnap_restore(&mut self.vm, &self.h.snaps[s0].x, check);
            if let (Some(d), Some(du)) = (&mut self.vm.vio.disk, durable) {
                *d = du;
            }
            self.vm.vio.set_fault(0, 0, 0);
            from = self.vm.m.console_len as usize;
            if self.h.c.on {
                // a new VM with its own clock: queued input does not carry over
                self.h.ctl_drop_pending();
                self.h.c.phase = Phase::Recovery;
                self.h.c.start = 1;
                self.h.c.k.evfrom = from;
            }
            r = self.run_to(&mut why);
            if r < 0 && r != -3 {
                self.h.spin_hit |= why == STOP_SPIN;
                verdict = format!("ERROR recovery: {}", stop_name(why));
            } else {
                ffp = self.vm.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
            }
            log_tag = format!("{tag}.recovery");
        }
        self.save_log(&log_tag, from);
        if rewound {
            self.h
                .say(&format!("hvmx: {tag}: reported from a rewind-mode run"));
        }
        if self.h.c.on {
            let k = &self.h.c.k;
            tjinfo.push_str(&format!(
                " stops={} typed={} inh={:016x}",
                k.nstop, k.ntyped, k.inh
            ));
            cap(&mut tjinfo);
        }
        self.result(
            &tag,
            fnv_u64(ffp, wfp),
            wfp,
            now_s() - t0,
            &tjinfo,
            &verdict,
        );
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    /// The serve loop (CTL, RESUME, QUIT); any other request line is a bad request.
    fn serve_loop(&mut self) {
        loop {
            let (tag, sched) = match self.h.port.request() {
                Req::End | Req::Request(Request::Quit) => return,
                Req::Request(Request::Resume { tag, snapshot }) => {
                    self.ctl_resume(&tag, &snapshot);
                    continue;
                }
                Req::Request(Request::Ctl { tag, schedule }) => (tag, schedule),
                Req::BadRun => {
                    self.h.result_err("?", "ERROR bad request");
                    continue;
                }
            };
            let Some(cs) = self.h.snap_find("S1") else {
                self.h
                    .result_err(&tag, "ERROR no snapshot S1 (the boot took none)");
                continue;
            };
            if !sched.guest.is_empty() {
                self.h.result_err(
                    &tag,
                    "ERROR an agent image's schedule has the guest part \"run\" only",
                );
                continue;
            }
            self.ctl_request(&tag, &sched, cs);
        }
    }
}

fn ram_mib_parse(v: &str, what: &str, diag: &Diag) -> Option<u64> {
    let (n, used) = cstr::strtoull(v.as_bytes(), 10);
    if v.is_empty() || used != v.len() || !(RAM_MIN_MIB..=RAM_MAX_MIB).contains(&n) {
        say(
            diag,
            &format!(
                "hvmx: {what}={v}: guest RAM must be a whole number of MiB in [{RAM_MIN_MIB}, {RAM_MAX_MIB}]"
            ),
        );
        return None;
    }
    Some(n)
}

/// The settings from the environment (hvm.c's and serve.c's getenv calls).
pub fn opts_from(o: &HvmOptions, hvmx: bool) -> VmOpts {
    let spin_rewind = o.spin_rewind.clone();
    let mut virt = virtio::Config {
        disk: o.disk.clone(),
        disk_out: o.disk_out.clone(),
        fault: o.fault.clone(),
        fault_classes: o.fault_classes.clone(),
        fault_prob: o.fault_prob.clone(),
        fault_maxdelay: o.fault_maxdelay.clone(),
        net: o.net,
        net_rx: o.net_rx.clone(),
        net_rx_pace: o.net_rx_pace.clone(),
        net_tx_out: o.net_tx_out.clone(),
        net_fault: o.net_fault.clone(),
        net_fault_classes: o.net_fault_classes.clone(),
        net_fault_prob: o.net_fault_prob.clone(),
    };
    if hvmx && virt.disk.is_some() && virt.fault.is_none() {
        // negotiate FLUSH: flush-aware class at probability 0
        virt.fault = Some("0".into());
        virt.fault_classes = Some("unflushed".into());
        virt.fault_prob = Some("0".into());
    }
    let soft = o.soft_irq.clone().map(|v| cstr::atoi(v.as_bytes()) != 0);
    VmOpts {
        ram_size: 0,
        step: o
            .step
            .clone()
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(64),
        echo: o.echo,
        trace: o.trace.clone(),
        trace_watch: o.trace_watch.clone(),
        trace_sysreg: o.trace_sysreg,
        spin_secs: o.spin_secs.clone().map(|v| cstr::atof(&v)).unwrap_or(10.0),
        kick_s: o
            .spin_ms
            .clone()
            .map(|v| cstr::atof(&v) / 1000.0)
            .unwrap_or(0.005),
        spin_observe: o
            .spin_observe
            .clone()
            .map(|v| cstr::atoi(v.as_bytes()) != 0)
            .unwrap_or(true),
        rw_on: spin_rewind.as_deref() == Some("1"),
        splog: o.splog.clone(),
        soft_irq: soft.unwrap_or(hvmx),
        bootargs: o.bootargs.clone().unwrap_or_else(|| BOOTARGS.to_string()),
        virtio: virt,
    }
}

/// hvmx's `main` over the given streams, with the environment from `get`: the command line and
/// the request lines are read here (the text boundary); the exit status.
pub fn main_with(args: &[String], o: &HvmOptions, io: Io) -> i32 {
    let diag: Diag = Arc::new(Mutex::new(io.diag));
    let Io {
        out, input, echo, ..
    } = io;
    let mut a: Vec<String> = args.to_vec();
    let mut ram_mib = None;
    while a.len() > 2 && a[1] == "--ram-mib" {
        match ram_mib_parse(&a[2], "--ram-mib", &diag) {
            Some(n) => ram_mib = Some(n),
            None => return 2,
        }
        a.drain(1..3);
    }
    let control_boot = a.len() > 2 && a[1] == "--control" && a[2] == "boot";
    if control_boot {
        a.drain(1..3);
    } else if a.len() > 1 && matches!(a[1].as_str(), "serve" | "one" | "--control") {
        say(&diag, "hvmx: serve and one without --control boot, and --control one, were the shell-init images' modes and were removed in Milestone 145: every image is an agent image (rebuild an older one with hydo build)");
        return 2;
    }
    let argc = a.len();
    let usage = !control_boot
        || argc < 5
        || (a[1] != "serve" && a[1] != "one")
        || (a[1] == "one" && argc < 6);
    if usage {
        say(&diag, "usage: hvmx [--ram-mib N] --control boot serve|one <Image> <initramfs> <outdir> [\"<schedule>\"]");
        return 2;
    }
    // every schedule is read by hydo-schedule (M142, M143), as on hvx
    let sched = match a.get(5).map(String::as_str) {
        None | Some("|") => Some(Schedule {
            host: Vec::new(),
            guest: Vec::new(),
        }),
        Some(t) => match Schedule::parse(t) {
            Ok(s) => Some(s),
            Err(e) => {
                say(&diag, &format!("hvmx: bad schedule: {e}"));
                return 2;
            }
        },
    };
    let mode = if a[1] == "one" {
        Mode::One(sched.expect("one has a schedule"))
    } else {
        Mode::Serve { boot: sched }
    };
    let start = Start {
        mode,
        kernel: a[2].clone().into(),
        initrd: a[3].clone().into(),
        outdir: a[4].clone().into(),
        ram_mib,
    };
    let port = Box::new(hydo_hvx::text::TextPort::new(input, out));
    run_diag(start, o, port, diag, echo)
}

/// `main` of the program over the process's own streams, with the options its `main` read.
pub fn main_stdio(args: &[String], o: &HvmOptions) -> i32 {
    let io = Io {
        out: Box::new(std::io::LineWriter::new(std::io::stdout())),
        input: Box::new(std::io::BufReader::new(std::io::stdin())),
        diag: Box::new(std::io::stderr()),
        echo: Some(Box::new(std::io::stdout())),
    };
    main_with(args, o, io)
}

/// The engine: boot as `start` says and serve the host over `port` (diagnostics to `diag`, the
/// environment from `get`). The exit status as the program's. One VM per process: a second
/// engine in a process that already runs one ends with an error.
pub fn run(
    start: Start,
    o: &HvmOptions,
    port: Box<dyn Port>,
    diag: Box<dyn Write + Send>,
    echo: Option<Box<dyn Write + Send>>,
) -> i32 {
    run_diag(start, o, port, Arc::new(Mutex::new(diag)), echo)
}

fn run_diag(
    start: Start,
    o: &HvmOptions,
    port: Box<dyn Port>,
    diag: Diag,
    echo: Option<Box<dyn Write + Send>>,
) -> i32 {
    let d2 = diag.clone();
    catch_fatal(&d2, move || engine_main(start, o, port, diag, echo))
}

fn engine_main(
    start: Start,
    o: &HvmOptions,
    port: Box<dyn Port>,
    diag: Diag,
    echo: Option<Box<dyn Write + Send>>,
) -> i32 {
    let mut ram_mib = 1024;
    if let Some(v) = o.mem_mb.clone() {
        match ram_mib_parse(&v, "HVM_MEM_MB", &diag) {
            Some(n) => ram_mib = n,
            None => return 2,
        }
    }
    if let Some(n) = start.ram_mib {
        ram_mib = n;
    }
    let read = |p: &std::path::Path| {
        std::fs::read(p).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", p.display())))
    };
    let kernel = read(&start.kernel);
    let initrd = read(&start.initrd);
    let outdir = start.outdir.display().to_string();
    let _ = std::fs::create_dir(&outdir);
    let mut vo = opts_from(o, true);
    vo.ram_size = ram_mib << 20;
    if ram_mib != 1024 {
        say(&diag, &format!("hvmx: guest RAM {ram_mib} MiB"));
    }
    if vo.rw_on && !vo.spin_observe {
        say(
            &diag,
            "hvm: HVM_SPIN_REWIND=1 needs the observer (HVM_SPIN_OBSERVE)",
        );
        return 2;
    }
    vo.bootargs = format!("{} quiet loglevel=1", vo.bootargs);
    let o = Opts {
        vm: vo.clone(),
        ram_mib,
        run_exits: o
            .run_exits
            .clone()
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(50_000_000),
        snap_dir: o.snap_dir.clone().unwrap_or_else(|| "/tmp/hvm-snap".into()),
        diag: o.diag,
        diag_chunks: o.diag_chunks.clone(),
        check_restore: o.check_restore,
        no_mlock: o.no_mlock,
    };
    let vm = Vmm::new(&vo, &kernel, &initrd, diag.clone(), echo, !o.no_mlock);
    drop(kernel);
    drop(initrd);
    let t0 = now_s();
    let mut e = Engine {
        vm,
        h: Host {
            port,
            diag,
            outdir,
            o,
            c: Ctl::default(),
            snaps: (0..CTL_MAX_SNAPS).map(|_| CtlSnap::default()).collect(),
            g_ctl_one: false,
            g_s0: None,
            spin_hit: false,
        },
    };
    e.vm.max_exits = 200_000_000;
    // M121: the boot under control; S0 and S1 are the host's SNAPs
    {
        let one = matches!(start.mode, Mode::One(_));
        let tag = if one { "one" } else { "boot" };
        let boot = match start.mode {
            Mode::One(s) => s,
            Mode::Serve { boot } => boot.unwrap_or(Schedule {
                host: Vec::new(),
                guest: Vec::new(),
            }),
        };
        let (s, rb, rw) = e.ctl_sched(&boot);
        if !s.guest.is_empty() {
            e.h.say("hvmx: an agent image's schedule has the guest part \"run\" only");
            return 2;
        }
        e.h.g_ctl_one = one;
        let mut why = 0;
        let r = e.agent_boot(tag, rb, &mut why);
        if r != -4 {
            e.agent_boot_result(tag, r, why, t0);
        }
        if e.h.g_ctl_one {
            if r == -4 {
                // the workload from the stop where S1 was taken, as a CTL from S1
                e.h.g_s0 = e.h.snap_find("S0");
                e.h.c.boot_s1 = false;
                e.h.c.phase = Phase::Workload;
                e.h.spin_hit = false;
                e.vm.sp.rw_on = rw; // M130: the rewind mode from S1's stop on
                e.run_once(tag, &s, rw);
                if e.h.c.on {
                    e.h.ctl_finish();
                }
            }
            return 0;
        }
        e.h.g_s0 = e.h.snap_find("S0");
        e.h.emit(EngineMsg::Ready(Ready::new(now_s() - t0)));
        e.serve_loop();
        0
    }
}
