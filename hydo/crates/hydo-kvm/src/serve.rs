//! The run engine's protocol: hydo-hvm's `serve` (the Mac engine, M119, M121, M126) for this
//! machine. It boots an agent image under lock-step control from its first instruction, the host
//! taking the snapshots S0 and S1 where it decides; serves `CTL`, `RESUME` and `QUIT`; runs `one`
//! schedule from a fresh boot under the same control; DISKOFF ends the workload, CUT is a power
//! cut with the recovery boot from the host's S0. The protocol's lines are in hydo-engine-proto's
//! documentation; every line here has the Mac engine's text with `hydo-kvm` for `hvmx`.
//!
//! What is particular to this engine:
//!
//! - **The clock is the exit count**, as on the Mac: `at=` in every EVENT, SENT, STOP and SEND is
//!   the number of VM exits that reached the engine ([`crate::vmm`]). Kicks are not exits.
//! - **The cadence** is the schedule's `rb=<n>` (exits from the phase's start), default 65,536;
//!   a stop is due when the exit count crosses a multiple of it (an instruction that needs two
//!   exits, a long string I/O, can step over one).
//! - **SNAP** is refused while a typed send is not acknowledged unless `retype` is given (typed
//!   input not yet read is in no snapshot); at most 64 snapshots. A snapshot keeps the run's tick
//!   jitter and forced preemption points, and its RESUME restores them (the state search,
//!   docs/design/state-search.md; until then such a SNAP was refused).
//! - **TIME** `<tokens>` at a workload stop replaces the timing from that stop on: the tick
//!   jitter of the tokens, and their `pctn` preemption points among the next `pctk` exits.
//! - **The image**: the x86_64 image's disk (`*-disk.img`, the root on vda1 and the data on vda2)
//!   is the virtio-mmio block device; its kernel is the KVM engine's (`bzImage-kvm`) with the
//!   image's initramfs. The disk boots with the flush-aware fault class at probability 0, so the
//!   guest negotiates FLUSH, as on the other engines. `disk=none|ioerr:P|cut|lying:P` with
//!   `dseed=` arm hvx's seeded fault classes on it.
//! - **Timing tokens**: tick jitter (`tj=`, `tjx=`, delays in exits, `tjmax=` default 1000) and
//!   forced preemption points (`pct=`, `pctn=`, `pctk=`), counted in exits as on the Mac. Host
//!   tokens only hvx applies (`pp=`, `xt*`, `q*`, `cf=`, `cfn`) are refused.
//! - **Several vCPUs** (`--vcpus N`, docs/design/kvm-vcpus.md): the turn tokens `q=` (the turn
//!   length in exits) and `qs=` (seeded lengths in `[1, q]`) apply from the run's start, and the
//!   RESULT line has `qn=`, the turns the run took. Tick jitter is refused with several vCPUs
//!   (not built yet), as are hvx's burst and margin tokens.
//! - **Exit-free loops**: exact cycles are resolved in every run; a counter loop ends the run at
//!   once as a SPIN unless the schedule has `spin=rewind`, which runs its workload, check and
//!   recovery phases in the rewind mode ([`crate::vmm`], Milestone 161; the host runs a SPIN again
//!   with it, as for the Mac engine), and a SNAP keeps the mode for its RESUME. The mode's
//!   checkpoints are taken at loop tops after the stop hook and its replays do not call the hook,
//!   so the stops stay those of the run: a schedule that completes plainly gives the same
//!   fingerprint, stops and `inh=` with the mode on.
//! - **Snapshots** share memory: RAM is a base (S0 and S1 each make one) and the pages that differ
//!   from it, the disk a copy-on-write copy ([`crate::mem`], `hydo_hvm::disk`).
//! - **Guest RAM**: `--ram-mib N`, 256..8192 (default 1024; above 3072 the rest of RAM is at 4 GiB,
//!   `crate::layout::ram_blocks`).

use crate::cpu::Hypervisor;
use crate::fnv::{fnv, fnv_u64, OFFSET};
use crate::layout::*;
use crate::machine::Tjit;
use crate::vmm::{VmOpts, Vmm, XSnap};
use crate::{catch_fatal, fatal, say, Diag};
use hydo_engine_proto::{
    Answer, DiskOff, EngineMsg, Event, HostCmd, Phase, Ready, Request, RunResult, SendAnswer, Sent,
    Stop, StopWhy,
};
use hydo_hvm::machine::Pct;
use hydo_hvm::virtio::{self, F_DROP, F_ERROR, F_UNFLUSHED};
use hydo_hvx::cstr;
use hydo_hvx::port::{Cmd, Port, Req};
use hydo_schedule::{Disk, HostToken, Schedule};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const CTL_MAX_SENDS: usize = hydo_engine_proto::MAX_SENDS;
/// Named snapshots a worker keeps (the host's S0 and S1, an inspection's stop and its branches,
/// docs/design/agent-environments.md). A free slot costs nothing; a used one what its state
/// differs from the RAM base and the disk image by.
const CTL_MAX_SNAPS: usize = 64;
const CTL_SEND_MAX: usize = crate::uart::RXQ;
const CTL_RB_DEFAULT: u64 = 65536;

/// The engine's options (hydo-config's typed group).
pub use hydo_config::kvm::Options as KvmOptions;

/// Where the engine's lines go and where the host's come from (the program).
pub struct Io {
    pub out: Box<dyn Write + Send>,
    pub input: Box<dyn BufRead + Send>,
    pub diag: Box<dyn Write + Send>,
    pub echo: Option<Box<dyn Write + Send>>,
}

/// What [`run`] boots and how.
pub struct Start {
    pub mode: Mode,
    /// the KVM engine's guest kernel (bzImage-kvm)
    pub kernel: std::path::PathBuf,
    /// the image's initramfs (it mounts the root and switches to it)
    pub initrd: std::path::PathBuf,
    /// the image's disk (None: no disk)
    pub disk: Option<std::path::PathBuf>,
    pub outdir: std::path::PathBuf,
    /// `--ram-mib` (None: 1024)
    pub ram_mib: Option<u64>,
    /// `--image-epoch`: the fingerprint epoch the image was built at (None: this hydo's, 2). An
    /// image of epoch 1 runs as it did then (Milestone 204): no full-state cycles unless
    /// HYDO_KVM_SPIN_STATE asks for them, and a fingerprint that keeps KVM's leftover event fields.
    pub image_epoch: Option<u64>,
    /// `--vcpus`: the vCPUs (1: the machine of every recorded value)
    pub vcpus: usize,
}

pub enum Mode {
    /// One schedule from a fresh boot, then exit.
    One(Schedule),
    /// Boot under the host's control with this boot schedule's `rb=` as the cadence, then serve.
    Serve { boot: Option<Schedule> },
}

/// A run from the workload's start on: what run_rest needs, and what a SNAP keeps.
#[derive(Clone, Debug)]
struct RunSt {
    tag: String,
    t0: Instant,
    from: usize,
    cut: bool,
    pct_n: u32,
    wx0: u64,
    tjinfo: String,
    /// the run's phases in the rewind mode (`spin=rewind`)
    rewind: bool,
    /// several vCPUs: the turns ended before the run (for `qn=`)
    turns0: Option<u64>,
}

impl Default for RunSt {
    fn default() -> RunSt {
        RunSt {
            tag: String::new(),
            t0: Instant::now(),
            from: 0,
            cut: false,
            pct_n: 0,
            wx0: 0,
            tjinfo: String::new(),
            rewind: false,
            turns0: None,
        }
    }
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
    state: Cs,
    buf: Vec<u8>,
}

/// What a snapshot carries besides the VM.
#[derive(Clone, Default, Debug)]
struct Book {
    nstop: u64,
    nev: u64,
    next_id: u64,
    ntyped: u64,
    inh: u64,
    cad0: u64,
    evfrom: usize,
    s: Vec<CtlSend>,
    /// Every send before this index is typed or acknowledged for good, so the scans at each exit
    /// start here (a snapshot's copy marks its typed sends for a retype and starts again at 0).
    lo: usize,
}

struct CtlSnap {
    name: String,
    x: XSnap,
    /// the run's timing at the snapshot (host-side, so not in the VM's snapshot)
    tj: Tjit,
    pct: Pct,
    rs: RunSt,
    k: Book,
    rb: u64,
    used: bool,
}

/// The lock-step state.
struct Ctl {
    on: bool,
    /// 1 = the phase's start stop is due, 2 = a RESUME's
    start: i32,
    ended: i32,
    boot_s1: bool,
    /// DISKOFF/CUT given (1), then acted on (2)
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

/// Settings read once.
#[derive(Clone, Debug)]
pub struct Opts {
    pub vm: VmOpts,
    pub run_exits: u64,
    pub diag: bool,
    /// HYDO_KVM_DIAG_RAM: the first this many MiB of guest RAM written next to each fingerprint
    pub diag_ram_mib: u64,
    /// HYDO_KVM_NO_FP: no fingerprints (0), to measure their cost
    pub no_fp: bool,
    /// HYDO_PREEMPT_RECORD: write `<tag>.preempt.jsonl`, a record per forced preemption point
    pub preempt_record: bool,
}

struct Host {
    port: Box<dyn Port>,
    diag: Diag,
    outdir: String,
    o: Opts,
    c: Ctl,
    snaps: Vec<Option<CtlSnap>>,
    g_ctl_one: bool,
    g_s0: Option<usize>,
}

/// The engine: one VM and the host side around it.
pub struct Engine {
    vm: Vmm,
    h: Host,
}

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
        self.snaps
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.used && s.name == name))
    }

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
                "hydo-kvm: {}: send {id}: {} of {len} bytes do not fit in the receive queue",
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
    /// Move the book's start past the sends that are settled (typed or acknowledged).
    fn ctl_settle(&mut self) {
        let k = &mut self.c.k;
        while k.lo < k.s.len() && matches!(k.s[k.lo].state, Cs::Typed | Cs::Acked) {
            k.lo += 1;
        }
    }
    /// The next send to type: the pending send due at this stop first in (at, id) order.
    fn ctl_next(&self) -> Option<usize> {
        let mut best: Option<usize> = None;
        for i in self.c.k.lo..self.c.k.s.len() {
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
        best
    }
    /// Whether send `i` fits in the receive queue now. A send waits until it fits, so that the
    /// guest reads every byte the host sent (open item 82: the records a recovery boot carries
    /// came as several sends at one stop, and the bytes that did not fit were lost). A send is
    /// never longer than the queue, so it fits once the guest has read what came before it.
    fn ctl_fits(&self, vm: &Vmm, i: usize) -> bool {
        self.c.k.s[i].buf.len() as u32 <= vm.rx_room()
    }
    /// every send due at this stop: retypes first (id order), then (at, id) order, while each
    /// fits in the receive queue
    fn ctl_deliver(&mut self, vm: &mut Vmm) {
        self.ctl_settle();
        for i in self.c.k.lo..self.c.k.s.len() {
            if self.c.k.s[i].state == Cs::Retype {
                self.ctl_type(vm, i, true);
            }
        }
        while let Some(b) = self.ctl_next() {
            if !self.ctl_fits(vm, b) {
                return; // typed at the first exit with room (ctl_due)
            }
            self.ctl_type(vm, b, true);
        }
    }
    fn ctl_due(&mut self, vm: &Vmm) -> bool {
        self.ctl_settle();
        let k = &self.c.k;
        k.s[k.lo..].iter().any(|s| s.state == Cs::Retype)
            || self.ctl_next().is_some_and(|i| self.ctl_fits(vm, i))
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
            .or_else(|| self.snaps.iter().position(|s| s.is_none()));
        let Some(slot) = slot else {
            self.refuse(format!("snap {name} no free slot ({CTL_MAX_SNAPS} names)"));
            return;
        };
        let mut x = match self.snaps[slot].take() {
            Some(old) => old.x,
            None => XSnap::default(),
        };
        // S0 and S1, which every run restores, become RAM bases of their own
        vm.snapshot(&mut x, name == "S0" || name == "S1");
        let mut k = self.c.k.clone();
        for s in &mut k.s {
            if s.state == Cs::Typed {
                s.state = Cs::Retype;
            }
        }
        k.lo = 0;
        self.snaps[slot] = Some(CtlSnap {
            name: lossy(&ccap(name.as_bytes().to_vec(), 64)),
            x,
            tj: vm.tj,
            pct: vm.pct,
            rs: self.c.rs.clone(),
            k,
            rb: self.c.rb,
            used: true,
        });
        let at = self.c.at;
        self.answer(Answer::Snap {
            name: name.to_string(),
            at,
            retype: typed,
        });
        if self.g_ctl_one && self.c.phase == Phase::Boot && name == "S1" {
            self.c.boot_s1 = true;
        }
    }

    /// TIME: the timing from this stop on (the state search's branch point).
    fn ctl_time_cmd(&mut self, vm: &mut Vmm, tokens: &str) {
        if self.c.phase != Phase::Workload || self.c.wdone != 0 {
            self.refuse("time only in the workload phase".into());
            return;
        }
        let s = match Schedule::parse(&format!("{tokens} | run")) {
            Ok(s) => s,
            Err(e) => {
                self.refuse(format!("time {e}"));
                return;
            }
        };
        if let Some(bad) = s
            .host
            .iter()
            .find(|t| !hydo_schedule::BRANCH_KEYS.contains(&t.key()))
        {
            self.refuse(format!("time {}= is not a timing token", bad.key()));
            return;
        }
        let mut tj = tick_jitter(&s);
        tj.on = tj_wanted(&tj);
        vm.tj = tj;
        let pct_n = if s.host_token("pct").is_some() {
            num(&s, "pctn").unwrap_or(0) as u32
        } else {
            0
        };
        vm.pct = Pct::default();
        if pct_n != 0 {
            let x = vm.m.exits;
            let k = num(&s, "pctk").unwrap_or(20000);
            vm.pct = Pct::set(num(&s, "pct").unwrap_or(0), pct_n, x, x.wrapping_add(k));
            if vm.pct_log.is_none() && self.o.preempt_record {
                vm.pct_log = Some(Vec::new());
            }
        }
        self.c.rs.pct_n = self.c.rs.pct_n.max(pct_n);
        let at = self.c.at;
        self.answer(Answer::Time { at });
    }

    /// One stop: report, then serve host commands until CONT (false) or END/EOF (true).
    fn ctl_stop(&mut self, vm: &mut Vmm, why: StopWhy) -> bool {
        vm.stops += 1; // the rewind mode takes a checkpoint after a conversation
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
                        self.refuse(hydo_engine_proto::too_many_sends(CTL_MAX_SENDS));
                        continue;
                    }
                    self.c.k.next_id += 1;
                    let id = self.c.k.next_id;
                    self.c.k.s.push(CtlSend {
                        id,
                        at,
                        state: Cs::Pending,
                        buf: bytes,
                    });
                    let i = self.c.k.s.len() - 1;
                    // typed now when it is due, no earlier send waits for room, and it fits
                    if at <= self.c.at && self.ctl_next() == Some(i) && self.ctl_fits(vm, i) {
                        self.ctl_type(vm, i, false);
                        let at = self.c.at;
                        self.answer(Answer::Send(SendAnswer::Typed { id, at }));
                    } else {
                        self.answer(Answer::Send(SendAnswer::Queued { id, at }));
                    }
                }
                HostCmd::Ack(id) => {
                    // the sends are in id order
                    let k = &mut self.c.k;
                    let a = match k.s.binary_search_by_key(&id, |s| s.id) {
                        Err(_) => Answer::Refused(format!("ack {id} unknown")),
                        Ok(i) if k.s[i].state == Cs::Pending => {
                            Answer::Refused(format!("ack {id} not typed yet"))
                        }
                        Ok(i) => {
                            let s = &mut k.s[i];
                            s.state = Cs::Acked;
                            // an acknowledged send is never typed again: its bytes can go
                            s.buf = Vec::new();
                            Answer::Ack(id)
                        }
                    };
                    self.answer(a);
                }
                HostCmd::Snap { name, retype } => self.ctl_snap_cmd(vm, &name, retype),
                HostCmd::Time(tokens) => self.ctl_time_cmd(vm, &tokens),
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

    /// The stop hook at every loop top of a lock-step phase.
    fn ctl_hook(&mut self, vm: &mut Vmm) -> bool {
        let x = vm.m.exits;
        if x == self.c.last_x {
            return false; // a kick: not a new exit
        }
        let crossed = x.wrapping_sub(self.c.k.cad0) / self.c.rb
            != self.c.last_x.wrapping_sub(self.c.k.cad0) / self.c.rb;
        self.c.last_x = x;
        self.c.at = x;
        let why = if self.ctl_due(vm) {
            Some(StopWhy::Send)
        } else if crossed {
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

/// The host tokens hvx alone applies: the first one's refusal (the Mac engine's rule,
/// [`hydo_hvm::machine::host_token_problem`]).
pub fn host_token_problem(s: &Schedule) -> Option<String> {
    hydo_hvm::machine::host_token_problem(s)
}

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

impl Engine {
    fn diag(&self, tag: &str, phase: &str) {
        if !self.h.o.diag {
            return;
        }
        let path = format!("{}/{tag}.diag", self.h.outdir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(self.vm.diag_lines(phase).as_bytes());
            let _ = writeln!(f, "{phase} stats {}", self.vm.stats());
        }
        if self.h.o.diag_ram_mib > 0 {
            let n = ((self.h.o.diag_ram_mib << 20) as usize).min(self.vm.ram.len());
            let p = format!("{}/{tag}.{phase}.ram", self.h.outdir);
            let _ = std::fs::write(p, &self.vm.ram.bytes()[..n]);
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

    /// One phase under lock-step: -3 when the host ended the run (END), -4 (`--control boot one`)
    /// at the stop where the host took S1 and said CONT, -7 at the host's DISKOFF/CUT, -1 when
    /// the guest stopped (the reason in `why`).
    fn run_to(&mut self, why: &mut i32) -> i32 {
        if self.h.c.start != 0 {
            let w = if self.h.c.start == 2 {
                StopWhy::Resume
            } else {
                StopWhy::Start
            };
            if self.h.c.start == 2 {
                self.h.c.k.nstop = self.h.c.k.nstop.wrapping_sub(1);
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
        let s = self.vm.run_until(&mut |vm| h.ctl_hook(vm));
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

    /// The lock-step part of a schedule: its `rb=` taken out (the cadence; 0 = none).
    fn ctl_sched(&self, s: &Schedule) -> (Schedule, u64) {
        let rb = num(s, "rb").unwrap_or(0);
        let mut t = s.clone();
        t.host.retain(|h| !matches!(h, HostToken::Rb(_)));
        (t, rb)
    }

    fn ctl_request(&mut self, tag: &str, sched: &Schedule, cs: usize) {
        let (s, rb) = self.ctl_sched(sched);
        let snap = self.h.snaps[cs].as_ref().expect("S1");
        self.vm.restore(&snap.x);
        let k = snap.k.clone();
        self.h.ctl_begin(tag);
        self.h.c.k = k;
        self.h.c.start = 2;
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.run_once(tag, &s);
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    fn agent_boot(&mut self, tag: &str, rb: u64, why: &mut i32) -> i32 {
        self.h.ctl_begin(tag);
        self.h.c.phase = Phase::Boot;
        if rb != 0 {
            self.h.c.rb = rb;
        }
        self.h.c.rs = RunSt {
            tag: tag.to_string(),
            ..RunSt::default()
        };
        self.run_to(why)
    }

    fn agent_boot_result(&mut self, tag: &str, r: i32, why: i32, t0: Instant) {
        let mut wfp = 0;
        let verdict = if r == -3 {
            wfp = self.xfp();
            self.h.ctl_verdict(&self.vm, r, 0)
        } else {
            format!("ERROR boot: {}", stop_name(why))
        };
        self.save_log(tag, 0);
        let k = &self.h.c.k;
        let extras = format!(" stops={} typed={} inh={:016x}", k.nstop, k.ntyped, k.inh);
        self.result(
            tag,
            fnv_u64(wfp, wfp),
            wfp,
            t0.elapsed().as_secs_f64(),
            &extras,
            &verdict,
        );
        self.h.ctl_finish();
    }

    /// The VM's fingerprint, or 0 with HYDO_KVM_NO_FP.
    fn xfp(&self) -> u64 {
        if self.h.o.no_fp {
            0
        } else {
            self.vm.xfp()
        }
    }

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
                "hydo-kvm: {tag}: a RESULT that does not read back: {verdict}"
            )),
        }
    }

    fn ctl_resume(&mut self, tag: &str, name: &str) {
        let Some(i) = self
            .h
            .snaps
            .iter()
            .rposition(|s| s.as_ref().is_some_and(|s| s.used && s.name == name))
        else {
            self.h.result_err(tag, &format!("ERROR no snapshot {name}"));
            return;
        };
        let snap = self.h.snaps[i].as_ref().expect("a snapshot");
        self.vm.restore(&snap.x);
        // the timing of the run the snapshot was taken in (none for S0, S1 and every snapshot
        // of a run without timing tokens: what a RESUME found before snapshots kept it)
        self.vm.tj = snap.tj;
        self.vm.pct = snap.pct;
        self.vm.pct_log = (snap.rs.pct_n != 0 && self.h.o.preempt_record).then(Vec::new);
        let (k, rb, mut rs) = (snap.k.clone(), snap.rb, snap.rs.clone());
        self.h.ctl_begin(tag);
        self.h.c.k = k;
        self.h.c.start = 2;
        self.h.c.rb = rb;
        rs.tag = lossy(&ccap(tag.as_bytes().to_vec(), 64));
        rs.t0 = Instant::now();
        self.run_rest(rs);
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

    fn unsupported(&self, s: &Schedule) -> Option<&'static str> {
        if !self.vm.vio.attached() && s.host_token("disk").is_some() && s.disk() != Disk::None {
            return Some("disk faults and power cuts need the image's disk");
        }
        None
    }

    /// The first host token this machine does not apply: with one vCPU hvx's tokens; with
    /// several the turn tokens `q=` and `qs=` apply, and tick jitter does not yet.
    fn token_problem(&self, s: &Schedule) -> Option<String> {
        if self.vm.smp.is_none() {
            return host_token_problem(s);
        }
        for t in &s.host {
            match t {
                HostToken::Q(_) | HostToken::Qs(_) => {}
                HostToken::Tj(_) | HostToken::Tjx(_) => {
                    return Some(format!(
                        "host token {t} with several vCPUs (tick jitter is one vCPU's for now)"
                    ))
                }
                _ => {
                    let one = Schedule {
                        host: vec![t.clone()],
                        guest: Vec::new(),
                    };
                    if let Some(p) = host_token_problem(&one) {
                        return Some(p);
                    }
                }
            }
        }
        None
    }

    fn run_once(&mut self, tag: &str, s: &Schedule) {
        let t0 = Instant::now();
        if let Some(hw) = self.token_problem(s) {
            self.h.result_err(tag, &format!("UNSUPPORTED on kvm: {hw}"));
            return;
        }
        if let Some(u) = self.unsupported(s) {
            self.h.result_err(tag, &format!("UNSUPPORTED on kvm: {u}"));
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
            pct_n,
            ..RunSt::default()
        };
        if tj_wanted(&tj) {
            self.vm.tj = tj;
            self.vm.tj.on = true;
            rs.tjinfo = format!(" tjk={}", self.vm.m.vtime / TJ_PERIOD);
        }
        if let Some(sm) = self.vm.smp.as_mut() {
            let q = match s.host_token("q") {
                Some(HostToken::Q(v)) => *v,
                _ => crate::smp::Q_DEFAULT,
            };
            let qs = match s.host_token("qs") {
                Some(HostToken::Qs(v)) => *v,
                _ => 0,
            };
            sm.set_policy(q, qs);
            rs.turns0 = Some(sm.turns);
        }
        rs.wx0 = self.vm.m.exits;
        rs.rewind = s.host_token("spin").is_some();
        if pct_n != 0 {
            let x = self.vm.m.exits;
            self.vm.pct = Pct::set(pct_seed, pct_n, x, x.wrapping_add(pct_k));
        }
        self.vm.pct_log = (pct_n != 0 && self.h.o.preempt_record).then(Vec::new);
        self.run_rest(rs);
    }

    /// The rest of a run from the workload's start: a CTL goes on here, a RESUME starts here.
    fn run_rest(&mut self, rs: RunSt) {
        let tag = rs.tag.clone();
        let mut tjinfo = rs.tjinfo.clone();
        let t0 = rs.t0;
        let (mut wfp, mut ffp) = (0u64, 0u64);
        let wx0 = rs.wx0;
        let pct_n = rs.pct_n;
        let mut cut = rs.cut;
        let mut why = 0;
        let mut from = rs.from;
        let rewind = rs.rewind;
        self.vm.set_rewind(rewind);
        self.h.c.rs = rs;
        let mut r = self.run_to(&mut why);
        if r == -7 {
            r = 0;
            cut |= self.h.c.cut;
        }
        // a TIME at a stop may have switched forced preemption points on (the state search)
        let pct_n = pct_n.max(self.h.c.rs.pct_n);
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
            if let Some(log) = self.vm.pct_log.take() {
                hydo_hvm::machine::write_preempt(&self.h.outdir, &tag, &log);
            }
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
        if let (Some(t0), Some(sm)) = (self.h.c.rs.turns0, self.vm.smp.as_ref()) {
            tjinfo.push_str(&format!(" qn={}", sm.turns.wrapping_sub(t0)));
            cap(&mut tjinfo);
        }
        let mut log_tag = tag.clone();
        let verdict: String;
        'out: {
            if r < 0 && r != -3 {
                verdict = format!("ERROR workload: {}", stop_name(why));
                break 'out;
            }
            wfp = self.xfp();
            self.diag(&tag, "w");
            if r == -3 {
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                ffp = wfp;
                break 'out;
            }
            if !cut {
                self.vm.vio.set_fault(0, 0, 0);
                if self.h.c.on {
                    self.h.c.phase = Phase::Check;
                    self.h.c.start = 1;
                }
                r = self.run_to(&mut why);
                if r < 0 && r != -3 {
                    verdict = format!("ERROR check: {}", stop_name(why));
                    break 'out;
                }
                ffp = self.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
                break 'out;
            }
            // power cut: only the flushed disk bytes survive; recover from the host's S0
            let durable = self.vm.vio.disk.clone();
            self.save_log(&tag, from);
            let Some(s0) = self.h.g_s0 else {
                fatal(3, "hydo-kvm: a power cut without the host's S0 snapshot");
            };
            let snap = self.h.snaps[s0].as_ref().expect("S0");
            self.vm.restore(&snap.x);
            if let (Some(d), Some(du)) = (&mut self.vm.vio.disk, durable) {
                *d = du;
            }
            self.vm.vio.set_fault(0, 0, 0);
            from = self.vm.m.console_len as usize;
            if self.h.c.on {
                self.h.ctl_drop_pending();
                self.h.c.phase = Phase::Recovery;
                self.h.c.start = 1;
                self.h.c.k.evfrom = from;
            }
            r = self.run_to(&mut why);
            if r < 0 && r != -3 {
                verdict = format!("ERROR recovery: {}", stop_name(why));
            } else {
                ffp = self.xfp();
                self.diag(&tag, "f");
                verdict = self.h.ctl_verdict(&self.vm, r, from);
            }
            log_tag = format!("{tag}.recovery");
        }
        self.save_log(&log_tag, from);
        if rewind {
            let rw = &self.vm.rw;
            self.h.say(&format!(
                "hydo-kvm: {tag}: reported from a rewind-mode run (counter loops proven {}, resolved {}; checkpoints {}, exits replayed {})",
                rw.proven, rw.resolved, rw.checkpoints, rw.replayed_exits
            ));
        }
        self.vm.set_rewind(false);
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
            t0.elapsed().as_secs_f64(),
            &tjinfo,
            &verdict,
        );
        if self.h.c.on {
            self.h.ctl_finish();
        }
    }

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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn ram_mib_parse(v: &str, what: &str, diag: &Diag) -> Option<u64> {
    let (n, used) = cstr::strtoull(v.as_bytes(), 10);
    if v.is_empty() || used != v.len() || !(RAM_MIN_MIB..=RAM_MAX_MIB).contains(&n) {
        say(
            diag,
            &format!(
                "hydo-kvm: {what}={v}: guest RAM must be a whole number of MiB in [{RAM_MIN_MIB}, {RAM_MAX_MIB}]"
            ),
        );
        return None;
    }
    Some(n)
}

/// The run loop's settings from the options (a disk, when there is one, negotiates FLUSH: the
/// flush-aware fault class at probability 0).
pub fn vm_opts(o: &KvmOptions, disk: Option<&std::path::Path>) -> VmOpts {
    let virt = virtio::Config {
        disk: disk.map(|d| d.display().to_string()),
        fault: disk.map(|_| "0".to_string()),
        fault_classes: disk.map(|_| "unflushed".to_string()),
        fault_prob: disk.map(|_| "0".to_string()),
        ..virtio::Config::default()
    };
    VmOpts {
        ram_size: RAM_DEFAULT_MIB << 20,
        step: o
            .step
            .clone()
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .filter(|&s| s > 0)
            .unwrap_or(STEP_DEFAULT),
        spin_secs: o.spin_secs.clone().map(|v| cstr::atof(&v)).unwrap_or(10.0),
        spin_look: o
            .spin_ms
            .clone()
            .map(|v| cstr::atof(&v) / 1000.0)
            .unwrap_or(0.005),
        trace: o.trace.clone(),
        trace_unknown: o.trace_unknown,
        spin_state: o.spin_state.unwrap_or(true),
        stale_events: false,
        cmdline: CMDLINE.to_string(),
        virtio: virt,
        vcpus: 1,
    }
}

/// The program's `main` over the given streams: `hydo-kvm [--ram-mib N] [--cpu v2|v3] [--cpu-cache
/// on|off] [--image-epoch N] [--vcpus N] --control boot serve|one <bzImage-kvm> <initramfs>
/// <disk|-> <outdir> ["<schedule>"]`.
#[cfg(target_os = "linux")]
pub fn main_with(args: &[String], o: &KvmOptions, io: Io) -> i32 {
    let diag: Diag = Arc::new(Mutex::new(io.diag));
    let Io {
        out, input, echo, ..
    } = io;
    let mut a: Vec<String> = args.to_vec();
    let mut ram_mib = None;
    let mut level = crate::cpuid::Level::V2;
    let mut cache = false;
    let mut image_epoch = None;
    let mut vcpus = 1;
    while a.len() > 2
        && (a[1] == "--ram-mib"
            || a[1] == "--cpu"
            || a[1] == "--cpu-cache"
            || a[1] == "--image-epoch"
            || a[1] == "--vcpus")
    {
        if a[1] == "--vcpus" {
            match a[2].parse::<usize>() {
                Ok(n) if (1..=crate::smp::MAX_VCPUS).contains(&n) => vcpus = n,
                _ => {
                    say(
                        &diag,
                        &format!(
                            "hydo-kvm: --vcpus {}: expected 1 to {}",
                            a[2],
                            crate::smp::MAX_VCPUS
                        ),
                    );
                    return 2;
                }
            }
        } else if a[1] == "--image-epoch" {
            match a[2].parse::<u64>() {
                Ok(n) if n >= 1 => image_epoch = Some(n),
                _ => {
                    say(
                        &diag,
                        &format!("hydo-kvm: --image-epoch {}: expected a number from 1", a[2]),
                    );
                    return 2;
                }
            }
        } else if a[1] == "--cpu-cache" {
            match a[2].as_str() {
                "on" => cache = true,
                "off" => cache = false,
                v => {
                    say(
                        &diag,
                        &format!("hydo-kvm: --cpu-cache {v}: expected on or off"),
                    );
                    return 2;
                }
            }
        } else if a[1] == "--cpu" {
            match crate::cpuid::Level::parse(&a[2]) {
                Some(l) => level = l,
                None => {
                    say(
                        &diag,
                        &format!("hydo-kvm: --cpu {}: expected v2 or v3", a[2]),
                    );
                    return 2;
                }
            }
        } else {
            match ram_mib_parse(&a[2], "--ram-mib", &diag) {
                Some(n) => ram_mib = Some(n),
                None => return 2,
            }
        }
        a.drain(1..3);
    }
    let control_boot = a.len() > 2 && a[1] == "--control" && a[2] == "boot";
    if control_boot {
        a.drain(1..3);
    }
    let argc = a.len();
    if !control_boot
        || argc < 6
        || (a[1] != "serve" && a[1] != "one")
        || (a[1] == "one" && argc < 7)
    {
        say(&diag, "usage: hydo-kvm [--ram-mib N] [--cpu v2|v3] [--cpu-cache on|off] [--image-epoch N] [--vcpus N] --control boot serve|one <bzImage-kvm> <initramfs> <disk|-> <outdir> [\"<schedule>\"]");
        return 2;
    }
    let sched = match a.get(6).map(String::as_str) {
        None | Some("|") => Schedule {
            host: Vec::new(),
            guest: Vec::new(),
        },
        Some(t) => match Schedule::parse(t) {
            Ok(s) => s,
            Err(e) => {
                say(&diag, &format!("hydo-kvm: bad schedule: {e}"));
                return 2;
            }
        },
    };
    let mode = if a[1] == "one" {
        Mode::One(sched)
    } else {
        Mode::Serve { boot: Some(sched) }
    };
    let start = Start {
        mode,
        kernel: a[2].clone().into(),
        initrd: a[3].clone().into(),
        disk: (a[4] != "-").then(|| a[4].clone().into()),
        outdir: a[5].clone().into(),
        ram_mib,
        image_epoch,
        vcpus,
    };
    let dev = o
        .dev
        .clone()
        .unwrap_or_else(|| hydo_config::kvm::DEFAULT_DEV.to_string());
    let hv = match crate::sys::Kvm::open(&dev).and_then(|k| k.with_level(level)) {
        Ok(k) => k.with_cache(cache),
        Err(e) => {
            say(&diag, &e);
            return 1;
        }
    };
    let port = Box::new(hydo_hvx::text::TextPort::new(input, out));
    run_diag(start, o, port, diag, echo, Box::new(hv))
}

/// The engine over `port` with the vCPUs `hv` makes: boot as `start` says and serve the host.
/// The exit status as the program's.
pub fn run(
    start: Start,
    o: &KvmOptions,
    port: Box<dyn Port>,
    diag: Box<dyn Write + Send>,
    echo: Option<Box<dyn Write + Send>>,
    hv: Box<dyn Hypervisor>,
) -> i32 {
    run_diag(start, o, port, Arc::new(Mutex::new(diag)), echo, hv)
}

fn run_diag(
    start: Start,
    o: &KvmOptions,
    port: Box<dyn Port>,
    diag: Diag,
    echo: Option<Box<dyn Write + Send>>,
    hv: Box<dyn Hypervisor>,
) -> i32 {
    let d2 = diag.clone();
    catch_fatal(&d2, move || engine_main(start, o, port, diag, echo, hv))
}

fn engine_main(
    start: Start,
    o: &KvmOptions,
    port: Box<dyn Port>,
    diag: Diag,
    echo: Option<Box<dyn Write + Send>>,
    hv: Box<dyn Hypervisor>,
) -> i32 {
    let ram_mib = start.ram_mib.unwrap_or(RAM_DEFAULT_MIB);
    let read = |p: &std::path::Path| {
        std::fs::read(p).unwrap_or_else(|e| fatal(1, &format!("{}: {e}", p.display())))
    };
    let kernel = read(&start.kernel);
    let initrd = read(&start.initrd);
    let outdir = start.outdir.display().to_string();
    let _ = std::fs::create_dir(&outdir);
    let mut vo = vm_opts(o, start.disk.as_deref());
    // what the image's own fingerprint epoch decides (Milestone 204; hydo_image::defaults)
    let before_2 = start.image_epoch.is_some_and(|e| e < 2);
    vo.spin_state = o.spin_state.unwrap_or(!before_2);
    vo.stale_events = before_2;
    vo.ram_size = ram_mib << 20;
    vo.vcpus = start.vcpus.max(1);
    if vo.vcpus > 1 {
        say(&diag, &format!("hydo-kvm: {} vCPUs", vo.vcpus));
    }
    if ram_mib != RAM_DEFAULT_MIB {
        say(&diag, &format!("hydo-kvm: guest RAM {ram_mib} MiB"));
    }
    vo.cmdline = format!("{} quiet loglevel=1", vo.cmdline);
    let opts = Opts {
        vm: vo.clone(),
        run_exits: o
            .run_exits
            .clone()
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(50_000_000),
        diag: o.diag,
        diag_ram_mib: o
            .diag_ram
            .clone()
            .map(|v| cstr::strtoull0(v.as_bytes()))
            .unwrap_or(0),
        no_fp: o.no_fp,
        preempt_record: o.preempt_record,
    };
    let echo = if o.echo { echo } else { None };
    let vm = Vmm::new(&vo, &kernel, &initrd, hv, diag.clone(), echo);
    drop(kernel);
    drop(initrd);
    let t0 = Instant::now();
    let mut e = Engine {
        vm,
        h: Host {
            port,
            diag,
            outdir,
            o: opts,
            c: Ctl::default(),
            snaps: (0..CTL_MAX_SNAPS).map(|_| None).collect(),
            g_ctl_one: false,
            g_s0: None,
        },
    };
    let one = matches!(start.mode, Mode::One(_));
    let tag = if one { "one" } else { "boot" };
    let boot = match start.mode {
        Mode::One(s) => s,
        Mode::Serve { boot } => boot.unwrap_or(Schedule {
            host: Vec::new(),
            guest: Vec::new(),
        }),
    };
    let (s, rb) = e.ctl_sched(&boot);
    if !s.guest.is_empty() {
        e.h.say("hydo-kvm: an agent image's schedule has the guest part \"run\" only");
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
            e.h.g_s0 = e.h.snap_find("S0");
            e.h.c.boot_s1 = false;
            e.h.c.phase = Phase::Workload;
            e.run_once(tag, &s);
            if e.h.c.on {
                e.h.ctl_finish();
            }
        }
        return 0;
    }
    e.h.g_s0 = e.h.snap_find("S0");
    e.h.emit(EngineMsg::Ready(Ready::new(t0.elapsed().as_secs_f64())));
    e.serve_loop();
    0
}
