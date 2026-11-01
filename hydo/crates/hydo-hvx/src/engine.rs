//! The engine: `hv/hvx/hvx_explore.c` (and the parts of `hvx_stack_verify.c` it uses) in Rust,
//! function by function, driving `/dev/hvx` from this process. The same requests give the same
//! output lines (RESULT, READY, BRANCHPOINT, the lock-step lines) and the same consoles; the
//! differences are inside: snapshots stay in memory as their non-zero pages, a fresh VM is filled
//! with the non-zero pages of what it needs only, fingerprints skip zero pages exactly, and old
//! VMs are torn down on a helper thread while the next run starts.

use crate::abi::*;
use crate::boot;
use crate::cstr::{atof, find, strtoull, strtoull0, Scan};
use crate::ctl::{self, Book, Send, SendState};
use crate::fnv::{fnv, fnv_sched_state, fnv_snapshot, fnv_zskip, BASIS};
use crate::pages::Pages;
use crate::sched::Sched;
use crate::vm::{Mapped, Vm, UBUF_CAP};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

const EXP_MEM_MIB: u64 = 1024;
const EXP_MEM_MIN_MIB: u64 = 256;
const EXP_MEM_MAX_MIB: u64 = 3072;
const S0_MARKER: &[u8] = b"hydo-explore-ready-S0";
const S1_MARKER: &[u8] = b"hydo-explore-ready-S1";
const DONE_MARKER: &[u8] = b"hydo-explore-workload-done";
const FINAL_MARKER: &[u8] = b"hydo-stack-marker: workload complete";
const ACKED_PREFIX: &[u8] = b"hydo-explore-acked:";
const FAILED_PREFIX: &[u8] = b"hydo-explore-failed:";
const AGENT_PREFIX: &[u8] = b"HYDO-AG ";
const BRANCH_BUDGET: u64 = 16;
const BOOT_TSC_STEP: u64 = 0x10000;
const JIT: &str = "c1sync";

/// The engine's environment knobs (the variables hvx_explore and hvx_stack_verify.c read), read
/// once. `from_lookup` takes the same names; see `hvx_explore.c` for each.
#[derive(Clone, Debug)]
pub struct Options {
    pub dev: String,
    pub vcpus: u32,
    pub park: bool,
    pub cpu_v3: bool,
    pub pmu_clock: bool,
    pub pause_exit: bool,
    pub pt_trace_vm: bool,
    pub pause_k: Option<String>,
    pub wall_secs: f64,
    pub turn_margin: Option<String>,
    pub quantum: Option<String>,
    pub burst: Option<String>,
    pub quantum_after: bool,
    pub ftstat: bool,
    pub cmdline_extra: Option<String>,
    pub phases: bool,
    pub no_unsync: bool,
    pub dump: Option<String>,
    pub dump_hashonly: bool,
    pub durable_out: Option<String>,
    pub bootcon: bool,
    pub console_out: Option<String>,
    pub drain_no_retry: bool,
    pub vcpustat: bool,
    pub ram_mib_env: Option<String>,
    pub vcpus_env: Option<String>,
    /// HVX_EXPLORE_PT_ELF / HVX_EXPLORE_PT_RANGES: coverage tracing, not in this engine.
    pub pt: bool,
    /// HYDO_HVX_REAPER=0: tear old VMs down in line, as the C engine does (default: on a thread).
    pub reaper: bool,
    /// HYDO_HVX_CHECK_ZERO=1: verify that a fresh VM's RAM and disk are zero before a restore.
    pub check_zero: bool,
}

impl Options {
    pub fn from_lookup(get: &dyn Fn(&str) -> Option<String>) -> Options {
        let set_not0 = |k: &str| get(k).is_some_and(|v| v != "0");
        let not_0 = |k: &str| get(k).as_deref() != Some("0");
        let wall = get("HVX_EXPLORE_WALL_SECS")
            .map(|v| atof(&v))
            .filter(|&v| v > 0.0)
            .unwrap_or(900.0);
        let dirty = set_not0("HVX_EXPLORE_DIRTY");
        Options {
            dev: get("HVX_DEV").unwrap_or_else(|| "/dev/hvx".into()),
            vcpus: 1,
            park: set_not0("HVX_VCPU_PARK"),
            cpu_v3: set_not0("HVX_CPU_V3"),
            pmu_clock: not_0("HVX_PMU_CLOCK"),
            pause_exit: not_0("HVX_PAUSE_EXIT"),
            pt_trace_vm: get("HVX_PT_TRACE_VM").is_some(),
            pause_k: get("HVX_PAUSE_K"),
            wall_secs: wall,
            turn_margin: get("HVX_TURN_MARGIN"),
            quantum: get("HVX_QUANTUM"),
            burst: get("HVX_BURST"),
            quantum_after: get("HVX_QUANTUM_AFTER").is_some(),
            ftstat: get("HVX_FTSTAT").is_some(),
            cmdline_extra: get("HVX_EXPLORE_CMDLINE_EXTRA"),
            phases: set_not0("HVX_EXPLORE_PHASES") || dirty,
            no_unsync: get("HVX_EXPLORE_NO_UNSYNC").is_some(),
            dump: get("HVX_EXPLORE_DUMP"),
            dump_hashonly: get("HVX_EXPLORE_DUMP_HASHONLY").is_some(),
            durable_out: get("HVX_EXPLORE_DURABLE_OUT"),
            bootcon: get("HVX_EXPLORE_BOOTCON").is_some(),
            console_out: get("HVX_CONSOLE_OUT"),
            drain_no_retry: get("HVX_DRAIN_NO_RETRY").is_some(),
            vcpustat: get("HVX_EXPLORE_VCPUSTAT").is_some(),
            ram_mib_env: get("HVX_EXPLORE_RAM_MIB"),
            vcpus_env: get("HVX_EXPLORE_VCPUS"),
            pt: get("HVX_EXPLORE_PT_ELF").is_some() || get("HVX_EXPLORE_PT_RANGES").is_some(),
            reaper: not_0("HYDO_HVX_REAPER"),
            check_zero: set_not0("HYDO_HVX_CHECK_ZERO"),
        }
    }

    /// From the process environment.
    pub fn from_env() -> Options {
        Options::from_lookup(&|k| std::env::var(k).ok())
    }
}

/// The inputs every VM is built from (`struct env`).
struct Env {
    mem_size: u64,
    image: Mapped,
    initrd: Mapped,
    base: Mapped,
    disk_len: u64,
    p2_off: u64,
    p2_len: u64,
    cmdbase: String,
    /// The base image's non-zero 4 KiB blocks as merged (offset, length) runs.
    base_runs: Vec<(usize, usize)>,
}

/// `struct boot`: one VM and its reconstructed console.
struct Boot {
    vm: Option<Vm>,
    ubuf: Vec<u8>,
    drained: usize,
    p2_off: usize,
    p2_len: usize,
}

impl Boot {
    fn none() -> Boot {
        Boot {
            vm: None,
            ubuf: Vec::new(),
            drained: 0,
            p2_off: 0,
            p2_len: 0,
        }
    }
    fn vm(&self) -> &Vm {
        self.vm.as_ref().expect("a VM")
    }
    fn ulen(&self) -> usize {
        self.ubuf.len()
    }
    fn p2(&self) -> &[u8] {
        &self.vm().disk()[self.p2_off..self.p2_off + self.p2_len]
    }
    fn p2_mut(&mut self) -> &mut [u8] {
        let (o, l) = (self.p2_off, self.p2_len);
        &mut self.vm.as_mut().expect("a VM").disk_mut()[o..o + l]
    }
    /// `mem_find(ubuf + from, ulen - from, needle) >= 0` (with `ulen > from`).
    fn has_after(&self, from: usize, needle: &[u8]) -> bool {
        self.ulen() > from && find(&self.ubuf[from..], needle).is_some()
    }
}

/// Other vCPUs and the scheduler (`struct smpstate`).
struct Smp {
    v: Vec<hvx_snapshot>,
    sch: Box<hvx_sched>,
}

/// `struct snap`.
struct Snap {
    s: Box<hvx_snapshot>,
    smp: Option<Smp>,
    ram: Pages,
    rw: Pages,
    con: Vec<u8>,
    unsync: Vec<u8>,
    unsync_n: u32,
}

/// `struct ctl_snap`.
struct CtlSnap {
    name: String,
    sn: Snap,
    sc: Sched,
    from: usize,
    tjinfo: String,
    k: Book,
}

/// The C engine's `C` (lock-step state of the current run).
struct Ctl {
    on: bool,
    start: u8,
    capped: bool,
    boot_s1: bool,
    wdone: u8,
    cut: bool,
    tag: String,
    phase: &'static str,
    at: u64,
    k: Book,
    endv: String,
    sc: Box<Sched>,
    from: usize,
    tjinfo: String,
}

impl Ctl {
    fn zero() -> Ctl {
        Ctl {
            on: false,
            start: 0,
            capped: false,
            boot_s1: false,
            wdone: 0,
            cut: false,
            tag: String::new(),
            phase: "",
            at: 0,
            k: Book::default(),
            endv: String::new(),
            sc: Box::new(Sched::empty()),
            from: 0,
            tjinfo: String::new(),
        }
    }
}

const PH_NAMES: [&str; 21] = [
    "fresh",
    "r_snap",
    "r_ram",
    "r_disk",
    "r_con",
    "setup",
    "workload",
    "wfp",
    "savecon",
    "check",
    "ffp",
    "durable",
    "rec_teardown",
    "rec_fresh",
    "rec_restore",
    "recovery",
    "rfp",
    "teardown",
    "run_ioctl",
    "drain",
    "dirtyscan",
];
#[derive(Clone, Copy)]
enum Ph {
    Fresh = 0,
    RSnap,
    RRam,
    RDisk,
    RCon,
    Setup,
    Work,
    Wfp,
    SaveCon,
    Check,
    Ffp,
    Durable,
    RTear,
    RFresh,
    RRestore,
    Recov,
    Rfp,
    Tear,
    RunIoctl,
    Drain,
}

/// How a VM is set up: a real boot, or a fresh VM a snapshot is restored into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setup {
    Boot,
    Fresh,
}

/// Where the engine's lines go and where the host's come from.
pub struct Io {
    /// stdout: the protocol (each line flushed).
    pub out: Box<dyn Write + std::marker::Send>,
    /// stdin: requests, and the host's lines in a controlled run.
    pub input: Box<dyn BufRead + std::marker::Send>,
    /// stderr: diagnostics.
    pub diag: Box<dyn Write + std::marker::Send>,
}

/// Truncate to at most `cap - 1` bytes (a C buffer of `cap` bytes), on a char boundary.
fn ccap(s: &mut String, cap: usize) {
    if s.len() >= cap {
        let mut n = cap - 1;
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        s.truncate(n);
    }
}

fn now() -> f64 {
    thread_local!(static T0: Instant = Instant::now());
    T0.with(|t| t.elapsed().as_secs_f64())
}

pub struct Core {
    o: Options,
    e: Env,
    outdir: PathBuf,
    io: Io,
    budget: u64,
    c: Ctl,
    snaps: Vec<Option<Box<CtlSnap>>>,
    s0: Option<Box<Snap>>,
    s1: Option<Box<Snap>>,
    ctl_one: bool,
    ctl_boot: bool,
    q_old_quantum: u64,
    q_old_seed: u64,
    q_turns0: u64,
    pp_old_period: u64,
    pp_old_tsc: u64,
    ss_br0: [u64; HVX_SCHED_MAX_VCPUS],
    ss_turns0: [u64; HVX_SCHED_MAX_VCPUS],
    vs_br0: [u64; HVX_SCHED_MAX_VCPUS],
    vs_t0: [u64; HVX_SCHED_MAX_VCPUS],
    con: Box<hvx_console>,
    pmu_noted: bool,
    reaper: Option<std::sync::mpsc::SyncSender<Vm>>,
    reaper_join: Option<std::thread::JoinHandle<()>>,
    boot_line: Option<String>,
    ph: [f64; 21],
    ph_t0: f64,
    ph_calls: u64,
    ph_extra: String,
    ph_rsplit: bool,
    line_buf: Vec<u8>,
}

impl Drop for Core {
    fn drop(&mut self) {
        self.reaper = None;
        if let Some(j) = self.reaper_join.take() {
            let _ = j.join();
        }
    }
}

macro_rules! diag {
    ($self:ident, $($a:tt)*) => {{
        let _ = writeln!($self.io.diag, $($a)*);
        let _ = $self.io.diag.flush();
    }};
}

impl Core {
    fn say(&mut self, line: &str) {
        let _ = self.io.out.write_all(line.as_bytes());
        let _ = self.io.out.write_all(b"\n");
        let _ = self.io.out.flush();
    }

    fn pt(&self) -> f64 {
        if self.o.phases {
            now()
        } else {
            0.0
        }
    }
    fn padd(&mut self, i: Ph, t: f64) {
        if self.o.phases {
            self.ph[i as usize] += now() - t;
        }
    }
    fn ph_reset(&mut self) {
        self.ph = [0.0; 21];
        self.ph_calls = 0;
        self.ph_extra.clear();
        self.ph_t0 = now();
    }
    fn ph_print(&mut self, tag: &str) {
        if !self.o.phases {
            return;
        }
        let mut l = format!(
            "PHASES {tag} t0={:.3} total={:.3}",
            self.ph_t0,
            now() - self.ph_t0
        );
        for (i, n) in PH_NAMES.iter().enumerate() {
            l.push_str(&format!(" {n}={:.3}", self.ph[i]));
        }
        l.push_str(&format!(
            " runcalls={}{} engine=rust",
            self.ph_calls, self.ph_extra
        ));
        diag!(self, "{l}");
    }

    // ---- VMs ----

    /// `boot_setup`: a VM with the base disk; `Setup::Boot` also loads the kernel (a real boot),
    /// `Setup::Fresh` leaves RAM and the data partition zero for a restore.
    fn boot_setup(&mut self, mode: Setup, cmdline: &str, fault_classes: u32) -> Option<Boot> {
        let mut b = Boot::none();
        b.ubuf.reserve(UBUF_CAP);
        b.p2_off = self.e.p2_off as usize;
        b.p2_len = self.e.p2_len as usize;
        let mut vm = match Vm::open(&self.o.dev) {
            Ok(v) => v,
            Err(e) => {
                diag!(self, "open /dev/hvx: {e}");
                return None;
            }
        };
        let vcpus = self.o.vcpus;
        let mut cfg = hvx_config {
            mem_size: self.e.mem_size,
            n_vcpus: 0,
            flags: HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT,
        };
        if self.o.pause_exit {
            cfg.flags |= HVX_VM_PAUSE_EXIT;
        }
        if self.o.pmu_clock {
            cfg.flags |= HVX_VM_PMU_CLOCK;
        }
        if self.o.cpu_v3 {
            cfg.flags |= HVX_VM_X86_64_V3;
        }
        if self.o.pt_trace_vm {
            cfg.flags |= HVX_VM_PT_TRACE;
        }
        if vcpus > 1 {
            cfg.n_vcpus = vcpus;
            cfg.flags |= HVX_VM_SMP_SCHED;
            if !self.o.park {
                cfg.flags |= HVX_VM_SMP_BOOT;
            }
            if cfg.flags & HVX_VM_PMU_CLOCK == 0 {
                diag!(
                    self,
                    "hvx_stack_verify: {vcpus} vCPUs need the PMU clock (HVX_PMU_CLOCK=0 given)"
                );
                return None;
            }
        }
        if let Err(e) = vm.create(&cfg) {
            if vcpus > 1 && e.raw_os_error() == Some(libc::ENODEV) {
                diag!(self, "hvx_stack_verify: {vcpus} vCPUs need the PMU clock; this host has no usable PMU");
                return None;
            }
            if e.raw_os_error() != Some(libc::ENODEV) || cfg.flags & HVX_VM_PMU_CLOCK == 0 {
                diag!(self, "HVX_CREATE_VM: {e}");
                return None;
            }
            if !self.pmu_noted {
                self.pmu_noted = true;
                diag!(
                    self,
                    "hvx_stack_verify: no usable PMU on this host; using the exit-count clock"
                );
            }
            cfg.flags &= !HVX_VM_PMU_CLOCK;
            if let Err(e) = vm.create(&cfg) {
                diag!(self, "HVX_CREATE_VM: {e}");
                return None;
            }
        }
        if let Err(e) = vm.map_ram(self.e.mem_size as usize) {
            diag!(self, "mmap RAM: {e}");
            return None;
        }
        let mut tsc = hvx_tsc {
            tsc: 0,
            step: BOOT_TSC_STEP,
            aux: 0,
            count: 0,
            mode: HVX_TSC_COUNTER,
        };
        if let Err(e) = vm.io(HVX_SET_TSC, &mut tsc) {
            diag!(self, "HVX_SET_TSC: {e}");
            return None;
        }
        let mut dk = hvx_disk {
            len: self.e.disk_len,
            data: 0,
            flags: HVX_DISK_NO_IDMAP,
        };
        if let Err(e) = vm.io(HVX_SET_DISK, &mut dk) {
            diag!(self, "HVX_SET_DISK: {e}");
            return None;
        }
        if let Err(e) = vm.map_disk(self.e.disk_len as usize) {
            diag!(self, "mmap disk window: {e}");
            return None;
        }
        // the image's non-zero blocks (the page array is zero); a fresh VM leaves the data
        // partition zero, the restore writes it
        {
            let base = self.e.base.bytes();
            let (lo, hi) = (b.p2_off, b.p2_off + b.p2_len);
            let disk = vm.disk_mut();
            for &(o, n) in &self.e.base_runs {
                let (s, t) = (o, o + n);
                if mode == Setup::Boot || t <= lo || s >= hi {
                    disk[s..t].copy_from_slice(&base[s..t]);
                    continue;
                }
                if s < lo {
                    disk[s..lo].copy_from_slice(&base[s..lo]);
                }
                if t > hi {
                    disk[hi..t].copy_from_slice(&base[hi..t]);
                }
            }
        }
        if fault_classes != 0 {
            let mut f = hvx_fault {
                classes: fault_classes,
                ..Default::default()
            };
            if let Err(e) = vm.io(HVX_SET_FAULT, &mut f) {
                diag!(self, "HVX_SET_FAULT: {e}");
                return None;
            }
        }
        let ncpus = if vcpus > 1 && !self.o.park { vcpus } else { 1 };
        if mode == Setup::Boot {
            let mut w = Vec::new();
            boot::build_page_tables_and_gdt(vm.mem_mut(), &mut w);
            let image = self.e.image.bytes();
            let initrd = self.e.initrd.bytes();
            match boot::load_bzimage_n(vm.mem_mut(), image, initrd, cmdline, ncpus, &mut w) {
                Ok(l) => diag!(self, "{l}"),
                Err(m) => {
                    diag!(self, "{m}");
                    return None;
                }
            }
        } else {
            // the C engine loads a kernel into every fresh VM too (overwritten by the restore);
            // here RAM stays zero and only its line is printed, for the same log
            if self.boot_line.is_none() {
                let mut scratch = vec![0u8; self.e.mem_size as usize];
                let mut w = Vec::new();
                self.boot_line = boot::load_bzimage_n(
                    &mut scratch,
                    self.e.image.bytes(),
                    self.e.initrd.bytes(),
                    cmdline,
                    ncpus,
                    &mut w,
                )
                .ok();
            }
            if let Some(l) = self.boot_line.clone() {
                diag!(self, "{l}");
            }
        }
        let mut sregs = boot::fill_sregs();
        if let Err(e) = vm.io(HVX_SET_SREGS, &mut sregs) {
            diag!(self, "HVX_SET_SREGS: {e}");
            return None;
        }
        let mut regs = hvx_regs {
            rip: boot::KERNEL_LOAD_ADDR + boot::KERNEL_ENTRY_OFF,
            rsi: boot::BOOT_PARAMS_ADDR,
            rsp: boot::STACK_TOP,
            rflags: 0x2,
            ..Default::default()
        };
        if let Err(e) = vm.io(HVX_SET_REGS, &mut regs) {
            diag!(self, "HVX_SET_REGS: {e}");
            return None;
        }
        if vcpus > 1 && self.o.park && self.park_aps(&vm).is_err() {
            return None;
        }
        if vcpus > 1 && !self.o.park {
            let mut c = hvx_sched::zeroed_box();
            if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *c).is_err() {
                diag!(self, "HVX_GET_SCHED: failed");
                return None;
            }
            c.pause_k = match &self.o.pause_k {
                Some(p) => strtoull0(p.as_bytes()) as u32,
                None => 4,
            };
            if let Err(e) = vm.io(HVX_SET_SCHED, &mut *c) {
                diag!(self, "HVX_SET_SCHED: {e}");
                return None;
            }
        }
        if vcpus > 1 && !self.o.quantum_after && self.turn_knobs(&vm).is_err() {
            return None;
        }
        b.vm = Some(vm);
        Some(b)
    }

    fn park_aps(&mut self, vm: &Vm) -> Result<(), ()> {
        for i in 1..self.o.vcpus {
            let mut r = hvx_regs {
                rflags: 0x2,
                ..Default::default()
            };
            if vm.set_vcpu(i).is_err() || vm.io(HVX_SET_REGS, &mut r).is_err() {
                diag!(self, "park: HVX_SET_REGS: failed");
                return Err(());
            }
        }
        let mut c = hvx_sched::zeroed_box();
        if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *c).is_err() {
            diag!(self, "park: HVX_GET_SCHED: failed");
            return Err(());
        }
        for i in 1..self.o.vcpus as usize {
            c.v[i].halted = 1;
            c.v[i].halt_len = 1;
        }
        if vm.io(HVX_SET_SCHED, &mut *c).is_err() {
            diag!(self, "park: HVX_SET_SCHED: failed");
            return Err(());
        }
        Ok(())
    }

    /// `turn_knobs` (hvx_stack_verify.c): HVX_QUANTUM / HVX_TURN_MARGIN / HVX_BURST at boot setup.
    fn turn_knobs(&mut self, vm: &Vm) -> Result<(), ()> {
        let (q, m, bu) = (
            self.o.quantum.clone(),
            self.o.turn_margin.clone(),
            self.o.burst.clone(),
        );
        if self.o.vcpus < 2 || (q.is_none() && m.is_none() && bu.is_none()) {
            return Ok(());
        }
        if self.o.ftstat {
            let _ = vm.reset_ftstat();
        }
        if let Some(q) = &q {
            let mut c = hvx_sched::zeroed_box();
            if vm.io(HVX_GET_SCHED, &mut *c).is_err() {
                diag!(self, "HVX_GET_SCHED (HVX_QUANTUM): failed");
                return Err(());
            }
            c.quantum = strtoull0(q.as_bytes());
            if vm.io(HVX_SET_SCHED, &mut *c).is_err() {
                diag!(self, "HVX_SET_SCHED (HVX_QUANTUM): failed");
                return Err(());
            }
        }
        if m.is_some() || bu.is_some() {
            let mut t = hvx_turnctl::default();
            if let Some(m) = &m {
                t.margin = strtoull0(m.as_bytes());
            }
            if let Some(bu) = &bu {
                let mut s = Scan::new(bu.as_bytes());
                let bl = s.ull();
                s.lit(":");
                let bq = s.ull();
                s.lit(":");
                let pm = s.uint();
                s.lit(":");
                let sd = s.hex();
                if s.n < 3 {
                    diag!(self, "bad HVX_BURST");
                    return Err(());
                }
                t.burst_len = bl.unwrap_or(0);
                t.burst_q = bq.unwrap_or(0);
                t.burst_permille = pm.unwrap_or(0);
                t.burst_seed = sd.unwrap_or(1);
            }
            if vm.io(HVX_SET_TURNCTL, &mut t).is_err() {
                diag!(self, "HVX_SET_TURNCTL: failed");
                return Err(());
            }
        }
        let f = |k: &str, v: &Option<String>| match v {
            Some(v) => format!(" {k}={v}"),
            None => String::new(),
        };
        diag!(
            self,
            "hvx_stack_verify: turn knobs applied:{}{}{}",
            f("quantum", &q),
            f("margin", &m),
            f("burst", &bu)
        );
        Ok(())
    }

    fn teardown(&mut self, b: &mut Boot) {
        b.ubuf = Vec::new();
        if let Some(vm) = b.vm.take() {
            match &self.reaper {
                Some(tx) => {
                    if let Err(e) = tx.send(vm) {
                        drop(e.0);
                    }
                }
                None => drop(vm),
            }
        }
    }

    /// `vm_fresh`: a VM ready for a restore.
    fn vm_fresh(&mut self) -> Option<Boot> {
        let b = self.boot_setup(Setup::Fresh, "console=ttyS0", 0)?;
        if self.o.check_zero {
            let vm = b.vm();
            if !crate::fnv::is_zero(vm.mem()) || !crate::fnv::is_zero(b.p2()) {
                diag!(
                    self,
                    "hydo-hvx: a fresh VM's RAM or data partition is not zero"
                );
                return None;
            }
        }
        Some(b)
    }

    // ---- the console ----

    fn console_fetch(&mut self, b: &Boot) -> Result<(), ()> {
        let mut tries = 0u32;
        loop {
            match b.vm().get_console(&mut self.con) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    if self.o.drain_no_retry {
                        return Err(());
                    }
                    if tries == 0 || tries.is_multiple_of(1000) {
                        diag!(
                            self,
                            "hvx_stack_verify: HVX_GET_CONSOLE failed ({e}), retrying"
                        );
                    }
                    if tries >= 10000 {
                        diag!(
                            self,
                            "hvx_stack_verify: HVX_GET_CONSOLE still failing after {tries} tries"
                        );
                        return Err(());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            tries += 1;
        }
    }

    /// `console_drain`. Err: the console cannot be read (the C engine exits with status 3).
    fn console_drain(&mut self, b: &mut Boot) -> Result<(), ()> {
        if self.console_fetch(b).is_err() {
            if self.o.drain_no_retry {
                return Ok(());
            }
            diag!(
                self,
                "hvx_stack_verify: cannot read the guest console; giving up"
            );
            return Err(());
        }
        let modlen = (self.con.len as usize).min(HVX_CONSOLE_SZ);
        if modlen <= b.drained {
            return Ok(());
        }
        let mut add = modlen - b.drained;
        if b.ulen() + add > UBUF_CAP {
            add = UBUF_CAP - b.ulen();
        }
        b.ubuf
            .extend_from_slice(&self.con.buf[b.drained..b.drained + add]);
        b.drained = modlen;
        Ok(())
    }

    fn console_rebase(&mut self, b: &mut Boot) -> Result<(), ()> {
        if self.console_fetch(b).is_err() {
            if self.o.drain_no_retry {
                return Ok(());
            }
            diag!(
                self,
                "hvx_stack_verify: cannot read the guest console; giving up"
            );
            return Err(());
        }
        b.drained = (self.con.len as usize).min(HVX_CONSOLE_SZ);
        Ok(())
    }

    fn console_dump(&mut self, b: &Boot) {
        if let Some(p) = &self.o.console_out {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
            {
                let _ = f.write_all(b"\n===== hvx_stack_verify: console of one boot =====\n");
                let _ = f.write_all(&b.ubuf);
            }
        }
    }

    fn save_console(&self, b: &Boot, from: usize, tag: &str, suffix: &str) {
        let path = self.outdir.join(format!("{tag}{suffix}.log"));
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            if b.ulen() > from {
                let _ = f.write_all(&b.ubuf[from..]);
            }
        }
    }

    // ---- fingerprints ----

    fn get_snap(&mut self, b: &Boot) -> Option<Box<hvx_snapshot>> {
        let mut s = hvx_snapshot::zeroed_box();
        match b.vm().get_snapshot(&mut s) {
            Ok(()) => Some(s),
            Err(e) => {
                diag!(self, "HVX_GET_SNAPSHOT: {e}");
                None
            }
        }
    }

    fn fnv_smp(&mut self, mut h: u64, b: &Boot) -> u64 {
        let vm = b.vm();
        h = fnv(h, b"hvx-smp-v1");
        let mut s = hvx_snapshot::zeroed_box();
        for i in 1..self.o.vcpus {
            *s = hvx_snapshot::zeroed();
            if vm.set_vcpu(i).is_err() || vm.get_snapshot(&mut s).is_err() {
                let _ = vm.set_vcpu(0);
                return 0;
            }
            h = fnv_snapshot(h, &s);
        }
        let mut c = hvx_sched::zeroed_box();
        if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *c).is_err() {
            return 0;
        }
        fnv_sched_state(h, &c, self.o.vcpus as usize)
    }

    /// `fingerprint`: console + snapshot (+ other vCPUs) + all RAM + the data partition.
    fn fingerprint(&mut self, b: &Boot) -> u64 {
        let Some(s) = self.get_snap(b) else {
            return 0;
        };
        let mut h = fnv(BASIS, b"hvx-stack-state-v2");
        h = fnv(h, &(b.ulen() as u64).to_le_bytes());
        h = fnv(h, &b.ubuf);
        h = fnv_snapshot(h, &s);
        if self.o.vcpus > 1 {
            h = self.fnv_smp(h, b);
        }
        h = fnv(h, &self.e.mem_size.to_le_bytes());
        h = fnv_zskip(h, b.vm().mem());
        h = fnv(h, &(b.p2_len as u64).to_le_bytes());
        fnv_zskip(h, b.p2())
    }

    fn dump_state(&mut self, b: &Boot, tag: &str, phase: &str) {
        let Some(dir) = self.o.dump.clone() else {
            return;
        };
        if b.vm.is_none() {
            return;
        }
        let Some(s) = self.get_snap(b) else {
            return;
        };
        diag!(
            self,
            "SUBFP {tag} {phase} ulen={} con={:016x} snap={:016x} ram={:016x} disk={:016x}",
            b.ulen(),
            fnv(BASIS, &b.ubuf),
            fnv_snapshot(BASIS, &s),
            fnv_zskip(BASIS, b.vm().mem()),
            fnv_zskip(BASIS, b.p2())
        );
        if !self.o.dump_hashonly {
            let w = |ext: &str, d: &[u8]| {
                let _ = std::fs::write(Path::new(&dir).join(format!("{tag}.{phase}.{ext}")), d);
            };
            w("con", &b.ubuf);
            w("snap", s.as_bytes());
            w("ram", b.vm().mem());
            w("disk", b.p2());
        }
    }

    // ---- snapshots ----

    fn snap_take(&mut self, b: &Boot) -> Option<Snap> {
        let s = self.get_snap(b)?;
        let smp = if self.o.vcpus > 1 {
            let vm = b.vm();
            let mut v = Vec::new();
            for i in 1..self.o.vcpus {
                let mut x = hvx_snapshot::zeroed_box();
                if vm.set_vcpu(i).is_err() || vm.get_snapshot(&mut x).is_err() {
                    diag!(self, "HVX_GET_SNAPSHOT (vCPU): failed");
                    let _ = vm.set_vcpu(0);
                    return None;
                }
                v.push(*x);
            }
            let mut sch = hvx_sched::zeroed_box();
            if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *sch).is_err() {
                diag!(self, "HVX_GET_SCHED: failed");
                return None;
            }
            Some(Smp { v, sch })
        } else {
            None
        };
        let ram = Pages::store(b.vm().mem());
        let rw = Pages::store(b.p2());
        let mut unsync = vec![0u8; unsync_bytes(HVX_UNSYNC_MAX) as usize];
        let mut io = hvx_unsync_io {
            addr: unsync.as_mut_ptr() as u64,
            len: unsync.len() as u64,
            n: 0,
            pad: 0,
        };
        if let Err(e) = b.vm().io(HVX_GET_UNSYNC, &mut io) {
            diag!(self, "HVX_GET_UNSYNC: {e}");
            return None;
        }
        unsync.truncate(unsync_bytes(io.n as u64) as usize);
        Some(Snap {
            s,
            smp,
            ram,
            rw,
            con: b.ubuf.clone(),
            unsync,
            unsync_n: io.n,
        })
    }

    /// `snap_restore` into a fresh VM; `rw` replaces the snapshot's data partition.
    fn snap_restore(&mut self, b: &mut Boot, sn: &Snap, rw: Option<&Pages>) -> Result<(), ()> {
        let mut pht = self.pt();
        if let Err(e) = b.vm().set_snapshot(&sn.s) {
            diag!(self, "HVX_SET_SNAPSHOT: {e}");
            return Err(());
        }
        if let Some(smp) = &sn.smp {
            let vm = b.vm();
            for (k, v) in smp.v.iter().enumerate() {
                if vm.set_vcpu(k as u32 + 1).is_err() || vm.set_snapshot(v).is_err() {
                    let _ = vm.set_vcpu(0);
                    diag!(self, "HVX_SET_SNAPSHOT (vCPU): failed");
                    return Err(());
                }
            }
            let mut sch = *smp.sch;
            if vm.set_vcpu(0).is_err() || vm.io(HVX_SET_SCHED, &mut sch).is_err() {
                diag!(self, "HVX_SET_SCHED: failed");
                return Err(());
            }
        } else if self.o.vcpus > 1 {
            diag!(self, "smp_set: no scheduler state");
            return Err(());
        }
        if self.ph_rsplit {
            self.padd(Ph::RSnap, pht);
            pht = self.pt();
        }
        sn.ram
            .load_into_zero(b.vm.as_mut().expect("a VM").mem_mut());
        if self.ph_rsplit {
            self.padd(Ph::RRam, pht);
            pht = self.pt();
        }
        rw.unwrap_or(&sn.rw).load_into_zero(b.p2_mut());
        if self.ph_rsplit {
            self.padd(Ph::RDisk, pht);
            pht = self.pt();
        }
        b.ubuf.clear();
        b.ubuf.extend_from_slice(&sn.con);
        self.console_rebase(b)?;
        let mut io = hvx_unsync_io {
            addr: sn.unsync.as_ptr() as u64,
            len: unsync_bytes(sn.unsync_n as u64),
            n: sn.unsync_n,
            pad: 0,
        };
        if self.o.no_unsync {
            io.n = 0;
            io.len = 0;
        }
        if let Err(e) = b.vm().io(HVX_SET_UNSYNC, &mut io) {
            diag!(self, "HVX_SET_UNSYNC: {e}");
            return Err(());
        }
        if self.ph_rsplit {
            self.padd(Ph::RCon, pht);
        }
        Ok(())
    }

    fn fresh_restore(&mut self, sn: &Snap) -> Option<Boot> {
        let mut b = self.vm_fresh()?;
        if self.snap_restore(&mut b, sn, None).is_err() {
            self.teardown(&mut b);
            return None;
        }
        Some(b)
    }

    // ---- input and the disk fault ----

    fn put_bytes(&mut self, b: &Boot, p: &[u8]) -> Result<(), ()> {
        for c in p.chunks(HVX_INPUT_MAX) {
            let mut inp = hvx_input {
                len: c.len() as u32,
                buf: [0; HVX_INPUT_MAX],
            };
            inp.buf[..c.len()].copy_from_slice(c);
            if let Err(e) = b.vm().io(HVX_PUT_INPUT, &mut inp) {
                diag!(self, "HVX_PUT_INPUT: {e}");
                return Err(());
            }
        }
        Ok(())
    }

    fn set_fault(&mut self, b: &Boot, seed: u64, classes: u32, prob: u32) -> Result<(), ()> {
        let mut f = hvx_fault {
            seed,
            classes,
            probability: prob,
            ..Default::default()
        };
        if let Err(e) = b.vm().io(HVX_SET_FAULT, &mut f) {
            diag!(self, "HVX_SET_FAULT: {e}");
            return Err(());
        }
        Ok(())
    }

    // ---- lock-step control ----

    fn ctl_icount(&mut self, b: &Boot) -> u64 {
        let mut s = hvx_snapshot::zeroed_box();
        if b.vm().get_snapshot(&mut s).is_ok() {
            s.icount
        } else {
            0
        }
    }

    fn ctl_begin(&mut self, tag: &str) {
        self.c = Ctl::zero();
        let mut t = tag.to_string();
        ccap(&mut t, 64);
        self.c.tag = t;
        self.c.on = true;
        self.c.start = 1;
        self.c.phase = "workload";
    }

    fn ctl_type(&mut self, b: &Boot, i: usize, print: bool) {
        let retype = self.c.k.s[i].state == SendState::Retype;
        let buf = self.c.k.s[i].buf.clone();
        let _ = self.put_bytes(b, &buf);
        let at = self.c.at;
        let s = &mut self.c.k.s[i];
        s.state = SendState::Typed;
        if !retype {
            s.sent = at;
            let len = s.buf.len() as u32;
            let k = &mut self.c.k;
            k.ntyped += 1;
            k.inh = fnv(if k.inh != 0 { k.inh } else { BASIS }, &at.to_le_bytes());
            k.inh = fnv(k.inh, &len.to_le_bytes());
            k.inh = fnv(k.inh, &buf);
        }
        if print {
            let l = format!(
                "SENT {} id={} at={}{}",
                self.c.tag,
                self.c.k.s[i].id,
                at,
                if retype { " retype=1" } else { "" }
            );
            self.say(&l);
        }
    }

    fn ctl_deliver(&mut self, b: &Boot, print: bool) -> usize {
        let mut n = 0;
        for i in 0..self.c.k.s.len() {
            if self.c.k.s[i].state == SendState::Retype {
                self.ctl_type(b, i, print);
                n += 1;
            }
        }
        loop {
            let mut best: Option<usize> = None;
            for (i, s) in self.c.k.s.iter().enumerate() {
                if s.state != SendState::Pending || s.at > self.c.at {
                    continue;
                }
                match best {
                    None => best = Some(i),
                    Some(j) => {
                        let bs = &self.c.k.s[j];
                        if s.at < bs.at || (s.at == bs.at && s.id < bs.id) {
                            best = Some(i);
                        }
                    }
                }
            }
            match best {
                None => return n,
                Some(i) => {
                    self.ctl_type(b, i, print);
                    n += 1;
                }
            }
        }
    }

    fn ctl_due(&self) -> bool {
        self.c.k.s.iter().any(|s| {
            s.state == SendState::Retype || (s.state == SendState::Pending && s.at <= self.c.at)
        })
    }

    fn ctl_cap(&self) -> u64 {
        let mut cap = 0u64;
        for s in &self.c.k.s {
            if s.state != SendState::Pending || s.at <= self.c.at {
                continue;
            }
            let c = ((s.at - self.c.at) / 2048).max(1);
            if cap == 0 || c < cap {
                cap = c;
            }
        }
        cap
    }

    fn ctl_drop_pending(&mut self) {
        for i in 0..self.c.k.s.len() {
            let st = self.c.k.s[i].state;
            if st == SendState::Pending || st == SendState::Retype {
                self.c.k.s[i].state = SendState::Acked;
                let l = format!("DROP {} id={}", self.c.tag, self.c.k.s[i].id);
                self.say(&l);
            }
        }
    }

    fn ctl_find(&self, name: &str) -> Option<usize> {
        self.snaps
            .iter()
            .position(|s| s.as_ref().is_some_and(|s| s.name == name))
    }

    fn ctl_snap_cmd(&mut self, b: &Boot, arg: &[u8]) {
        let mut sc = Scan::new(arg);
        let name = sc.word(63).map(|w| String::from_utf8_lossy(w).into_owned());
        let mode = name.as_ref().and_then(|_| sc.word(15).map(|w| w.to_vec()));
        let n = sc.n;
        let name = name.unwrap_or_default();
        if n < 1 || (n == 2 && mode.as_deref() != Some(b"retype")) {
            self.say("ERR snap bad request");
            return;
        }
        let phase = self.c.phase;
        if phase != "workload" && phase != "boot" {
            self.say(&format!(
                "ERR snap {name} only in the workload or boot phase"
            ));
            return;
        }
        let s = &self.c.sc;
        if phase != "boot" && (s.tj_on || s.xt_on || s.pp != 0 || s.pptsc != 0 || s.q_on || s.cf_on)
        {
            self.say(&format!("ERR snap {name} not for tj/xt/pp/q/cf schedules"));
            return;
        }
        if self.c.wdone != 0 {
            self.say(&format!("ERR snap {name} after DISKOFF/CUT"));
            return;
        }
        let mut ids = String::new();
        let mut il = 0usize; // the C buffer's would-be length
        for s in &self.c.k.s {
            if s.state == SendState::Typed {
                let piece = format!("{}{}", if il != 0 { "," } else { "" }, s.id);
                il += piece.len();
                ids.push_str(&piece);
            }
        }
        if il >= 512 {
            ids.truncate(512 - 5);
            ids.push_str(",...");
        }
        if il != 0 && n < 2 {
            self.say(&format!("ERR snap {name} unacked={ids}"));
            return;
        }
        let slot = match self.ctl_find(&name) {
            Some(i) => {
                self.snaps[i] = None;
                i
            }
            None => match self.snaps.iter().position(|s| s.is_none()) {
                Some(i) => i,
                None => {
                    self.say(&format!(
                        "ERR snap {name} no free slot ({} names)",
                        ctl::MAX_SNAPS
                    ));
                    return;
                }
            },
        };
        let Some(sn) = self.snap_take(b) else {
            self.say(&format!("ERR snap {name} snapshot failed"));
            return;
        };
        let mut k = self.c.k.clone();
        for s in &mut k.s {
            if s.state == SendState::Typed {
                s.state = SendState::Retype;
            }
        }
        let mut tjinfo = self.c.tjinfo.clone();
        ccap(&mut tjinfo, 160);
        self.snaps[slot] = Some(Box::new(CtlSnap {
            name: name.clone(),
            sn,
            sc: *self.c.sc,
            from: self.c.from,
            tjinfo,
            k,
        }));
        self.say(&format!(
            "OK snap {name} at={} retype={}",
            self.c.at,
            if il != 0 { ids.as_str() } else { "-" }
        ));
        if self.ctl_one && self.c.phase == "boot" && name == "S1" {
            self.c.boot_s1 = true;
        }
    }

    /// A host line (getline, the new line and carriage returns cut, up to a NUL). None at EOF.
    fn host_line(&mut self) -> Option<Vec<u8>> {
        let mut l = Vec::new();
        match self.io.input.read_until(b'\n', &mut l) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                while matches!(l.last(), Some(b'\n') | Some(b'\r')) {
                    l.pop();
                }
                if let Some(z) = l.iter().position(|&c| c == 0) {
                    l.truncate(z);
                }
                Some(l)
            }
        }
    }

    /// `ctl_stop`: report, then serve host lines until CONT (false) or END/EOF (true).
    fn ctl_stop(&mut self, b: &Boot, why: &str) -> bool {
        let mut p = self.c.k.evfrom;
        let mut lines = Vec::new();
        let first = p;
        for q in first..b.ulen() {
            if b.ubuf[q] != b'\n' {
                continue;
            }
            let mut e = q;
            if e > p && b.ubuf[e - 1] == b'\r' {
                e -= 1;
            }
            self.c.k.nev += 1;
            let mut l = format!("EVENT {} at={} n={} ", self.c.tag, self.c.at, self.c.k.nev);
            ctl::esc(&b.ubuf[p..e], &mut l);
            lines.push(l);
            p = q + 1;
        }
        for l in lines {
            self.say(&l);
        }
        self.c.k.evfrom = p;
        self.ctl_deliver(b, true);
        self.c.k.nstop = self.c.k.nstop.wrapping_add(1);
        let l = format!(
            "STOP {} at={} n={} phase={} why={}",
            self.c.tag, self.c.at, self.c.k.nstop, self.c.phase, why
        );
        self.say(&l);
        loop {
            let Some(ln) = self.host_line() else {
                self.c.endv = "ERROR control channel closed".into();
                return true;
            };
            if ln == b"CONT" {
                return false;
            }
            if ln.starts_with(b"END") && (ln.len() == 3 || ln[3] == b' ') {
                let mut v = if ln.len() > 3 {
                    String::from_utf8_lossy(&ln[4..]).into_owned()
                } else {
                    String::new()
                };
                ccap(&mut v, 256);
                self.c.endv = v;
                return true;
            }
            if let Some(r) = ln.strip_prefix(b"SEND at=") {
                let (at, used) = strtoull(r, 10);
                let rest = &r[used..];
                let payload = if used == 0 || rest.first() != Some(&b' ') {
                    None
                } else {
                    ctl::unesc(&rest[1..])
                };
                let Some(buf) = payload else {
                    self.say("ERR send bad request");
                    continue;
                };
                let len = buf.len();
                if len == 0 || len > ctl::SEND_MAX {
                    self.say(&format!(
                        "ERR send length {len} not in [1, {}]",
                        ctl::SEND_MAX
                    ));
                    continue;
                }
                if self.c.k.s.len() >= ctl::MAX_SENDS {
                    self.say(&format!(
                        "ERR send more than {} sends in one run",
                        ctl::MAX_SENDS
                    ));
                    continue;
                }
                self.c.k.next_id += 1;
                let id = self.c.k.next_id;
                self.c.k.s.push(Send {
                    id,
                    at,
                    sent: 0,
                    state: SendState::Pending,
                    buf,
                });
                if at <= self.c.at {
                    let i = self.c.k.s.len() - 1;
                    self.ctl_type(b, i, false);
                    let l = format!("OK send id={id} sent={}", self.c.at);
                    self.say(&l);
                } else {
                    self.say(&format!("OK send id={id} queued={at}"));
                }
            } else if let Some(r) = ln.strip_prefix(b"ACK ") {
                let id = strtoull(r, 10).0;
                match self.c.k.s.iter().position(|s| s.id == id) {
                    None => self.say(&format!("ERR ack {id} unknown")),
                    Some(i) => {
                        if self.c.k.s[i].state == SendState::Pending {
                            self.say(&format!("ERR ack {id} not typed yet"));
                        } else {
                            self.c.k.s[i].state = SendState::Acked;
                            self.say(&format!("OK ack {id}"));
                        }
                    }
                }
            } else if let Some(r) = ln.strip_prefix(b"SNAP ") {
                let r = r.to_vec();
                self.ctl_snap_cmd(b, &r);
            } else if ln == b"DISKOFF" || ln == b"CUT" {
                let cut = ln[0] == b'C';
                let no = if !self.ctl_boot {
                    Some("only for an agent image (--control boot)")
                } else if self.c.phase != "workload" {
                    Some("only in the workload phase")
                } else if self.c.wdone != 0 {
                    Some("after DISKOFF/CUT")
                } else if cut && self.c.sc.cut {
                    Some("for a cf= schedule (its DISKOFF is the cut)")
                } else if cut && self.ctl_find("S0").is_none() {
                    Some("without a snapshot S0")
                } else {
                    None
                };
                if let Some(no) = no {
                    self.say(&format!("ERR {} {no}", if cut { "cut" } else { "diskoff" }));
                    continue;
                }
                self.c.wdone = 1;
                self.c.cut = cut;
                if cut {
                    let l = format!("OK cut at={}", self.c.at);
                    self.say(&l);
                    return false;
                }
                let l = format!(
                    "OK diskoff at={} cut={}",
                    self.c.at,
                    if self.c.sc.cut { 1 } else { 0 }
                );
                self.say(&l);
            } else {
                let mut l = "ERR unknown command ".to_string();
                ctl::esc(&ln, &mut l);
                self.say(&l);
            }
        }
    }

    // ---- running ----

    /// One run call; Err on an ioctl error. Returns the exit reason (STEP reported as THROTTLE).
    fn run_call(&mut self, b: &Boot) -> Result<u32, ()> {
        let vm = b.vm();
        if self.c.on {
            let cap = self.ctl_cap();
            self.c.capped = cap != 0 && (self.budget == 0 || cap < self.budget);
            let mut c = hvx_run_ctl {
                max_insns: if self.c.capped { cap } else { self.budget },
                ..Default::default()
            };
            loop {
                match vm.io_once(HVX_RUNC, &mut c) {
                    Ok(()) => break,
                    Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                    Err(e) => {
                        diag!(self, "hvx_explore: HVX_RUNC: {e}");
                        return Err(());
                    }
                }
            }
            self.c.at = c.icount;
            return Ok(if c.reason == HVX_EXIT_STEP {
                HVX_EXIT_THROTTLE
            } else {
                c.reason
            });
        }
        if self.budget != 0 {
            let mut c = hvx_run_ctl {
                max_insns: self.budget,
                ..Default::default()
            };
            loop {
                match vm.io_once(HVX_RUNC, &mut c) {
                    Ok(()) => break,
                    Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                    Err(e) => {
                        diag!(self, "hvx_explore: HVX_RUNC: {e}");
                        return Err(());
                    }
                }
            }
            return Ok(if c.reason == HVX_EXIT_STEP {
                HVX_EXIT_THROTTLE
            } else {
                c.reason
            });
        }
        let mut ex = hvx_exit::default();
        loop {
            match vm.io_once(HVX_RUN, &mut ex) {
                Ok(()) => return Ok(ex.reason),
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => {
                    diag!(self, "hvx_explore: HVX_RUN: {e}");
                    return Err(());
                }
            }
        }
    }

    /// `run_until_any`: the index of the first marker seen after `from`; -1 VM error, -2 wall
    /// limit, -3 the host ended the run, -4 the host took S1 in a `--control boot one` boot,
    /// -7 the host said DISKOFF or CUT.
    fn run_until_any(&mut self, b: &mut Boot, mk: &[&[u8]], from: usize) -> i32 {
        let t0 = now();
        if self.c.on && self.c.start != 0 {
            let why = if self.c.start == 2 { "resume" } else { "start" };
            if self.c.start == 2 {
                self.c.k.nstop = self.c.k.nstop.wrapping_sub(1);
            }
            self.c.start = 0;
            self.c.at = self.ctl_icount(b);
            if self.ctl_stop(b, why) {
                return -3;
            }
            if self.c.boot_s1 {
                return -4;
            }
            if self.c.wdone == 1 {
                self.c.wdone = 2;
                return -7;
            }
        }
        // markers: where the scan of each may start again (a found one returns)
        let mut scanned: Vec<usize> = vec![from; mk.len()];
        let first_hit = |b: &Boot, scanned: &mut Vec<usize>| -> i32 {
            for (i, m) in mk.iter().enumerate() {
                if b.ulen() > from && !m.is_empty() {
                    let st = scanned[i].max(from);
                    if b.ulen() >= st && find(&b.ubuf[st..], m).is_some() {
                        return i as i32;
                    }
                    scanned[i] = b.ulen().saturating_sub(m.len() - 1).max(from);
                }
            }
            -1
        };
        let h = first_hit(b, &mut scanned);
        if h >= 0 {
            return h;
        }
        loop {
            let pht = self.pt();
            self.ph_calls += 1;
            let reason = match self.run_call(b) {
                Ok(r) => r,
                Err(()) => return -1,
            };
            self.padd(Ph::RunIoctl, pht);
            let pht = self.pt();
            if self.console_drain(b).is_err() {
                return -1;
            }
            self.padd(Ph::Drain, pht);
            let hit = first_hit(b, &mut scanned);
            if self.c.on
                && (hit >= 0 || !self.c.capped || reason != HVX_EXIT_THROTTLE || self.ctl_due())
            {
                let why = if hit >= 0 {
                    "marker"
                } else if reason != HVX_EXIT_THROTTLE {
                    "exit"
                } else if self.ctl_due() {
                    "send"
                } else {
                    "cadence"
                };
                if self.ctl_stop(b, why) {
                    return -3;
                }
                if self.c.boot_s1 {
                    return -4;
                }
                if self.c.wdone == 1 {
                    self.c.wdone = 2;
                    return -7;
                }
            }
            if hit >= 0 {
                return hit;
            }
            if reason != HVX_EXIT_THROTTLE {
                diag!(
                    self,
                    "hvx_explore: guest stopped: reason={reason} ({})",
                    boot::exit_reason_name(reason)
                );
                return -1;
            }
            if now() - t0 > self.o.wall_secs {
                return -2;
            }
        }
    }

    /// `run_until_tick` (BRANCH): to tick `target` (0: until the write cache holds writes).
    fn run_until_tick(&mut self, b: &mut Boot, target: u64, from: usize) -> i32 {
        let t0 = now();
        loop {
            let mut c = hvx_run_ctl {
                max_insns: self.budget,
                ..Default::default()
            };
            loop {
                match b.vm().io_once(HVX_RUNC, &mut c) {
                    Ok(()) => break,
                    Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                    Err(e) => {
                        diag!(self, "hvx_explore: HVX_RUNC: {e}");
                        return -1;
                    }
                }
            }
            if self.console_drain(b).is_err() {
                return -1;
            }
            if c.reason != HVX_EXIT_STEP && c.reason != HVX_EXIT_THROTTLE {
                diag!(
                    self,
                    "hvx_explore: guest stopped: reason={} ({})",
                    c.reason,
                    boot::exit_reason_name(c.reason)
                );
                return -1;
            }
            if now() - t0 > self.o.wall_secs {
                return -2;
            }
            for (i, m) in [DONE_MARKER, FINAL_MARKER].iter().enumerate() {
                if b.has_after(from, m) {
                    return i as i32 + 1;
                }
            }
            if target == 0 {
                let mut io = hvx_unsync_io::default();
                let _ = b.vm().io(HVX_GET_UNSYNC, &mut io);
                if io.n != 0 {
                    return 0;
                }
                continue;
            }
            let mut pm = hvx_pmu::default();
            if b.vm().io(HVX_GET_PMU, &mut pm).is_err() || pm.period == 0 {
                return -1;
            }
            if pm.next / pm.period >= target {
                return 0;
            }
        }
    }

    // ---- verdicts ----

    /// `last_verdict`: the last `HYDO-ORACLE:` line after `from` (at most 255 bytes).
    fn last_verdict(b: &Boot, from: usize) -> String {
        let u = &b.ubuf;
        let mut at: Option<usize> = None;
        let mut i = from;
        while i + 12 <= u.len() {
            if &u[i..i + 12] == b"HYDO-ORACLE:" {
                at = Some(i);
            }
            i += 1;
        }
        let Some(at) = at else {
            return "ERROR no verdict".into();
        };
        let mut v = Vec::new();
        let mut i = 0;
        while at + i < u.len() && u[at + i] != b'\n' && u[at + i] != b'\r' && i + 1 < 256 {
            v.push(u[at + i]);
            i += 1;
        }
        String::from_utf8_lossy(&v).into_owned()
    }

    fn append_line(b: &Boot, from: usize, prefix: &[u8], out: &mut Vec<u8>, n: usize) {
        if b.ulen() <= from {
            return;
        }
        if let Some(at) = find(&b.ubuf[from..], prefix) {
            let mut p = from + at + prefix.len();
            while p < b.ulen() && b.ubuf[p] != b'\n' && b.ubuf[p] != b'\r' && out.len() + 2 < n {
                out.push(b.ubuf[p]);
                p += 1;
            }
        }
    }

    /// `oracle_cmd`: the recovery boot's `oracle <acked ids> [failed <ids>]\n` (3584 bytes max).
    fn oracle_cmd(b: &Boot, from: usize) -> Vec<u8> {
        const N: usize = 3584;
        let mut o = b"oracle".to_vec();
        Self::append_line(b, from, ACKED_PREFIX, &mut o, N);
        if b.has_after(from, FAILED_PREFIX) && o.len() + 8 < N {
            o.extend_from_slice(b" failed");
            Self::append_line(b, from, FAILED_PREFIX, &mut o, N);
        }
        o.push(b'\n');
        o
    }

    fn result_line(&mut self, tag: &str, fp: u64, wfp: u64, t0: f64, tjinfo: &str, verdict: &str) {
        let l = format!(
            "RESULT {tag} fp={fp:016x} wfp={wfp:016x} secs={:.1}{tjinfo} verdict={verdict}",
            now() - t0
        );
        self.say(&l);
    }

    /// `sched_fail`: the RESULT of a schedule that failed before its workload; consumes b.
    fn sched_fail(
        &mut self,
        b: &mut Boot,
        tag: &str,
        from: usize,
        t0: f64,
        tjinfo: &str,
        verdict: &str,
    ) {
        if b.vm.is_some() {
            self.save_console(b, from, tag, "");
        }
        let fp = fnv(0, &0u64.to_le_bytes());
        self.result_line(tag, fp, 0, t0, tjinfo, verdict);
        self.teardown(b);
    }

    fn schedstat(&mut self, b: &Boot, tag: &str, start: bool) {
        if self.o.vcpus < 2 {
            return;
        }
        let mut c = hvx_sched::zeroed_box();
        if b.vm().io(HVX_GET_SCHED, &mut *c).is_err() {
            return;
        }
        let n = (self.o.vcpus as usize).min(HVX_SCHED_MAX_VCPUS);
        if start {
            for i in 0..n {
                self.ss_br0[i] = c.v[i].branches;
                self.ss_turns0[i] = c.v[i].turns;
            }
            return;
        }
        let mut l = format!("SCHEDSTAT {tag}");
        for i in 0..n {
            l.push_str(&format!(
                " v{i}_br={} v{i}_turns={}",
                c.v[i].branches.wrapping_sub(self.ss_br0[i]),
                c.v[i].turns.wrapping_sub(self.ss_turns0[i])
            ));
        }
        diag!(self, "{l}");
    }

    fn vcpustat(&mut self, b: &Boot, tag: Option<&str>) {
        let mut c = hvx_sched::zeroed_box();
        if b.vm().io(HVX_GET_SCHED, &mut *c).is_err() {
            diag!(self, "HVX_GET_SCHED (vcpustat): failed");
            return;
        }
        let n = self.o.vcpus as usize;
        match tag {
            None => {
                for i in 0..n {
                    self.vs_br0[i] = c.v[i].branches;
                    self.vs_t0[i] = c.v[i].turns;
                }
            }
            Some(t) => {
                let mut l = format!("VCPUSTAT {t}");
                for i in 0..n {
                    l.push_str(&format!(
                        " v{i} br={} t={}",
                        c.v[i].branches.wrapping_sub(self.vs_br0[i]),
                        c.v[i].turns.wrapping_sub(self.vs_t0[i])
                    ));
                }
                diag!(self, "{l}");
            }
        }
    }

    fn vcpustat_on(&self) -> bool {
        self.o.vcpus > 1 && self.o.vcpustat
    }

    /// The S0 a power cut's recovery restores: the worker's, or an agent image's host S0.
    fn take_s0(&mut self) -> Option<(Box<Snap>, Option<usize>)> {
        if self.ctl_boot {
            let i = self.ctl_find("S0")?;
            let mut cs = self.snaps[i].take()?;
            let sn = std::mem::replace(&mut cs.sn, empty_snap());
            self.snaps[i] = Some(cs);
            Some((Box::new(sn), Some(i)))
        } else {
            self.s0.take().map(|s| (s, None))
        }
    }

    fn put_s0(&mut self, s: Box<Snap>, slot: Option<usize>) {
        match slot {
            Some(i) => {
                if let Some(cs) = self.snaps[i].as_mut() {
                    cs.sn = *s;
                }
            }
            None => self.s0 = Some(s),
        }
    }

    /// `finish_sched`: the workload to its end, then the check or the recovery; the RESULT.
    fn finish_sched(
        &mut self,
        mut b: Boot,
        tag: &str,
        sc: &Sched,
        mut from: usize,
        t0: f64,
        tjinfo_in: &str,
    ) {
        let wk: [&[u8]; 2] = [DONE_MARKER, FINAL_MARKER];
        let fin: [&[u8]; 1] = [FINAL_MARKER];
        let nmk = |n: usize, ab: bool| if ab { 0 } else { n };
        let verdict: String;
        let mut tjinfo = tjinfo_in.to_string();
        ccap(&mut tjinfo, 160);
        let (mut wfp, mut ffp) = (0u64, 0u64);
        *self.c.sc = *sc;
        self.c.from = from;
        self.c.tjinfo = tjinfo_in.to_string();
        let ab = self.ctl_boot;
        let pht = self.pt();
        let mut r = self.run_until_any(&mut b, &wk[..nmk(2, ab)], from);
        self.padd(Ph::Work, pht);
        if r == -7 {
            r = 0;
        }
        // where the C function jumps: Out (save the console if there is a VM), OutSave, Done
        #[derive(PartialEq)]
        enum End {
            Out,
            OutSave,
            Done,
        }
        let end: End = 'body: {
            if r < 0 && r != -3 {
                verdict = if r == -2 {
                    "TIMEOUT workload".into()
                } else {
                    "ERROR vm workload".into()
                };
                break 'body End::Out;
            }
            let pht = self.pt();
            wfp = self.fingerprint(&b);
            self.padd(Ph::Wfp, pht);
            self.schedstat(&b, tag, false);
            self.dump_state(&b, tag, "workload");
            if sc.cf_on {
                let mut fc = hvx_flushcut::default();
                let _ = b.vm().io(HVX_GET_FLUSHCUT, &mut fc);
                diag!(
                    self,
                    "FLUSHCUT {tag} target={} flushes={} off={} hung={}",
                    fc.target,
                    fc.count,
                    fc.off,
                    fc.hung
                );
            }
            if sc.tj_on {
                let mut got = hvx_tjitter::default();
                let _ = b.vm().io(HVX_GET_TJITTER, &mut got);
                tjinfo.push_str(&format!("-{} tjn={}", got.last_tick, got.applied));
                ccap(&mut tjinfo, 160);
                let mut off = hvx_tjitter::default();
                if let Err(e) = b.vm().io(HVX_SET_TJITTER, &mut off) {
                    diag!(self, "HVX_SET_TJITTER: {e}");
                }
            }
            if sc.xt_on {
                let mut got = hvx_xtick::default();
                let _ = b.vm().io(HVX_GET_XTICK, &mut got);
                tjinfo.push_str(&format!(" xtn={}", got.applied));
                ccap(&mut tjinfo, 160);
                let mut off = hvx_xtick::default();
                if let Err(e) = b.vm().io(HVX_SET_XTICK, &mut off) {
                    diag!(self, "HVX_SET_XTICK: {e}");
                }
            }
            if self.vcpustat_on() {
                self.vcpustat(&b, Some(tag));
            }
            if sc.q_on {
                let mut c = hvx_sched::zeroed_box();
                if b.vm().io(HVX_GET_SCHED, &mut *c).is_ok() {
                    tjinfo.push_str(&format!(" qn={}", c.turns.wrapping_sub(self.q_turns0)));
                    ccap(&mut tjinfo, 160);
                    if sc.tc_on {
                        let mut t = hvx_turnctl::default();
                        if sc.tc.burst_len != 0 && b.vm().io(HVX_GET_TURNCTL, &mut t).is_ok() {
                            tjinfo.push_str(&format!(" qbn={}", t.burst_turns));
                            ccap(&mut tjinfo, 160);
                        }
                        let mut t = hvx_turnctl::default();
                        if let Some(m) = &self.o.turn_margin {
                            t.margin = strtoull0(m.as_bytes());
                        }
                        if let Err(e) = b.vm().io(HVX_SET_TURNCTL, &mut t) {
                            diag!(self, "HVX_SET_TURNCTL (restore): {e}");
                        }
                    }
                    c.quantum = self.q_old_quantum;
                    c.seed = self.q_old_seed;
                    if let Err(e) = b.vm().io(HVX_SET_SCHED, &mut *c) {
                        diag!(self, "HVX_SET_SCHED (restore turns): {e}");
                    }
                } else {
                    diag!(self, "HVX_GET_SCHED: failed");
                }
            }
            if sc.pp != 0 || sc.pptsc != 0 {
                let mut pm = hvx_pmu {
                    period: self.pp_old_period,
                    tick_tsc: self.pp_old_tsc,
                    ..Default::default()
                };
                let _ = b.vm().io(HVX_SET_PMU, &mut pm);
            }
            if r == 1 || r == -3 {
                verdict = if r == -3 && !self.c.endv.is_empty() {
                    self.c.endv.clone()
                } else {
                    Self::last_verdict(&b, from)
                };
                ffp = wfp;
                break 'body End::Out;
            }
            let orc = Self::oracle_cmd(&b, from);
            let pht = self.pt();
            self.save_console(&b, from, tag, "");
            self.padd(Ph::SaveCon, pht);
            if !sc.cut && !self.c.cut {
                let f2 = b.ulen();
                if self.set_fault(&b, 0, 0, 0).is_err()
                    || (!ab && self.put_bytes(&b, b"go\n").is_err())
                {
                    verdict = "ERROR harness go".into();
                    break 'body End::Out;
                }
                let pht = self.pt();
                self.c.phase = "check";
                self.c.start = 1;
                let r = self.run_until_any(&mut b, &fin[..nmk(1, ab)], f2);
                self.padd(Ph::Check, pht);
                if r < 0 && r != -3 {
                    verdict = if r == -2 {
                        "TIMEOUT check".into()
                    } else {
                        "ERROR vm check".into()
                    };
                    break 'body End::OutSave;
                }
                let pht = self.pt();
                ffp = self.fingerprint(&b);
                self.padd(Ph::Ffp, pht);
                self.dump_state(&b, tag, "check");
                verdict = if r == -3 && !self.c.endv.is_empty() {
                    self.c.endv.clone()
                } else {
                    Self::last_verdict(&b, f2)
                };
                self.save_console(&b, f2, tag, "");
                break 'body End::Done;
            }
            // the power cut: only the flushed disk survives; recover in a fresh VM from S0
            if ab && self.ctl_find("S0").is_none() {
                verdict = "ERROR harness no snapshot S0".into();
                break 'body End::Out;
            }
            let pht = self.pt();
            let durable = Pages::store(b.p2());
            self.padd(Ph::Durable, pht);
            if let Some(p) = &self.o.durable_out {
                let _ = std::fs::write(p, b.p2());
            }
            let pht = self.pt();
            self.teardown(&mut b);
            self.padd(Ph::RTear, pht);
            let pht = self.pt();
            let Some(mut ob) = self.vm_fresh() else {
                verdict = "ERROR harness recovery vm".into();
                break 'body End::Out;
            };
            self.padd(Ph::RFresh, pht);
            let pht = self.pt();
            let Some((s0, slot)) = self.take_s0() else {
                verdict = "ERROR harness recovery vm".into();
                self.teardown(&mut ob);
                break 'body End::Out;
            };
            let ok = self.snap_restore(&mut ob, &s0, Some(&durable)).is_ok();
            self.put_s0(s0, slot);
            if !ok {
                // the C engine leaves the half-restored VM aside (b is the torn-down one)
                verdict = "ERROR harness recovery vm".into();
                self.teardown(&mut ob);
                break 'body End::Out;
            }
            self.padd(Ph::RRestore, pht);
            drop(durable);
            b = ob;
            from = b.ulen();
            if self.set_fault(&b, 0, 0, 0).is_err() || (!ab && self.put_bytes(&b, &orc).is_err()) {
                verdict = "ERROR harness oracle cmd".into();
                break 'body End::Out;
            }
            let pht = self.pt();
            if self.c.on {
                self.ctl_drop_pending();
                self.c.phase = "recovery";
                self.c.start = 1;
                self.c.k.evfrom = from;
            }
            let r = self.run_until_any(&mut b, &fin[..nmk(1, ab)], from);
            self.padd(Ph::Recov, pht);
            if r < 0 && r != -3 {
                verdict = if r == -2 {
                    "TIMEOUT recovery".into()
                } else {
                    "ERROR vm recovery".into()
                };
                break 'body End::OutSave;
            }
            let pht = self.pt();
            ffp = self.fingerprint(&b);
            self.padd(Ph::Rfp, pht);
            self.dump_state(&b, tag, "recovery");
            verdict = if r == -3 && !self.c.endv.is_empty() {
                self.c.endv.clone()
            } else {
                Self::last_verdict(&b, from)
            };
            self.save_console(&b, from, tag, ".recovery");
            End::Done
        };
        match end {
            End::OutSave => self.save_console(&b, from, tag, ""),
            End::Out => {
                if b.vm.is_some() {
                    self.save_console(&b, from, tag, "");
                }
            }
            End::Done => {}
        }
        if self.c.on {
            tjinfo.push_str(&format!(
                " stops={} typed={} inh={:016x}",
                self.c.k.nstop, self.c.k.ntyped, self.c.k.inh
            ));
            ccap(&mut tjinfo, 160);
        }
        let mut fpb = Vec::with_capacity(8);
        fpb.extend_from_slice(&ffp.to_le_bytes());
        let fp = fnv(wfp, &fpb);
        self.result_line(tag, fp, wfp, t0, &tjinfo, &verdict);
        if self.c.on {
            self.c.k = Book::default();
            self.c.on = false;
        }
        let pht = self.pt();
        self.teardown(&mut b);
        self.padd(Ph::Tear, pht);
        if self.o.phases {
            self.ph_extra.push_str(&format!(
                " cut={} secs={:.3}",
                if sc.cut { 1 } else { 0 },
                now() - t0
            ));
            self.ph_print(tag);
        }
    }

    /// `run_sched`: arm the schedule's host part, type its guest part, then finish it.
    fn run_sched(&mut self, mut b: Boot, tag: &str, sc: &Sched) {
        let from = b.ulen();
        let t0 = now();
        let mut tjinfo = String::new();
        self.budget = sc.rb;
        if self.c.on && !self.ctl_boot {
            self.c.k.evfrom = from;
        }
        let fail: Option<&str> = 'setup: {
            if sc.q_on && self.o.vcpus < 2 {
                break 'setup Some(
                    "ERROR harness q=/qs=/qb=/qm= need several vCPUs (--vcpus N > 1)",
                );
            }
            if self.set_fault(&b, sc.dseed, sc.classes, sc.prob).is_err() {
                break 'setup Some("ERROR harness set_fault");
            }
            if sc.cf_on {
                let mut fc = hvx_flushcut {
                    on: 1,
                    target: sc.cf,
                    lo: self.e.p2_off,
                    hi: self.e.p2_off + self.e.p2_len,
                    ..Default::default()
                };
                if b.vm().io(HVX_SET_FLUSHCUT, &mut fc).is_err() {
                    break 'setup Some("ERROR harness cf= (HVX_SET_FLUSHCUT)");
                }
            }
            if sc.pp != 0 || sc.pptsc != 0 {
                let mut pm = hvx_pmu::default();
                if b.vm().io(HVX_GET_PMU, &mut pm).is_err() || pm.period == 0 {
                    break 'setup Some("ERROR harness pp= (needs the PMU clock)");
                }
                self.pp_old_period = pm.period;
                self.pp_old_tsc = pm.tick_tsc;
                let mut pm = hvx_pmu {
                    period: sc.pp,
                    tick_tsc: sc.pptsc,
                    ..Default::default()
                };
                if b.vm().io(HVX_SET_PMU, &mut pm).is_err() {
                    break 'setup Some("ERROR harness pp= value");
                }
            }
            if sc.xt_on {
                let mut xt = sc.xt;
                if let Err(e) = b.vm().io(HVX_SET_XTICK, &mut xt) {
                    diag!(self, "HVX_SET_XTICK: {e}");
                    break 'setup Some("ERROR harness extra ticks (needs the PMU clock)");
                }
            }
            if sc.tj_on {
                let mut tj = sc.tj;
                let mut pm = hvx_pmu::default();
                let bad = match b.vm().io(HVX_SET_TJITTER, &mut tj) {
                    Err(e) => {
                        diag!(self, "HVX_SET_TJITTER: {e}");
                        true
                    }
                    Ok(()) => b.vm().io(HVX_GET_PMU, &mut pm).is_err() || pm.period == 0,
                };
                if bad {
                    break 'setup Some("ERROR harness tick jitter (needs the PMU clock)");
                }
                tjinfo = format!(" tjk={}", pm.next / pm.period);
            }
            if sc.q_on {
                let mut c = hvx_sched::zeroed_box();
                if b.vm().io(HVX_GET_SCHED, &mut *c).is_err() {
                    break 'setup Some("ERROR harness q= (HVX_GET_SCHED)");
                }
                self.q_old_quantum = c.quantum;
                self.q_old_seed = c.seed;
                self.q_turns0 = c.turns;
                if sc.q != 0 {
                    c.quantum = sc.q;
                }
                c.seed = sc.qs;
                if b.vm().io(HVX_SET_SCHED, &mut *c).is_err() {
                    break 'setup Some("ERROR harness q= value (HVX_SET_SCHED)");
                }
                let mut tc = sc.tc;
                if tc.margin == 0 {
                    if let Some(m) = &self.o.turn_margin {
                        tc.margin = strtoull0(m.as_bytes());
                    }
                }
                if sc.tc_on && b.vm().io(HVX_SET_TURNCTL, &mut tc).is_err() {
                    break 'setup Some(
                        "ERROR harness qb=/qm= (HVX_SET_TURNCTL; needs a module with it)",
                    );
                }
            }
            self.schedstat(&b, tag, true);
            if self.vcpustat_on() {
                self.vcpustat(&b, None);
            }
            let skip = self.c.on && (sc.guest_len == 0 || self.ctl_boot);
            if !skip {
                let mut line = sc.guest().to_vec();
                line.push(b'\n');
                if self.put_bytes(&b, &line).is_err() {
                    break 'setup Some("ERROR harness put_input");
                }
            }
            None
        };
        if let Some(v) = fail {
            self.sched_fail(&mut b, tag, from, t0, &tjinfo, v);
            if self.c.on {
                self.c.k = Book::default();
                self.c.on = false;
            }
            self.budget = 0;
            return;
        }
        self.padd(Ph::Setup, t0);
        self.finish_sched(b, tag, sc, from, t0, &tjinfo);
        self.budget = 0;
    }

    /// `run_branch`: one base run to the marker, a snapshot there, timing variants from it.
    #[allow(clippy::too_many_arguments)]
    fn run_branch(
        &mut self,
        mut b: Boot,
        tag: &str,
        sc: &Sched,
        mark: &str,
        nvar: i32,
        pm_: u32,
        mx: u64,
    ) {
        let from = b.ulen();
        let t0 = now();
        let mut sub = format!("{tag}-base");
        ccap(&mut sub, 96);
        self.budget = BRANCH_BUDGET;
        let fail: Option<&str> = 'b: {
            if sc.tj_on || sc.rb != 0 || sc.pp != 0 || sc.pptsc != 0 || sc.xt_on || sc.q_on {
                break 'b Some("ERROR branch: not for timed or rb= schedules");
            }
            if self.set_fault(&b, sc.dseed, sc.classes, sc.prob).is_err() {
                break 'b Some("ERROR harness set_fault");
            }
            let mut line = sc.guest().to_vec();
            line.push(b'\n');
            if self.put_bytes(&b, &line).is_err() {
                break 'b Some("ERROR harness put_input");
            }
            let r = if let Some(n) = mark.strip_prefix('@') {
                let mut pm = hvx_pmu::default();
                if b.vm().io(HVX_GET_PMU, &mut pm).is_err() || pm.period == 0 {
                    break 'b Some("ERROR branch needs the PMU clock");
                }
                let target = if mark != "@held" {
                    pm.next / pm.period + strtoull(n.as_bytes(), 10).0
                } else {
                    0
                };
                self.run_until_tick(&mut b, target, from)
            } else {
                let mk: [&[u8]; 3] = [mark.as_bytes(), DONE_MARKER, FINAL_MARKER];
                self.run_until_any(&mut b, &mk, from)
            };
            if r < 0 {
                break 'b Some("ERROR vm workload (before the branch point)");
            }
            if r != 0 {
                self.say(&format!("BRANCHPOINT {tag} none"));
                self.finish_sched(b, &sub, sc, from, t0, "");
                b = Boot::none();
                break 'b None;
            }
            let mut pm = hvx_pmu::default();
            if b.vm().io(HVX_GET_PMU, &mut pm).is_err() || pm.period == 0 {
                break 'b Some("ERROR branch needs the PMU clock");
            }
            let lo = pm.next / pm.period + 1;
            let Some(s2) = self.snap_take(&b) else {
                break 'b Some("ERROR branch snapshot");
            };
            self.say(&format!("BRANCHPOINT {tag} tjlo={lo} held={}", s2.unsync_n));
            let mut nvar = nvar;
            if nvar < 0 {
                self.teardown(&mut b);
                nvar = -nvar;
            } else {
                self.finish_sched(b, &sub, sc, from, t0, "");
            }
            b = Boot::none();
            for v in 1..=nvar {
                let tv = now();
                let mut vsub = format!("{tag}-v{v}");
                ccap(&mut vsub, 96);
                let mut sv = *sc;
                sv.tj = hvx_tjitter::default();
                sv.tj_on = true;
                sv.tj.seed = v as u64;
                sv.tj.permille = pm_;
                sv.tj.max_delay = mx;
                sv.tj.tick_lo = lo;
                let vb = self.vm_fresh();
                let mut ok = None;
                if let Some(mut vb) = vb {
                    let good = self.snap_restore(&mut vb, &s2, None).is_ok() && {
                        let mut tj = sv.tj;
                        b_set_tj(&vb, &mut tj)
                    };
                    if good {
                        ok = Some(vb);
                    } else {
                        self.sched_fail(&mut vb, &vsub, from, tv, "", "ERROR branch restore");
                        continue;
                    }
                }
                match ok {
                    Some(vb) => {
                        let tji = format!(" tjk={}", lo - 1);
                        self.finish_sched(vb, &vsub, &sv, from, tv, &tji);
                    }
                    None => {
                        let mut nb = Boot::none();
                        self.sched_fail(&mut nb, &vsub, from, tv, "", "ERROR branch restore");
                    }
                }
            }
            None
        };
        if let Some(v) = fail {
            self.sched_fail(&mut b, &sub, from, t0, "", v);
        }
        self.budget = 0;
        self.say(&format!("BRANCHEND {tag}"));
    }

    /// `ctl_resume`: a controlled run continued from a SNAP, in a fresh VM.
    fn ctl_resume(&mut self, tag: &str, name: &str) {
        let t0 = now();
        let Some(i) = self.ctl_find(name) else {
            self.say(&format!(
                "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR no snapshot {name}"
            ));
            return;
        };
        let cs = self.snaps[i].take().expect("snapshot");
        let b = self.fresh_restore(&cs.sn);
        let (sc, from, tjinfo, k) = (cs.sc, cs.from, cs.tjinfo.clone(), cs.k.clone());
        self.snaps[i] = Some(cs);
        let Some(b) = b else {
            self.say(&format!(
                "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR harness restore"
            ));
            return;
        };
        self.ctl_begin(tag);
        self.c.k = k;
        self.c.start = 2;
        self.budget = sc.rb;
        self.finish_sched(b, tag, &sc, from, t0, &tjinfo);
        self.budget = 0;
    }

    fn agent_guest_ok(sc: &Sched) -> bool {
        sc.guest_len == 0 || sc.guest() == b"run"
    }

    /// `agent_boot`: an agent image under lock-step control from its first instruction.
    fn agent_boot(&mut self, sc: &Sched, tag: &str) -> (i32, Boot) {
        let mut cmd = format!("{} quiet loglevel=1", self.e.cmdbase);
        ccap(&mut cmd, 1200);
        let Some(mut b) = self.boot_setup(Setup::Boot, &cmd, HVX_FAULT_DROP_UNFLUSHED) else {
            return (-5, Boot::none());
        };
        self.ctl_begin(tag);
        self.c.phase = "boot";
        *self.c.sc = *sc;
        self.c.from = 0;
        self.c.tjinfo = String::new();
        self.budget = sc.rb;
        let r = self.run_until_any(&mut b, &[], 0);
        self.budget = 0;
        (r, b)
    }

    fn agent_boot_result(&mut self, b: &mut Boot, tag: &str, r: i32, t0: f64) {
        let (mut wfp, mut ffp) = (0, 0);
        let verdict = if r == -3 {
            wfp = self.fingerprint(b);
            ffp = wfp;
            if !self.c.endv.is_empty() {
                self.c.endv.clone()
            } else {
                Self::last_verdict(b, 0)
            }
        } else if r == -2 {
            "TIMEOUT boot".into()
        } else if r == -5 {
            "ERROR harness boot vm".into()
        } else {
            "ERROR vm boot".into()
        };
        if r != -5 {
            self.save_console(b, 0, tag, "");
        }
        let fp = fnv(wfp, &ffp.to_le_bytes());
        let l = format!(
            "RESULT {tag} fp={fp:016x} wfp={wfp:016x} secs={:.1} stops={} typed={} inh={:016x} verdict={verdict}",
            now() - t0,
            self.c.k.nstop,
            self.c.k.ntyped,
            self.c.k.inh
        );
        self.say(&l);
        self.c.k = Book::default();
        self.c.on = false;
    }

    /// `boot_to_s1`: boot to S0, snapshot, provision to S1; the VM is left at S1.
    fn boot_to_s1(&mut self, want_s1: bool) -> Option<Boot> {
        let mut cmd = format!("{} hydo_jit={JIT} hydo_phase=serve", self.e.cmdbase);
        ccap(&mut cmd, 512);
        let pt0 = now();
        let mut b = self.boot_setup(Setup::Boot, &cmd, HVX_FAULT_DROP_UNFLUSHED)?;
        let pt1 = now();
        let m0: [&[u8]; 3] = [S0_MARKER, FINAL_MARKER, AGENT_PREFIX];
        let r0 = self.run_until_any(&mut b, &m0, 0);
        if r0 == 2 {
            diag!(self, "hvx_explore: the guest's first process is hydo-agent (an agent image, /etc/hydo/control): boot it with --control boot");
            return None;
        }
        if r0 != 0 {
            diag!(self, "hvx_explore: no S0 marker");
            self.console_dump(&b);
            return None;
        }
        let pt2 = now();
        let s0 = self.snap_take(&b)?;
        let s0_ulen = s0.con.len();
        self.s0 = Some(Box::new(s0));
        let pt3 = now();
        let f = self.fingerprint(&b);
        diag!(
            self,
            "hvx_explore: S0 at {} console bytes, state {f:016x}",
            b.ulen()
        );
        if self.vcpustat_on() {
            let line = match find(&b.ubuf, b"smp: Brought up") {
                Some(at) => {
                    let e = b.ubuf[at..]
                        .iter()
                        .position(|&c| c == b'\n')
                        .map(|n| at + n.min(159))
                        .unwrap_or(b.ulen().min(at + 159));
                    String::from_utf8_lossy(&b.ubuf[at..e]).into_owned()
                }
                None => "no \"smp: Brought up\" line in the console".into(),
            };
            diag!(self, "VCPUSTAT boot: {line}");
        }
        if self.put_bytes(&b, b"provision\n").is_err() {
            return None;
        }
        let pt3b = now();
        let m1: [&[u8]; 2] = [S1_MARKER, FINAL_MARKER];
        if self.run_until_any(&mut b, &m1, s0_ulen) != 0 {
            diag!(self, "hvx_explore: no S1 marker");
            self.console_dump(&b);
            return None;
        }
        let pt4 = now();
        let f = self.fingerprint(&b);
        diag!(
            self,
            "hvx_explore: S1 at {} console bytes, state {f:016x}",
            b.ulen()
        );
        if want_s1 {
            let t5 = now();
            let s1 = self.snap_take(&b)?;
            let t6 = now();
            if self.o.phases {
                diag!(
                    self,
                    "PHBOOT setup={:.3} to_s0={:.3} snap_s0={:.3} fp_s0={:.3} to_s1={:.3} fp_s1={:.3} snap_s1={:.3} s1_ram_nonzero_pages={} s1_p2_nonzero_pages={} con_s0={} con_s1={} engine=rust",
                    pt1 - pt0,
                    pt2 - pt1,
                    pt3 - pt2,
                    pt3b - pt3,
                    pt4 - pt3b,
                    t5 - pt4,
                    t6 - t5,
                    s1.ram.nonzero_blocks(),
                    s1.rw.nonzero_blocks(),
                    s0_ulen,
                    b.ulen()
                );
            }
            self.s1 = Some(Box::new(s1));
        } else if self.o.phases {
            diag!(
                self,
                "PHBOOT setup={:.3} to_s0={:.3} snap_s0={:.3} fp_s0={:.3} to_s1={:.3} engine=rust",
                pt1 - pt0,
                pt2 - pt1,
                pt3 - pt2,
                pt3b - pt3,
                pt4 - pt3b
            );
        }
        Some(b)
    }

    /// `fgets(in, 4096, stdin)`: a request line of at most 4095 bytes (a longer line is read as
    /// several, as the C engine reads it).
    fn fgets(&mut self) -> Option<Vec<u8>> {
        if self.line_buf.is_empty() {
            match self.io.input.read_until(b'\n', &mut self.line_buf) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
        }
        let n = self.line_buf.len().min(4095);
        let rest = self.line_buf.split_off(n);
        let line = std::mem::replace(&mut self.line_buf, rest);
        Some(line)
    }

    /// The serve loop: READY was printed; requests until QUIT or EOF.
    fn serve_loop(&mut self) {
        while let Some(raw) = self.fgets() {
            // a C string ends at its first NUL
            let inp = match raw.iter().position(|&c| c == 0) {
                Some(z) => raw[..z].to_vec(),
                None => raw,
            };
            if inp.starts_with(b"QUIT") {
                break;
            }
            if inp.starts_with(b"BRANCH ") && self.ctl_boot {
                self.say("BRANCHEND ? unsupported for an agent image (it runs under control: CTL, RESUME)");
                continue;
            }
            if inp.starts_with(b"BRANCH ") {
                let mut s = Scan::new(&inp);
                s.lit("BRANCH").ws();
                let tag = s.word(63).map(|w| String::from_utf8_lossy(w).into_owned());
                let nv = s.ws().int();
                let bpm = s.ws().uint();
                let bmx = s.ws().ull();
                let mark = s
                    .ws()
                    .word(127)
                    .map(|w| String::from_utf8_lossy(w).into_owned());
                s.ws();
                let off = if s.ok() { s.pos } else { 0 };
                let sc = if s.n == 5 && off != 0 {
                    Sched::parse(&inp[off..])
                } else {
                    None
                };
                let (Some(sc), Some(tag), Some(nv), Some(bpm), Some(bmx), Some(mark)) =
                    (sc, tag, nv, bpm, bmx, mark)
                else {
                    self.say("BRANCHEND ? bad request");
                    continue;
                };
                let s1 = self.s1.take().expect("S1");
                let b = self.fresh_restore(&s1);
                self.s1 = Some(s1);
                let Some(b) = b else {
                    self.say(&format!("BRANCHEND {tag} restore failed"));
                    continue;
                };
                self.run_branch(b, &tag, &sc, &mark, nv, bpm, bmx);
                continue;
            }
            if inp.starts_with(b"RESUME ") {
                let mut s = Scan::new(&inp);
                s.lit("RESUME").ws();
                let tag = s.word(63).map(|w| String::from_utf8_lossy(w).into_owned());
                let name = s
                    .ws()
                    .word(63)
                    .map(|w| String::from_utf8_lossy(w).into_owned());
                let (Some(tag), Some(name)) = (tag, name) else {
                    self.say("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request");
                    continue;
                };
                if self.o.phases {
                    self.ph_reset();
                }
                self.ctl_resume(&tag, &name);
                continue;
            }
            let ctl = inp.starts_with(b"CTL ");
            let rq = &inp[if ctl { 4 } else { 0 }..];
            let mut s = Scan::new(rq);
            if !ctl {
                s.lit("RUN").ws();
            }
            let tag = s.word(63).map(|w| String::from_utf8_lossy(w).into_owned());
            s.ws();
            let off = if s.ok() { s.pos } else { 0 };
            let sc = match (&tag, off) {
                (Some(_), o) if o != 0 => Sched::parse(&rq[o..]),
                _ => None,
            };
            let (Some(tag), Some(sc)) = (tag, sc) else {
                self.say("RESULT ? fp=0 wfp=0 secs=0 verdict=ERROR bad request");
                continue;
            };
            if ctl {
                self.ctl_begin(&tag);
            }
            if self.ctl_boot {
                let cs = self.ctl_find("S1");
                let why = if !ctl {
                    Some("an agent image runs under control only (CTL, RESUME)")
                } else if cs.is_none() {
                    Some("no snapshot S1 (the boot took none)")
                } else if !Self::agent_guest_ok(&sc) {
                    Some("an agent image's schedule has the guest part \"run\" only")
                } else {
                    None
                };
                if let Some(why) = why {
                    self.say(&format!(
                        "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR {why}"
                    ));
                    self.c.k = Book::default();
                    self.c.on = false;
                    continue;
                }
                if self.o.phases {
                    self.ph_reset();
                }
                let i = cs.expect("S1");
                let snap = self.snaps[i].take().expect("S1");
                let b = self.fresh_restore(&snap.sn);
                let k = snap.k.clone();
                self.snaps[i] = Some(snap);
                let Some(b) = b else {
                    self.say(&format!(
                        "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR harness restore"
                    ));
                    self.c.k = Book::default();
                    self.c.on = false;
                    continue;
                };
                self.c.k = k;
                self.c.start = 2;
                self.run_sched(b, &tag, &sc);
                continue;
            }
            if self.o.phases {
                self.ph_reset();
            }
            let pht = self.pt();
            let fresh = self.vm_fresh();
            self.padd(Ph::Fresh, pht);
            self.ph_rsplit = true;
            let b = match fresh {
                Some(mut b) => {
                    let s1 = self.s1.take().expect("S1");
                    let ok = self.snap_restore(&mut b, &s1, None).is_ok();
                    self.s1 = Some(s1);
                    if ok {
                        Some(b)
                    } else {
                        self.teardown(&mut b);
                        None
                    }
                }
                None => None,
            };
            self.ph_rsplit = false;
            let Some(b) = b else {
                self.say(&format!(
                    "RESULT {tag} fp=0 wfp=0 secs=0 verdict=ERROR harness restore"
                ));
                self.c.on = false;
                continue;
            };
            self.run_sched(b, &tag, &sc);
        }
    }
}

fn b_set_tj(vb: &Boot, tj: &mut hvx_tjitter) -> bool {
    vb.vm().io(HVX_SET_TJITTER, tj).is_ok()
}

fn empty_snap() -> Snap {
    Snap {
        s: hvx_snapshot::zeroed_box(),
        smp: None,
        ram: Pages::default(),
        rw: Pages::default(),
        con: Vec::new(),
        unsync: Vec::new(),
        unsync_n: 0,
    }
}

/// The non-zero 4 KiB blocks of `p` as merged (offset, length) runs.
fn nonzero_runs(p: &[u8]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for (i, blk) in p.chunks(4096).enumerate() {
        if crate::fnv::is_zero(blk) {
            continue;
        }
        let o = i * 4096;
        match runs.last_mut() {
            Some((s, n)) if *s + *n == o => *n += blk.len(),
            _ => runs.push((o, blk.len())),
        }
    }
    runs
}

fn ram_mib_parse(v: &str, what: &str, diag: &mut dyn Write) -> Option<u64> {
    let ok = !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit());
    let n = strtoull(v.as_bytes(), 10).0;
    if !ok || !(EXP_MEM_MIN_MIB..=EXP_MEM_MAX_MIB).contains(&n) {
        let _ = writeln!(
            diag,
            "hvx_explore: {what}={v}: guest RAM must be a whole number of MiB in [{EXP_MEM_MIN_MIB}, {EXP_MEM_MAX_MIB}]"
        );
        return None;
    }
    Some(n)
}

fn vcpus_parse(v: &str, what: &str, diag: &mut dyn Write) -> Option<u32> {
    let ok = !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit());
    let n = strtoull(v.as_bytes(), 10).0;
    if !ok || n < 1 || n > HVX_SCHED_MAX_VCPUS as u64 {
        let _ = writeln!(
            diag,
            "hvx_explore: {what}={v}: the vCPU count must be a whole number in [1, {HVX_SCHED_MAX_VCPUS}]"
        );
        return None;
    }
    Some(n as u32)
}

/// The engine program: hvx_explore's command line (`[--ram-mib N] [--vcpus N] [--cpu v2|v3]
/// [--control [boot]] serve|one <bzImage> <initramfs> <disk.img> <outdir> ["<schedule>"]`)
/// with `o`'s knobs, on the given streams. Returns the exit status.
pub fn main_with(args: &[String], mut o: Options, mut io: Io) -> i32 {
    let t_start = now();
    let mut ram_mib = EXP_MEM_MIB;
    if let Some(v) = o.ram_mib_env.clone() {
        match ram_mib_parse(&v, "HVX_EXPLORE_RAM_MIB", &mut io.diag) {
            Some(n) => ram_mib = n,
            None => return 2,
        }
    }
    if let Some(v) = o.vcpus_env.clone() {
        match vcpus_parse(&v, "HVX_EXPLORE_VCPUS", &mut io.diag) {
            Some(n) => o.vcpus = n,
            None => return 2,
        }
    }
    let mut a: Vec<String> = args.to_vec();
    let (mut ctl_one, mut ctl_boot) = (false, false);
    while a.len() > 2
        && matches!(
            a[1].as_str(),
            "--ram-mib" | "--vcpus" | "--cpu" | "--control"
        )
    {
        if a[1] == "--control" && a[2] == "boot" {
            ctl_boot = true;
            a.drain(1..3);
            continue;
        }
        if a[1] == "--control" {
            ctl_one = true;
            a.remove(1);
            continue;
        }
        match a[1].as_str() {
            "--ram-mib" => match ram_mib_parse(&a[2], "--ram-mib", &mut io.diag) {
                Some(n) => ram_mib = n,
                None => return 2,
            },
            "--cpu" => {
                if a[2] == "v3" {
                    o.cpu_v3 = true;
                } else if a[2] != "v2" {
                    let _ = writeln!(io.diag, "hvx_explore: --cpu {}: expected v2 or v3", a[2]);
                    return 2;
                }
            }
            _ => match vcpus_parse(&a[2], "--vcpus", &mut io.diag) {
                Some(n) => o.vcpus = n,
                None => return 2,
            },
        }
        a.drain(1..3);
    }
    if o.cpu_v3 {
        let _ = writeln!(io.diag, "hvx_explore: guest CPU x86-64-v3 (AVX/AVX2)");
    }
    if a.len() < 6 {
        let _ = writeln!(io.diag, "usage: {} [--ram-mib N] [--vcpus N] [--cpu v2|v3] [--control [boot]] serve|one <bzImage> <initramfs> <explore-disk.img> <outdir> [\"<schedule>\"]", a.first().map(String::as_str).unwrap_or("hydo-hvx"));
        return 2;
    }
    if o.pt {
        let _ = writeln!(io.diag, "hydo-hvx: coverage tracing (HVX_EXPLORE_PT_ELF, HVX_EXPLORE_PT_RANGES) is not in the Rust engine; use hvx_explore");
        return 1;
    }
    let mode = a[1].clone();
    let outdir = PathBuf::from(&a[5]);
    let _ = std::fs::create_dir(&outdir);
    if ram_mib != EXP_MEM_MIB {
        let _ = writeln!(io.diag, "hvx_explore: guest RAM {ram_mib} MiB");
    }
    if o.vcpus > 1 {
        let _ = writeln!(
            io.diag,
            "hvx_explore: {} vCPUs in in-kernel turns (HVX_VM_SMP_SCHED){}",
            o.vcpus,
            if o.park {
                "; vCPUs 1.. parked (halted, IF=0) until the guest can start them"
            } else {
                ""
            }
        );
    }
    let map = |p: &str, diag: &mut dyn Write| match Mapped::open(Path::new(p)) {
        Ok(m) => Some(m),
        Err(e) => {
            let _ = writeln!(diag, "{p}: {e}");
            None
        }
    };
    let (Some(image), Some(initrd), Some(base)) = (
        map(&a[2], &mut io.diag),
        map(&a[3], &mut io.diag),
        map(&a[4], &mut io.diag),
    ) else {
        return 1;
    };
    let (_p1_off, _p1_len, p2_off, p2_len) = match boot::mbr_partitions(base.bytes()) {
        Ok(p) => p,
        Err(m) => {
            let _ = writeln!(io.diag, "{m}");
            return 1;
        }
    };
    let mut cmdbase = boot::ROOTFS_CMDLINE.to_string();
    if let Some(x) = &o.cmdline_extra {
        cmdbase = format!("{cmdbase} {x}");
        ccap(&mut cmdbase, 1024);
    }
    let base_runs = nonzero_runs(base.bytes());
    let disk_len = base.bytes().len() as u64;
    let (reaper, reaper_join) = if o.reaper {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vm>(0);
        let j = std::thread::Builder::new()
            .name("hvx-reaper".into())
            .spawn(move || {
                for vm in rx {
                    drop(vm);
                }
            })
            .ok();
        (j.as_ref().map(|_| tx), j)
    } else {
        (None, None)
    };
    let mut core = Core {
        e: Env {
            mem_size: ram_mib << 20,
            image,
            initrd,
            base,
            disk_len,
            p2_off,
            p2_len,
            cmdbase,
            base_runs,
        },
        outdir,
        io,
        budget: 0,
        c: Ctl::zero(),
        snaps: (0..ctl::MAX_SNAPS).map(|_| None).collect(),
        s0: None,
        s1: None,
        ctl_one,
        ctl_boot,
        q_old_quantum: 0,
        q_old_seed: 0,
        q_turns0: 0,
        pp_old_period: 0,
        pp_old_tsc: 0,
        ss_br0: [0; HVX_SCHED_MAX_VCPUS],
        ss_turns0: [0; HVX_SCHED_MAX_VCPUS],
        vs_br0: [0; HVX_SCHED_MAX_VCPUS],
        vs_t0: [0; HVX_SCHED_MAX_VCPUS],
        con: hvx_console::zeroed_box(),
        pmu_noted: false,
        reaper,
        reaper_join,
        boot_line: None,
        ph: [0.0; 21],
        ph_t0: 0.0,
        ph_calls: 0,
        ph_extra: String::new(),
        ph_rsplit: false,
        line_buf: Vec::new(),
        o,
    };
    if mode == "one" {
        let sc = if a.len() >= 7 {
            Sched::parse(a[6].as_bytes())
        } else {
            None
        };
        let Some(sc) = sc else {
            diag!(core, "hvx_explore: bad schedule");
            return 2;
        };
        if core.ctl_boot {
            if !Core::agent_guest_ok(&sc) {
                diag!(
                    core,
                    "hvx_explore: an agent image's schedule has the guest part \"run\" only"
                );
                return 2;
            }
            core.ctl_one = true;
            let (r, mut b) = core.agent_boot(&sc, "one");
            if r != -4 {
                core.agent_boot_result(&mut b, "one", r, t_start);
                core.teardown(&mut b);
                return 0;
            }
            core.c.boot_s1 = false;
            core.c.phase = "workload";
            if core.o.phases {
                core.ph_reset();
            }
            core.run_sched(b, "one", &sc);
            return 0;
        }
        let Some(b) = core.boot_to_s1(false) else {
            return 1;
        };
        if core.o.bootcon {
            core.save_console(&b, 0, "boot", "");
        }
        diag!(
            core,
            "hvx_explore: provisioned in {:.1} s (fresh boot, no snapshot restore)",
            now() - t_start
        );
        if core.o.phases {
            core.ph_reset();
        }
        if core.ctl_one {
            core.ctl_begin("one");
        }
        core.run_sched(b, "one", &sc);
        return 0;
    }
    if core.ctl_one {
        diag!(
            core,
            "hvx_explore: --control is for `one`; serve takes CTL requests"
        );
        return 2;
    }
    if mode != "serve" {
        diag!(core, "hvx_explore: unknown mode {mode}");
        return 2;
    }
    if core.ctl_boot {
        let bs = a.get(6).map(String::as_str).unwrap_or("|");
        let Some(bsc) = Sched::parse(bs.as_bytes()) else {
            diag!(core, "hvx_explore: bad boot schedule");
            return 2;
        };
        let (r, mut b) = core.agent_boot(&bsc, "boot");
        core.agent_boot_result(&mut b, "boot", r, t_start);
        core.teardown(&mut b);
    } else {
        let Some(mut b) = core.boot_to_s1(true) else {
            return 1;
        };
        let pht = now();
        core.teardown(&mut b);
        if core.o.phases {
            diag!(
                core,
                "PHBOOT teardown={:.3} total={:.3}",
                now() - pht,
                now() - t_start
            );
        }
    }
    let l = format!("READY boot_secs={:.1}", now() - t_start);
    core.say(&l);
    core.serve_loop();
    0
}

/// `main` of the engine program over the process's own streams and environment.
pub fn main_stdio(args: &[String]) -> i32 {
    let io = Io {
        out: Box::new(std::io::LineWriter::new(std::io::stdout())),
        input: Box::new(std::io::BufReader::new(std::io::stdin())),
        diag: Box::new(std::io::stderr()),
    };
    main_with(args, Options::from_env(), io)
}
