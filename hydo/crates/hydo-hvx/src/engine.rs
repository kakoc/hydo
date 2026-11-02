//! The engine: `hv/hvx/hvx_explore.c` (deleted in M137; `git show d9202e7:hv/hvx/hvx_explore.c`)
//! and the parts of `hvx_stack_verify.c` it used, in Rust, function by function, driving
//! `/dev/hvx` from this process. Comments name the C functions and variables they port. The same requests give the same
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
    /// HVX_EXPLORE_PT_ELF: trace this program's text (coverage, `crate::pt`).
    pub pt_elf: Option<String>,
    /// HVX_EXPLORE_PT_RANGES: trace up to two IP ranges instead.
    pub pt_ranges: Option<String>,
    /// HVX_EXPLORE_PTMULTI: the external decoder for PT_RANGES traces.
    pub ptmulti: Option<String>,
    /// HVX_EXPLORE_PT_MIB: the trace buffer (default 256 MiB).
    pub pt_mib: Option<String>,
    /// HVX_EXPLORE_PT_SAVE: keep the raw trace as `<tag>.pt`.
    pub pt_save: bool,
    /// HVX_EXPLORE_DIRTY=1 (with PHASES): pages of RAM and of the data disk that differ from S1.
    pub dirty: bool,
    /// HVX_EXPLORE_ZSKIP=1 (with PHASES): hash RAM both ways at the workload's end and compare.
    pub zskip: bool,
    /// HVX_EXPLORE_RETLOG=1: one `RET` line per run call on stderr.
    pub retlog: bool,
    /// HVX_SIM_DRAIN_DROP=<marker>: fail the first console read that shows the marker, once.
    pub sim_drain_drop: Option<String>,
    /// HYDO_HVX_REAPER=0: tear old VMs down in line, as the C engine does (default: on a thread).
    pub reaper: bool,
    /// HYDO_HVX_CHECK_ZERO=1: verify that a fresh VM's RAM and disk are zero before a restore.
    pub check_zero: bool,
    /// HYDO_HVX_PREPARE=0: make every fresh VM when it is needed (default in serve mode: the
    /// next one is made on a helper thread while a run runs).
    pub prepare: bool,
    /// The host-call port (M138; on by default since M140, HYDO_HVX_HOSTCALL=0 turns it off):
    /// every VM is made with HVX_VM_HOSTCALL, and when the module has the port (its HC ioctls
    /// answer) a controlled boot of an agent image gets `hydo.hc=1` on its kernel command line.
    /// An agent that uses it (hydo-agent 0.3.0 and later) sends its lines through the port, and
    /// the host's lines reach it there; a module without the port or an older agent stays on the
    /// serial line, unchanged.
    pub hostcall: bool,
    /// HYDO_HVX_REUSE=1 (M138): keep a used VM and bring it back to the next run's snapshot in
    /// place (HVX_RESET_VM and the dirty-page log of a module that has them) instead of making
    /// a fresh VM.
    pub reuse: bool,
    /// HYDO_HVX_CHECK_RESET=1: after every in-place restore, compare all of RAM and the disk with
    /// what a fresh VM would hold, and end the run with an error on a difference.
    pub check_reset: bool,
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
            pt_elf: get("HVX_EXPLORE_PT_ELF"),
            pt_ranges: get("HVX_EXPLORE_PT_RANGES"),
            ptmulti: get("HVX_EXPLORE_PTMULTI"),
            pt_mib: get("HVX_EXPLORE_PT_MIB"),
            pt_save: get("HVX_EXPLORE_PT_SAVE").is_some(),
            dirty,
            zskip: get("HVX_EXPLORE_ZSKIP").is_some(),
            retlog: get("HVX_EXPLORE_RETLOG").is_some(),
            sim_drain_drop: get("HVX_SIM_DRAIN_DROP").filter(|s| !s.is_empty()),
            reaper: not_0("HYDO_HVX_REAPER"),
            check_zero: set_not0("HYDO_HVX_CHECK_ZERO"),
            prepare: not_0("HYDO_HVX_PREPARE"),
            hostcall: not_0("HYDO_HVX_HOSTCALL"),
            reuse: set_not0("HYDO_HVX_REUSE"),
            check_reset: set_not0("HYDO_HVX_CHECK_RESET"),
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
    /// M138: the host-call records the guest committed (the agent's lines, when it uses the
    /// port), in order, as `ubuf` holds the console.
    pbuf: Vec<u8>,
    /// M138: the agent sends its lines through the port (its hello came that way), so the
    /// host's lines go there too.
    hc_active: bool,
    /// M138, HYDO_HVX_REUSE: what the VM's RAM and disk were last restored to (None: not
    /// reusable, as a booted VM or one whose restore failed).
    tgt: Option<Tgt>,
    /// M138: the VM comes from the reuse pool; the next restore resets it in place first.
    reused: bool,
}

/// M138: the snapshot a reusable VM was restored to: its RAM pages and its data partition's,
/// so that the next in-place restore knows which pages may differ beside the dirty ones.
struct Tgt {
    ram_id: u64,
    ram_idx: Vec<u32>,
    /// Some(snapshot id) when the data partition is the snapshot's own, None for a carried one.
    rw_id: Option<u64>,
    rw_idx: Vec<u32>,
}

impl Boot {
    fn none() -> Boot {
        Boot {
            vm: None,
            ubuf: Vec::new(),
            drained: 0,
            p2_off: 0,
            p2_len: 0,
            pbuf: Vec::new(),
            hc_active: false,
            tgt: None,
            reused: false,
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
    /// M138: the host-call records so far, whether the agent uses the port, and the port's
    /// buffers (the partial record and the staged bytes the guest has not read).
    pcon: Vec<u8>,
    hc_active: bool,
    hc: Option<(hvx_hc_state, Vec<u8>)>,
    /// M138: a number of its own, so a reused VM knows whether its RAM already holds this
    /// snapshot's pages outside the ones the last run wrote.
    id: u64,
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
    Dirty,
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
    e: std::sync::Arc<Env>,
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
    prep: Option<std::thread::JoinHandle<(Option<Vm>, Vec<String>)>>,
    prep_on: bool,
    boot_line: Option<String>,
    ph: [f64; 21],
    ph_t0: f64,
    ph_calls: u64,
    ph_extra: String,
    ph_rsplit: bool,
    line_buf: Vec<u8>,
    /// Coverage tracing (`crate::pt`), when on.
    pt: Option<PtOn>,
    /// HVX_SIM_DRAIN_DROP has dropped its one read.
    sim_dropped: bool,
    /// M138, HYDO_HVX_REUSE: VMs kept for the next runs, with what they hold.
    spare: Vec<(Vm, Tgt)>,
    /// M138: reuse is on (asked for, one vCPU, no tracing, a page-aligned data partition).
    reuse_on: bool,
    /// M138: the non-zero pages of a fresh VM's RAM (the module's own default tables), which a
    /// fresh VM keeps under a restore's pages.
    fresh_tpl: Option<Pages>,
}

/// `struct ph_ctr`: the counters PHASES reports.
#[derive(Clone, Copy, Default)]
struct PhCtr {
    exits: u64,
    branches: u64,
    ticks: u64,
    vtsc: u64,
    cyc: u64,
    entries: u64,
    steps: u64,
    calls: u64,
    stepcyc: u64,
    freecyc: u64,
    stephw: u64,
    rdtsc: u64,
    dreq: u64,
}

/// MSR_SMI_COUNT (0x34) of CPU 0 through /dev/cpu/0/msr (root and the msr module), else 0.
fn smi_count() -> u64 {
    use std::os::unix::fs::FileExt;
    let mut v = [0u8; 8];
    match std::fs::File::open("/dev/cpu/0/msr") {
        Ok(f) if f.read_exact_at(&mut v, 0x34).is_ok() => u64::from_le_bytes(v),
        _ => 0,
    }
}

/// `ph_rss_kb`: a field of /proc/self/status in kB (-1 when missing).
fn rss_kb(key: &str) -> i64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix(key)
                    .map(|v| crate::cstr::strtol(v.as_bytes()).0)
            })
        })
        .unwrap_or(-1)
}

/// Coverage tracing as set up from the environment (hvx_explore.c's g_pt* globals).
struct PtOn {
    lo: [u64; 2],
    hi: [u64; 2],
    mib: u64,
    elf: Option<crate::pt::Elf>,
    ptmulti: Option<String>,
    save: bool,
    cov: PathBuf,
}

impl Drop for Core {
    fn drop(&mut self) {
        if let Some(h) = self.prep.take() {
            let _ = h.join();
        }
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

    /// `boot_setup`: a VM made here (see [`create_vm`]).
    /// `hc`: offer the host-call port to the guest (M138: `hydo.hc=1` on the command line when
    /// the VM has the port).
    fn boot_setup(
        &mut self,
        mode: Setup,
        cmdline: &str,
        fault_classes: u32,
        hc: bool,
    ) -> Option<Boot> {
        let mut m = Vec::new();
        let vm = create_vm(
            &self.e,
            &self.o,
            mode,
            cmdline,
            fault_classes,
            &self.boot_line,
            &mut m,
            hc,
        );
        self.print_msgs(m);
        Some(self.boot_of(vm?))
    }

    fn print_msgs(&mut self, m: Vec<String>) {
        for l in m {
            if l == PMU_NOTE {
                if self.pmu_noted {
                    continue;
                }
                self.pmu_noted = true;
            }
            diag!(self, "{l}");
        }
    }

    fn boot_of(&self, vm: Vm) -> Boot {
        let mut b = Boot::none();
        b.ubuf.reserve(UBUF_CAP);
        b.p2_off = self.e.p2_off as usize;
        b.p2_len = self.e.p2_len as usize;
        b.vm = Some(vm);
        b
    }

    fn teardown(&mut self, b: &mut Boot) {
        b.ubuf = Vec::new();
        b.pbuf = Vec::new();
        // M138: a VM whose RAM and disk are known apart from its dirty pages goes to the pool
        // (and only while the machine has room for one more VM of this size: an idle VM holds
        // its memory; which VM a run gets never changes its result)
        let need = (self.e.mem_size + self.e.disk_len) >> 20;
        let keep = self.reuse_on
            && self.spare.len() < SPARE_MAX
            && b.tgt.is_some()
            && b.vm.as_ref().is_some_and(|v| v.dl)
            && mem_available_mib().is_none_or(|av| av >= need + 1024);
        if keep {
            if let (Some(vm), Some(t)) = (b.vm.take(), b.tgt.take()) {
                self.spare.push((vm, t));
            }
            return;
        }
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

    /// `vm_fresh`: a VM ready for a restore. With HYDO_HVX_PREPARE on (the default in serve
    /// mode), the VM was made on a helper thread while the previous run ran (a fresh VM is the
    /// same whenever it is made), and the next one is started at once.
    fn vm_fresh(&mut self) -> Option<Boot> {
        let got = self.prep.take().and_then(|h| h.join().ok());
        let (vm, m) = match got {
            Some(x) => x,
            None => {
                let mut m = Vec::new();
                let vm = create_vm(
                    &self.e,
                    &self.o,
                    Setup::Fresh,
                    "console=ttyS0",
                    0,
                    &self.boot_line,
                    &mut m,
                    false,
                );
                (vm, m)
            }
        };
        self.print_msgs(m);
        self.start_prep();
        let b = self.boot_of(vm?);
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

    /// M138: a VM for a restore of snapshot `id`: from the pool when reuse is on (one that holds
    /// that snapshot already, else any once the pool is full), else a fresh one.
    fn vm_for(&mut self, id: u64) -> Option<Boot> {
        if self.reuse_on {
            let pick = self.spare.iter().position(|(_, t)| t.ram_id == id).or(
                if self.spare.len() >= SPARE_MAX {
                    Some(0)
                } else {
                    None
                },
            );
            if let Some(i) = pick {
                let (vm, t) = self.spare.remove(i);
                let mut b = self.boot_of(vm);
                b.tgt = Some(t);
                b.reused = true;
                return Some(b);
            }
        }
        let b = self.vm_fresh()?;
        if self.reuse_on && self.fresh_tpl.is_none() {
            self.fresh_tpl = Some(Pages::store(b.vm().mem()));
        }
        Some(b)
    }

    /// M138: bring a used VM back to the state HVX_CREATE_VM gave it, apart from RAM and the disk
    /// (HVX_RESET_VM), after reading and clearing its dirty log: (RAM bitmap, disk bitmap).
    fn reuse_reset(&mut self, b: &Boot) -> Result<(Vec<u64>, Vec<u64>), ()> {
        let vm = b.vm();
        let mut ram = vec![0u64; (vm.mem().len() / 4096).div_ceil(64)];
        let mut disk = vec![0u64; (vm.disk().len().div_ceil(4096)).div_ceil(64)];
        if let Err(e) = vm.get_dirty(&mut ram, &mut disk, true) {
            diag!(self, "HVX_GET_DIRTY: {e}");
            return Err(());
        }
        if let Err(e) = vm.reset() {
            diag!(self, "HVX_RESET_VM: {e}");
            return Err(());
        }
        // what create_vm sets on a fresh VM (the snapshot then sets all of it again)
        let mut tsc = hvx_tsc {
            tsc: 0,
            step: BOOT_TSC_STEP,
            aux: 0,
            count: 0,
            mode: HVX_TSC_COUNTER,
        };
        let mut sregs = boot::fill_sregs();
        let mut regs = hvx_regs {
            rip: boot::KERNEL_LOAD_ADDR + boot::KERNEL_ENTRY_OFF,
            rsi: boot::BOOT_PARAMS_ADDR,
            rsp: boot::STACK_TOP,
            rflags: 0x2,
            ..Default::default()
        };
        if vm.io(HVX_SET_TSC, &mut tsc).is_err()
            || vm.io(HVX_SET_SREGS, &mut sregs).is_err()
            || vm.io(HVX_SET_REGS, &mut regs).is_err()
        {
            diag!(self, "hydo-hvx: setting up a reset VM failed");
            return Err(());
        }
        Ok((ram, disk))
    }

    /// Start making the next fresh VM on a helper thread (when on, and when the machine has
    /// memory for two more VMs of this size).
    fn start_prep(&mut self) {
        if !self.prep_on || self.prep.is_some() {
            return;
        }
        let need = (self.e.mem_size + self.e.disk_len) >> 20;
        if mem_available_mib().is_some_and(|av| av < 2 * need + 1024) {
            return;
        }
        let (e, o, bl) = (self.e.clone(), self.o.clone(), self.boot_line.clone());
        self.prep = std::thread::Builder::new()
            .name("hvx-prepare".into())
            .spawn(move || {
                let mut m = Vec::new();
                let vm = create_vm(&e, &o, Setup::Fresh, "console=ttyS0", 0, &bl, &mut m, false);
                if let Some(v) = &vm {
                    v.populate(e.p2_off as usize, e.p2_len as usize);
                }
                (vm, m)
            })
            .ok();
    }

    // ---- diagnostics (HVX_EXPLORE_RETLOG, the PHASES counters, DIRTY, ZSKIP) ----

    /// HVX_EXPLORE_RETLOG: where a run call returned.
    fn retlog(&mut self, b: &Boot, reason: u32) {
        let mut pm = hvx_pmu::default();
        let mut ts = hvx_tsc::default();
        let _ = b.vm().io(HVX_GET_PMU, &mut pm);
        let _ = b.vm().io(HVX_GET_TSC, &mut ts);
        let smi = smi_count();
        diag!(
            self,
            "RET reason={reason} ulen={} total={} ticks={} over={} land={:016x} steps={} hops={} tsc={} rdtsc={} smi={smi}",
            b.ulen(),
            pm.total,
            pm.ticks,
            pm.overshoots,
            pm.land_hash,
            pm.steps,
            pm.hops,
            ts.tsc,
            ts.count
        );
    }

    /// `ph_read`: the guest counters PHASES reports (exits, PMU branches and ticks, virtual TSC,
    /// and the module's HVX_GET_FTSTAT times, in microseconds).
    fn ph_read(&self, b: &Boot) -> PhCtr {
        let mut c = PhCtr::default();
        let vm = b.vm();
        let mut s = hvx_snapshot::zeroed_box();
        if vm.io(HVX_GET_SNAPSHOT, &mut *s).is_ok() {
            c.exits = s.icount;
            c.vtsc = s.tsc;
            c.rdtsc = s.rdtsc_count;
            c.dreq = s.fault_req_seq;
        }
        let mut pm = hvx_pmu::default();
        if vm.io(HVX_GET_PMU, &mut pm).is_ok() {
            c.branches = pm.total;
            c.ticks = pm.ticks;
        }
        let mut f = hvx_ftstat::zeroed_box();
        if vm.io(HVX_GET_FTSTAT, &mut *f).is_ok() {
            for v in f.v.iter().take((f.n_vcpus as usize).min(8)) {
                c.cyc = c.cyc.wrapping_add(v.total_cyc);
                c.entries = c
                    .entries
                    .wrapping_add(v.free_entries.wrapping_add(v.step_entries));
                c.steps = c.steps.wrapping_add(v.step_entries);
                c.calls = c.calls.wrapping_add(v.calls);
                c.stepcyc = c.stepcyc.wrapping_add(v.step_cyc);
                c.freecyc = c.freecyc.wrapping_add(v.free_cyc);
                c.stephw = c.stephw.wrapping_add(v.step_hw_cyc);
            }
            if f.tsc_khz != 0 {
                let us = |x: u64| x.wrapping_mul(1000) / f.tsc_khz;
                c.cyc = us(c.cyc);
                c.stepcyc = us(c.stepcyc);
                c.freecyc = us(c.freecyc);
                c.stephw = us(c.stephw);
            }
        }
        c
    }

    /// `ph_delta`: the counters' change between `a` and `z` as PHASES key=value pairs.
    fn ph_delta(&mut self, pre: &str, a: &PhCtr, z: &PhCtr) {
        let d = |x: u64, y: u64| y.wrapping_sub(x);
        let s = format!(
            " {pre}_exits={} {pre}_branches={} {pre}_ticks={} {pre}_vtsc={} {pre}_vmx_s={:.3} {pre}_entries={} {pre}_steps={} {pre}_runcalls={} {pre}_step_s={:.3} {pre}_step_hw_s={:.3} {pre}_free_s={:.3} {pre}_rdtsc={} {pre}_diskreq={}",
            d(a.exits, z.exits),
            d(a.branches, z.branches),
            d(a.ticks, z.ticks),
            d(a.vtsc, z.vtsc),
            d(a.cyc, z.cyc) as f64 / 1e6,
            d(a.entries, z.entries),
            d(a.steps, z.steps),
            d(a.calls, z.calls),
            d(a.stepcyc, z.stepcyc) as f64 / 1e6,
            d(a.stephw, z.stephw) as f64 / 1e6,
            d(a.freecyc, z.freecyc) as f64 / 1e6,
            d(a.rdtsc, z.rdtsc),
            d(a.dreq, z.dreq)
        );
        self.ph_extra.push_str(&s);
    }

    /// `ph_diff` (HVX_EXPLORE_DIRTY): blocks of `cur` that differ from the saved `sv`.
    fn ph_diff(&mut self, pre: &str, sv: &Pages, cur: &[u8]) {
        let t = now();
        let (n, diff, nz, curnz) = sv.diff(cur);
        self.ph_extra.push_str(&format!(
            " {pre}_pages={n} {pre}_dirty={diff} {pre}_nonzero_before={nz} {pre}_nonzero_after={curnz}"
        ));
        self.ph[Ph::Dirty as usize] += now() - t;
    }

    /// `ph_zskip` (HVX_EXPLORE_ZSKIP): hash guest RAM with and without the zero-page shortcut.
    fn ph_zskip(&mut self, b: &Boot) {
        if !self.o.zskip {
            return;
        }
        let t1 = now();
        let h1 = fnv(BASIS, b.vm().mem());
        let t2 = now();
        let h2 = fnv_zskip(BASIS, b.vm().mem());
        let t3 = now();
        self.ph_extra.push_str(&format!(
            " zskip_same={} fnv_ram_s={:.3} fnv_zskip_ram_s={:.3}",
            (h1 == h2) as i32,
            t2 - t1,
            t3 - t2
        ));
        self.ph[Ph::Dirty as usize] += t3 - t1;
    }

    /// The S0 snapshot a recovery restores (the host's under control of an agent image).
    fn s0_ref(&self) -> Option<&Snap> {
        if self.ctl_boot {
            let i = self.ctl_find("S0")?;
            self.snaps[i].as_ref().map(|c| &c.sn)
        } else {
            self.s0.as_deref()
        }
    }

    // ---- coverage tracing (crate::pt) ----

    /// `pt_arm`: trace this (workload) VM from here on.
    fn pt_arm(&mut self, b: &Boot) {
        let Some(p) = &self.pt else { return };
        let mut pc = hvx_pt_config {
            buf_mib: p.mib,
            flags: HVX_PT_USER | HVX_PT_NORETCOMP,
            lo: p.lo,
            hi: p.hi,
            ..Default::default()
        };
        if let Err(e) = b.vm().io(HVX_SET_PT, &mut pc) {
            diag!(self, "HVX_SET_PT: {e}");
        }
    }

    /// `pt_collect`: read this VM's trace and decode it into `<cov>/<tag>.edges` and
    /// `<tag>.stats` (or hand it to the external decoder).
    fn pt_collect(&mut self, b: &Boot, tag: &str) {
        let Some(p) = &self.pt else { return };
        let stats = p.cov.join(format!("{tag}.stats"));
        let mut ps = hvx_pt_status::default();
        if let Err(e) = b.vm().io(HVX_GET_PT, &mut ps) {
            diag!(self, "HVX_GET_PT: {e}");
            return;
        }
        if ps.saved != 0 {
            // HVX_SET_PT failed (ENOMEM on a busy host): this VM has no trace of its own and the
            // module returned the last freed one, which may be another process's: never decode it
            let _ = std::fs::write(&stats, "no_trace=1\n");
            return;
        }
        let mut buf = vec![0u8; ps.bytes as usize];
        if ps.bytes != 0 {
            let mut rq = hvx_pt_read {
                offset: 0,
                len: ps.bytes,
                addr: buf.as_mut_ptr() as u64,
            };
            if let Err(e) = b.vm().io(HVX_READ_PT, &mut rq) {
                diag!(self, "HVX_READ_PT: {e}");
                return;
            }
        }
        let truncated = (ps.bytes != 0 && ps.bytes >= ps.buf_bytes) as i32;
        let raw = p.cov.join(format!("{tag}.pt"));
        if let Some(dec) = p.ptmulti.clone() {
            // whole-system: save the trace and run "<decoder> <trace> <dir> <name>=<elf> ..."
            let _ = std::fs::write(&raw, &buf);
            let dir = p.cov.join(tag);
            let _ = std::fs::create_dir(&dir);
            let (bin, rest) = match dec.find(' ') {
                Some(i) => (&dec[..i], &dec[i..]),
                None => (dec.as_str(), ""),
            };
            let cmd = format!(
                "{bin} {} {}{rest} > {}/ptmulti.log 2>&1",
                raw.display(),
                dir.display(),
                dir.display()
            );
            let t0 = now();
            let rc = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(&cmd)
                .status()
                .map(|s| {
                    use std::os::unix::process::ExitStatusExt;
                    s.into_raw()
                })
                .unwrap_or(-1);
            let _ = std::fs::write(
                &stats,
                format!(
                    "trace_bytes={} truncated={truncated} ptmulti_rc={rc} decode_secs={:.2}\n",
                    ps.bytes,
                    now() - t0
                ),
            );
            if !p.save {
                let _ = std::fs::remove_file(&raw);
            }
            return;
        }
        if p.save {
            let _ = std::fs::write(&raw, &buf);
        }
        let t0 = now();
        let edges = p.cov.join(format!("{tag}.edges"));
        let (n, errs) = match &p.elf {
            Some(elf) => crate::pt::decode(&buf, elf, &edges, &mut self.io.diag),
            // ranges without a decoder program: hvx_explore decoded them against the ELF regions
            // it had, which were none
            None => crate::pt::decode(
                &buf,
                &crate::pt::Elf {
                    regions: Vec::new(),
                    lo: 0,
                    hi: 0,
                },
                &edges,
                &mut self.io.diag,
            ),
        };
        let _ = std::fs::write(
            &stats,
            format!(
                "trace_bytes={} truncated={truncated} edges={n} decode_errors={errs} decode_secs={:.2}\n",
                ps.bytes,
                now() - t0
            ),
        );
    }

    // ---- the console ----

    fn console_fetch(&mut self, b: &Boot) -> Result<(), ()> {
        let mut tries = 0u32;
        loop {
            let mut got = b.vm().get_console(&mut self.con);
            if got.is_ok() && !self.sim_dropped {
                if let Some(m) = &self.o.sim_drain_drop {
                    let n = (self.con.len as usize).min(HVX_CONSOLE_SZ);
                    if find(&self.con.buf[..n], m.as_bytes()).is_some() {
                        self.sim_dropped = true;
                        got = Err(std::io::Error::from_raw_os_error(libc::ENOMEM));
                        diag!(self, "hvx_stack_verify: HVX_SIM_DRAIN_DROP: dropped the read that first shows \"{m}\"");
                    }
                }
            }
            match got {
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

    /// M138: take the committed host-call records into `pbuf`; the first one makes the port
    /// the agent's channel. Err: the port cannot be read.
    fn hc_drain(&mut self, b: &mut Boot) -> Result<(), ()> {
        if !b.vm().hc {
            return Ok(());
        }
        let n0 = b.pbuf.len();
        let mut add = Vec::new();
        if let Err(e) = b.vm().hc_get(&mut add) {
            diag!(self, "HVX_HC_GET: {e}");
            return Err(());
        }
        if b.pbuf.len() + add.len() > UBUF_CAP {
            add.truncate(UBUF_CAP.saturating_sub(b.pbuf.len()));
        }
        b.pbuf.extend_from_slice(&add);
        if b.pbuf.len() > n0 {
            b.hc_active = true;
        }
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
        h = fnv_zskip(h, b.p2());
        // M138: an agent on the host-call port: its records and the port's buffers too (a VM
        // whose agent never used the port has the fingerprint it always had)
        if b.hc_active {
            h = fnv(h, b"hvx-hostcall-v1");
            h = fnv(h, &(b.pbuf.len() as u64).to_le_bytes());
            h = fnv(h, &b.pbuf);
            if let Ok((st, bytes)) = b.vm().hc_state() {
                h = fnv(h, &st.out_len.to_le_bytes());
                h = fnv(h, &st.out_done.to_le_bytes());
                h = fnv(h, &st.in_len.to_le_bytes());
                h = fnv(h, &st.flags.to_le_bytes());
                h = fnv(h, &bytes);
            }
        }
        h
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
        let hc = if b.vm().hc {
            match b.vm().hc_state() {
                Ok(x) => Some(x),
                Err(e) => {
                    diag!(self, "HVX_HC_GET_STATE: {e}");
                    return None;
                }
            }
        } else {
            None
        };
        Some(Snap {
            s,
            smp,
            ram,
            rw,
            con: b.ubuf.clone(),
            unsync,
            unsync_n: io.n,
            pcon: b.pbuf.clone(),
            hc_active: b.hc_active,
            hc,
            id: next_snap_id(),
        })
    }

    /// `snap_restore` into a fresh VM; `rw` replaces the snapshot's data partition. A VM from the
    /// reuse pool (M138) is reset in place first and gets only the pages that may differ.
    fn snap_restore(&mut self, b: &mut Boot, sn: &Snap, rw: Option<&Pages>) -> Result<(), ()> {
        let prev = b.tgt.take();
        let dirty = if std::mem::take(&mut b.reused) {
            Some(self.reuse_reset(b)?)
        } else {
            None
        };
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
        let rwp = rw.unwrap_or(&sn.rw);
        let rw_id = if rw.is_none() { Some(sn.id) } else { None };
        let mut reuse_n = (0u64, 0u64);
        match (&dirty, &prev) {
            (Some((dram, _)), Some(pv)) => {
                let tpl = self.fresh_tpl.as_ref();
                let mem = b.vm.as_mut().expect("a VM").mem_mut();
                reuse_n.0 = restore_ram(mem, sn, tpl, pv, dram);
            }
            _ => sn
                .ram
                .load_into_zero(b.vm.as_mut().expect("a VM").mem_mut()),
        }
        if self.ph_rsplit {
            self.padd(Ph::RRam, pht);
            pht = self.pt();
        }
        match (&dirty, &prev) {
            (Some((_, ddisk)), Some(pv)) => {
                let e = self.e.clone();
                let (p2o, p2l) = (b.p2_off, b.p2_len);
                let disk = b.vm.as_mut().expect("a VM").disk_mut();
                reuse_n.1 = restore_disk(disk, e.base.bytes(), p2o, p2l, rwp, rw_id, pv, ddisk);
            }
            _ => rwp.load_into_zero(b.p2_mut()),
        }
        if dirty.is_some() {
            if self.o.phases {
                self.ph_extra.push_str(&format!(
                    " reuse_ram={} reuse_disk={}",
                    reuse_n.0, reuse_n.1
                ));
            }
            if self.o.check_reset {
                let tpl = self.fresh_tpl.as_ref();
                if let Some(m) = check_restored(b.vm(), sn, tpl, self.e.base.bytes(), b.p2_off, rwp)
                {
                    diag!(self, "hydo-hvx: HYDO_HVX_CHECK_RESET: {m}");
                    return Err(());
                }
            }
        }
        if self.ph_rsplit {
            self.padd(Ph::RDisk, pht);
            pht = self.pt();
        }
        b.ubuf.clear();
        b.ubuf.extend_from_slice(&sn.con);
        self.console_rebase(b)?;
        b.pbuf.clear();
        b.pbuf.extend_from_slice(&sn.pcon);
        b.hc_active = sn.hc_active;
        if b.vm().hc {
            let (st, bytes) = match &sn.hc {
                Some((st, bytes)) => (*st, bytes.as_slice()),
                None => (hvx_hc_state::default(), &[][..]),
            };
            if let Err(e) = b.vm().hc_set_state(&st, bytes) {
                diag!(self, "HVX_HC_SET_STATE: {e}");
                return Err(());
            }
        }
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
        if self.reuse_on && b.vm().dl {
            b.tgt = Some(Tgt {
                ram_id: sn.id,
                ram_idx: sn.ram.idx().to_vec(),
                rw_id,
                rw_idx: rwp.idx().to_vec(),
            });
        }
        Ok(())
    }

    fn fresh_restore(&mut self, sn: &Snap) -> Option<Boot> {
        let mut b = self.vm_for(sn.id)?;
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
        if b.hc_active {
            // M138: the bytes go to the port, and one byte on the serial input wakes the agent;
            // a retype after a restore only rings again (the snapshot kept the staged bytes)
            if !retype {
                if let Err(e) = b.vm().hc_put(&buf) {
                    diag!(self, "HVX_HC_PUT: {e}");
                }
            }
            let _ = self.put_bytes(b, &[HC_DOORBELL]);
        } else {
            let _ = self.put_bytes(b, &buf);
        }
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
        // M138: the host-call records (one agent line each), after the console's lines
        let mut p2 = self.c.k.pevfrom;
        for q in self.c.k.pevfrom..b.pbuf.len() {
            if b.pbuf[q] != b'\n' {
                continue;
            }
            let mut e = q;
            if e > p2 && b.pbuf[e - 1] == b'\r' {
                e -= 1;
            }
            self.c.k.nev += 1;
            let mut l = format!("EVENT {} at={} n={} ", self.c.tag, self.c.at, self.c.k.nev);
            ctl::esc(&b.pbuf[p2..e], &mut l);
            lines.push(l);
            p2 = q + 1;
        }
        for l in lines {
            self.say(&l);
        }
        self.c.k.evfrom = p;
        self.c.k.pevfrom = p2;
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
            // M138: a host-call record ended the run call: take it, stop, and go on
            let hostcall = reason == HVX_EXIT_HOSTCALL;
            if hostcall && self.hc_drain(b).is_err() {
                return -1;
            }
            let reason = if hostcall { HVX_EXIT_THROTTLE } else { reason };
            self.padd(Ph::Drain, pht);
            if self.o.retlog {
                self.retlog(b, reason);
            }
            let hit = first_hit(b, &mut scanned);
            if self.c.on
                && (hit >= 0
                    || hostcall
                    || !self.c.capped
                    || reason != HVX_EXIT_THROTTLE
                    || self.ctl_due())
            {
                let why = if hit >= 0 {
                    "marker"
                } else if hostcall {
                    "hostcall"
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
        let phc0 = self.ph_calls;
        let pc0 = if self.o.phases {
            self.ph_read(&b)
        } else {
            PhCtr::default()
        };
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
            if self.o.phases {
                let pc1 = self.ph_read(&b);
                self.ph_delta("w", &pc0, &pc1);
                self.ph_extra.push_str(&format!(
                    " w_runcalls_user={} rss_kb={} rss_shmem_kb={} con_bytes={}",
                    self.ph_calls - phc0,
                    rss_kb("VmRSS:"),
                    rss_kb("RssShmem:"),
                    b.ulen() - from
                ));
                if self.o.dirty && !ab {
                    if let Some(s1) = self.s1.take() {
                        self.ph_diff("w_ram", &s1.ram, b.vm().mem());
                        self.ph_diff("w_disk", &s1.rw, b.p2());
                        self.s1 = Some(s1);
                    }
                }
                self.ph_zskip(&b);
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
                let pc1 = if self.o.phases {
                    self.ph_read(&b)
                } else {
                    PhCtr::default()
                };
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
                if self.o.phases {
                    let pc2 = self.ph_read(&b);
                    self.ph_delta("c", &pc1, &pc2);
                    if self.o.dirty && !ab {
                        if let Some(s1) = self.s1.take() {
                            self.ph_diff("c_ram", &s1.ram, b.vm().mem());
                            self.ph_diff("c_disk", &s1.rw, b.p2());
                            self.s1 = Some(s1);
                        }
                    }
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
                self.pt_collect(&b, tag);
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
            self.pt_collect(&b, tag);
            let pht = self.pt();
            self.teardown(&mut b);
            self.padd(Ph::RTear, pht);
            let pht = self.pt();
            let Some((s0, slot)) = self.take_s0() else {
                verdict = "ERROR harness recovery vm".into();
                break 'body End::Out;
            };
            let Some(mut ob) = self.vm_for(s0.id) else {
                self.put_s0(s0, slot);
                verdict = "ERROR harness recovery vm".into();
                break 'body End::Out;
            };
            self.padd(Ph::RFresh, pht);
            let pht = self.pt();
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
            let pc1 = if self.o.phases {
                self.ph_read(&b)
            } else {
                PhCtr::default()
            };
            let pht = self.pt();
            if self.c.on {
                self.ctl_drop_pending();
                self.c.phase = "recovery";
                self.c.start = 1;
                self.c.k.evfrom = from;
                // M138: the recovery VM's host-call records start at the end of S0's
                self.c.k.pevfrom = b.pbuf.len();
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
            if self.o.phases {
                let pc2 = self.ph_read(&b);
                self.ph_delta("rc", &pc1, &pc2);
                if self.o.dirty {
                    let ram = self.s0_ref().map(|s| s.ram.clone());
                    if let Some(r) = ram {
                        self.ph_diff("rc_ram", &r, b.vm().mem());
                    }
                }
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
            self.c.k.pevfrom = b.pbuf.len();
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
            self.pt_arm(&b);
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
                let vb = self.vm_for(s2.id);
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
        let hc = self.o.hostcall;
        let Some(mut b) = self.boot_setup(Setup::Boot, &cmd, HVX_FAULT_DROP_UNFLUSHED, hc) else {
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
        let mut b = self.boot_setup(Setup::Boot, &cmd, HVX_FAULT_DROP_UNFLUSHED, false)?;
        let pt1 = now();
        let ph = self.o.phases;
        let bc0 = if ph {
            self.ph_read(&b)
        } else {
            PhCtr::default()
        };
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
        let bc1 = if ph {
            self.ph_read(&b)
        } else {
            PhCtr::default()
        };
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
        let bc2 = if ph {
            self.ph_read(&b)
        } else {
            PhCtr::default()
        };
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
                let keep = std::mem::take(&mut self.ph_extra);
                self.ph_delta("s0", &bc0, &bc1);
                self.ph_delta("s1", &bc1, &bc2);
                let extra = std::mem::replace(&mut self.ph_extra, keep);
                diag!(
                    self,
                    "PHBOOT setup={:.3} to_s0={:.3} snap_s0={:.3} fp_s0={:.3} to_s1={:.3} fp_s1={:.3} snap_s1={:.3} mem={} disk={} p2={} s1_ram_nonzero_pages={} s1_p2_nonzero_pages={} con_s0={} con_s1={} rss_kb={} rss_shmem_kb={}{extra} engine=rust",
                    pt1 - pt0,
                    pt2 - pt1,
                    pt3 - pt2,
                    pt3b - pt3,
                    pt4 - pt3b,
                    t5 - pt4,
                    t6 - t5,
                    self.e.mem_size,
                    self.e.disk_len,
                    self.e.p2_len,
                    s1.ram.nonzero_blocks(),
                    s1.rw.nonzero_blocks(),
                    s0_ulen,
                    b.ulen(),
                    rss_kb("VmRSS:"),
                    rss_kb("RssShmem:")
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
            let s1id = self.s1.as_ref().map_or(0, |s| s.id);
            let fresh = self.vm_for(s1id);
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

/// M138: at most this many VMs wait in the reuse pool (a power-cut run uses two: the workload's,
/// restored from S1, and the recovery's, from S0).
const SPARE_MAX: usize = 2;

/// Set bit `i` of a page bitmap.
fn bit_set(m: &mut [u64], i: usize) {
    if let Some(w) = m.get_mut(i / 64) {
        *w |= 1 << (i % 64);
    }
}

/// The set bits of a page bitmap, below `n`.
fn bits(m: &[u64], n: usize) -> impl Iterator<Item = usize> + '_ {
    m.iter().enumerate().flat_map(move |(k, &w)| {
        (0..64)
            .filter(move |b| w & (1 << b) != 0)
            .map(move |b| k * 64 + b)
            .filter(move |&i| i < n)
    })
}

/// M138: an in-place restore of RAM: every page the last run wrote (`dirty`), and, when the
/// VM held another snapshot, every page either snapshot has, rewritten as a fresh VM restored
/// from `sn` holds it. Returns the pages written.
fn restore_ram(mem: &mut [u8], sn: &Snap, tpl: Option<&Pages>, pv: &Tgt, dirty: &[u64]) -> u64 {
    let n = mem.len().div_ceil(4096);
    let mut set = dirty.to_vec();
    if pv.ram_id != sn.id {
        for &i in pv.ram_idx.iter().chain(sn.ram.idx()) {
            bit_set(&mut set, i as usize);
        }
    }
    let mut k = 0;
    for i in bits(&set, n) {
        sn.ram.put_block(tpl, i as u32, mem);
        k += 1;
    }
    k
}

/// M138: an in-place restore of the disk: every page the device wrote, and, when the data
/// partition was another one, every page either has, as a fresh VM holds it: the base image
/// outside the data partition, `rw` (else zeros) inside it. Returns the pages written.
#[allow(clippy::too_many_arguments)]
fn restore_disk(
    disk: &mut [u8],
    base: &[u8],
    p2_off: usize,
    p2_len: usize,
    rw: &Pages,
    rw_id: Option<u64>,
    pv: &Tgt,
    dirty: &[u64],
) -> u64 {
    let n = disk.len().div_ceil(4096);
    let p2b = p2_off / 4096;
    let mut set = dirty.to_vec();
    if rw_id.is_none() || pv.rw_id != rw_id {
        for &i in pv.rw_idx.iter().chain(rw.idx()) {
            bit_set(&mut set, p2b + i as usize);
        }
    }
    let mut k = 0;
    for q in bits(&set, n) {
        let at = q * 4096;
        if at >= p2_off && at < p2_off + p2_len {
            rw.put_block(None, (q - p2b) as u32, &mut disk[p2_off..p2_off + p2_len]);
        } else {
            let end = (at + 4096).min(disk.len());
            let d = &mut disk[at..end];
            let have = base.len().saturating_sub(at).min(d.len());
            d[..have].copy_from_slice(&base[at..at + have]);
            d[have..].fill(0);
        }
        k += 1;
    }
    k
}

/// M138, HYDO_HVX_CHECK_RESET: compare every page of a VM restored in place with what a fresh VM
/// restored from `sn` (and `rw`) holds; the first difference, if any.
fn check_restored(
    vm: &Vm,
    sn: &Snap,
    tpl: Option<&Pages>,
    base: &[u8],
    p2_off: usize,
    rw: &Pages,
) -> Option<String> {
    let mem = vm.mem();
    let mut want = vec![0u8; 4096];
    for i in 0..mem.len().div_ceil(4096) {
        let at = i * 4096;
        let got = &mem[at..(at + 4096).min(mem.len())];
        let w = &mut want[..got.len()];
        match sn
            .ram
            .block(i as u32)
            .or_else(|| tpl.and_then(|t| t.block(i as u32)))
        {
            Some(src) => w.copy_from_slice(src),
            None => w.fill(0),
        }
        if got != w {
            return Some(format!("RAM page {i} differs from a fresh VM's"));
        }
    }
    let disk = vm.disk();
    let p2_end = p2_off + rw.len();
    for q in 0..disk.len().div_ceil(4096) {
        let at = q * 4096;
        let got = &disk[at..(at + 4096).min(disk.len())];
        let w = &mut want[..got.len()];
        if at >= p2_off && at < p2_end {
            match rw.block(((at - p2_off) / 4096) as u32) {
                Some(src) => w.copy_from_slice(src),
                None => w.fill(0),
            }
        } else {
            let have = base.len().saturating_sub(at).min(w.len());
            w[..have].copy_from_slice(&base[at..at + have]);
            w[have..].fill(0);
        }
        if got != w {
            return Some(format!("disk page {q} differs from a fresh VM's"));
        }
    }
    None
}

/// M138: the kernel command line's offer of the host-call port to the agent.
pub const HC_TOKEN: &str = "hydo.hc=1";
/// M138: the byte typed on the serial input to wake an agent that reads its lines from the port.
const HC_DOORBELL: u8 = 0x07;

const PMU_NOTE: &str = "hvx_stack_verify: no usable PMU on this host; using the exit-count clock";

/// `boot_setup`'s VM: the base disk attached; `Setup::Boot` also loads the kernel (a real
/// boot), `Setup::Fresh` leaves RAM and the data partition zero for a restore (the C engine
/// loads a kernel there too, which the restore overwrites; `boot_line` is the line it prints
/// for it). Messages go to `m` (printed by the caller, so a VM can be made on another thread).
#[allow(clippy::too_many_arguments)]
fn create_vm(
    e: &Env,
    o: &Options,
    mode: Setup,
    cmdline: &str,
    fault_classes: u32,
    boot_line: &Option<String>,
    m: &mut Vec<String>,
    hc_token: bool,
) -> Option<Vm> {
    macro_rules! say {
        ($($a:tt)*) => { m.push(format!($($a)*)) };
    }
    let (p2_off, p2_len) = (e.p2_off as usize, e.p2_len as usize);
    let mut vm = match Vm::open(&o.dev) {
        Ok(v) => v,
        Err(e) => {
            say!("open /dev/hvx: {e}");
            return None;
        }
    };
    let vcpus = o.vcpus;
    let mut cfg = hvx_config {
        mem_size: e.mem_size,
        n_vcpus: 0,
        flags: HVX_VM_X86_64_V2 | HVX_VM_IDLE_HEARTBEAT,
    };
    if o.pause_exit {
        cfg.flags |= HVX_VM_PAUSE_EXIT;
    }
    if o.pmu_clock {
        cfg.flags |= HVX_VM_PMU_CLOCK;
    }
    if o.cpu_v3 {
        cfg.flags |= HVX_VM_X86_64_V3;
    }
    if o.pt_trace_vm {
        cfg.flags |= HVX_VM_PT_TRACE;
    }
    if o.hostcall {
        cfg.flags |= HVX_VM_HOSTCALL;
    }
    if o.reuse && vcpus == 1 && !o.pt_trace_vm {
        cfg.flags |= HVX_VM_DIRTY_LOG;
    }
    if vcpus > 1 {
        cfg.n_vcpus = vcpus;
        cfg.flags |= HVX_VM_SMP_SCHED;
        if !o.park {
            cfg.flags |= HVX_VM_SMP_BOOT;
        }
        if cfg.flags & HVX_VM_PMU_CLOCK == 0 {
            say!("hvx_stack_verify: {vcpus} vCPUs need the PMU clock (HVX_PMU_CLOCK=0 given)");
            return None;
        }
    }
    // M138: a CPU without EPT dirty flags refuses the log (ENODEV): the VM is then made
    // without it (and is never reused); every other error is the plain path's below
    let mut made = false;
    if cfg.flags & HVX_VM_DIRTY_LOG != 0 {
        match vm.create(&cfg) {
            Ok(()) => made = true,
            Err(_) => cfg.flags &= !HVX_VM_DIRTY_LOG,
        }
    }
    let first = if made { Ok(()) } else { vm.create(&cfg) };
    if let Err(e) = first {
        if vcpus > 1 && e.raw_os_error() == Some(libc::ENODEV) {
            say!("hvx_stack_verify: {vcpus} vCPUs need the PMU clock; this host has no usable PMU");
            return None;
        }
        if e.raw_os_error() != Some(libc::ENODEV) || cfg.flags & HVX_VM_PMU_CLOCK == 0 {
            say!("HVX_CREATE_VM: {e}");
            return None;
        }
        say!("{PMU_NOTE}");
        cfg.flags &= !HVX_VM_PMU_CLOCK;
        if let Err(e) = vm.create(&cfg) {
            say!("HVX_CREATE_VM: {e}");
            return None;
        }
    }
    // M138: the main module ignores flags it does not know; its HC and dirty ioctls fail
    if cfg.flags & HVX_VM_HOSTCALL != 0 {
        vm.hc = vm.hc_get(&mut Vec::new()).is_ok();
    }
    if cfg.flags & HVX_VM_DIRTY_LOG != 0 {
        vm.dl = vm.get_dirty(&mut [], &mut [], false).is_ok();
    }
    if let Err(e) = vm.map_ram(e.mem_size as usize) {
        say!("mmap RAM: {e}");
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
        say!("HVX_SET_TSC: {e}");
        return None;
    }
    let mut dk = hvx_disk {
        len: e.disk_len,
        data: 0,
        flags: HVX_DISK_NO_IDMAP,
    };
    if let Err(e) = vm.io(HVX_SET_DISK, &mut dk) {
        say!("HVX_SET_DISK: {e}");
        return None;
    }
    if let Err(e) = vm.map_disk(e.disk_len as usize) {
        say!("mmap disk window: {e}");
        return None;
    }
    // the image's non-zero blocks (the page array is zero); a fresh VM leaves the data
    // partition zero, the restore writes it
    {
        let base = e.base.bytes();
        let (lo, hi) = (p2_off, p2_off + p2_len);
        let disk = vm.disk_mut();
        for &(o, n) in &e.base_runs {
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
            say!("HVX_SET_FAULT: {e}");
            return None;
        }
    }
    let ncpus = if vcpus > 1 && !o.park { vcpus } else { 1 };
    if mode == Setup::Boot {
        let mut w = Vec::new();
        boot::build_page_tables_and_gdt(vm.mem_mut(), &mut w);
        let image = e.image.bytes();
        let initrd = e.initrd.bytes();
        let hc_cmd;
        let cmdline = if hc_token && vm.hc {
            hc_cmd = format!("{cmdline} {HC_TOKEN}");
            hc_cmd.as_str()
        } else {
            cmdline
        };
        match boot::load_bzimage_n(vm.mem_mut(), image, initrd, cmdline, ncpus, &mut w) {
            Ok(l) => say!("{l}"),
            Err(m) => {
                say!("{m}");
                return None;
            }
        }
    } else {
        // the C engine loads a kernel into every fresh VM too (overwritten by the restore);
        // here RAM stays zero and only its line is printed, for the same log
        if let Some(l) = boot_line {
            say!("{l}");
        }
    }
    let mut sregs = boot::fill_sregs();
    if let Err(e) = vm.io(HVX_SET_SREGS, &mut sregs) {
        say!("HVX_SET_SREGS: {e}");
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
        say!("HVX_SET_REGS: {e}");
        return None;
    }
    if vcpus > 1 && o.park && park_aps(o, &vm, m).is_err() {
        return None;
    }
    if vcpus > 1 && !o.park {
        let mut c = hvx_sched::zeroed_box();
        if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *c).is_err() {
            say!("HVX_GET_SCHED: failed");
            return None;
        }
        c.pause_k = match &o.pause_k {
            Some(p) => strtoull0(p.as_bytes()) as u32,
            None => 4,
        };
        if let Err(e) = vm.io(HVX_SET_SCHED, &mut *c) {
            say!("HVX_SET_SCHED: {e}");
            return None;
        }
    }
    if vcpus > 1 && !o.quantum_after && turn_knobs(o, &vm, m).is_err() {
        return None;
    }
    Some(vm)
}

fn park_aps(o: &Options, vm: &Vm, m: &mut Vec<String>) -> Result<(), ()> {
    macro_rules! say {
        ($($a:tt)*) => { m.push(format!($($a)*)) };
    }
    for i in 1..o.vcpus {
        let mut r = hvx_regs {
            rflags: 0x2,
            ..Default::default()
        };
        if vm.set_vcpu(i).is_err() || vm.io(HVX_SET_REGS, &mut r).is_err() {
            say!("park: HVX_SET_REGS: failed");
            return Err(());
        }
    }
    let mut c = hvx_sched::zeroed_box();
    if vm.set_vcpu(0).is_err() || vm.io(HVX_GET_SCHED, &mut *c).is_err() {
        say!("park: HVX_GET_SCHED: failed");
        return Err(());
    }
    for i in 1..o.vcpus as usize {
        c.v[i].halted = 1;
        c.v[i].halt_len = 1;
    }
    if vm.io(HVX_SET_SCHED, &mut *c).is_err() {
        say!("park: HVX_SET_SCHED: failed");
        return Err(());
    }
    Ok(())
}

/// `turn_knobs` (hvx_stack_verify.c): HVX_QUANTUM / HVX_TURN_MARGIN / HVX_BURST at boot setup.
fn turn_knobs(o: &Options, vm: &Vm, m: &mut Vec<String>) -> Result<(), ()> {
    macro_rules! say {
        ($($a:tt)*) => { m.push(format!($($a)*)) };
    }
    let (q, m, bu) = (o.quantum.clone(), o.turn_margin.clone(), o.burst.clone());
    if o.vcpus < 2 || (q.is_none() && m.is_none() && bu.is_none()) {
        return Ok(());
    }
    if o.ftstat {
        let _ = vm.reset_ftstat();
    }
    if let Some(q) = &q {
        let mut c = hvx_sched::zeroed_box();
        if vm.io(HVX_GET_SCHED, &mut *c).is_err() {
            say!("HVX_GET_SCHED (HVX_QUANTUM): failed");
            return Err(());
        }
        c.quantum = strtoull0(q.as_bytes());
        if vm.io(HVX_SET_SCHED, &mut *c).is_err() {
            say!("HVX_SET_SCHED (HVX_QUANTUM): failed");
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
                say!("bad HVX_BURST");
                return Err(());
            }
            t.burst_len = bl.unwrap_or(0);
            t.burst_q = bq.unwrap_or(0);
            t.burst_permille = pm.unwrap_or(0);
            t.burst_seed = sd.unwrap_or(1);
        }
        if vm.io(HVX_SET_TURNCTL, &mut t).is_err() {
            say!("HVX_SET_TURNCTL: failed");
            return Err(());
        }
    }
    let f = |k: &str, v: &Option<String>| match v {
        Some(v) => format!(" {k}={v}"),
        None => String::new(),
    };
    say!(
        "hvx_stack_verify: turn knobs applied:{}{}{}",
        f("quantum", &q),
        f("margin", &m),
        f("burst", &bu)
    );
    Ok(())
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
        pcon: Vec::new(),
        hc_active: false,
        hc: None,
        id: next_snap_id(),
    }
}

/// M138: a new snapshot number.
fn next_snap_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// `MemAvailable` in MiB.
/// hvx_explore.c's coverage set-up in main: HVX_EXPLORE_PT_RANGES (with HVX_EXPLORE_PTMULTI) or
/// HVX_EXPLORE_PT_ELF; the VMs are then created with HVX_VM_PT_TRACE and `<outdir>/../cov` holds
/// the edge and stats files. Err: a bad range or an unreadable program (exit status 1).
fn pt_setup(o: &mut Options, outdir: &Path, diag: &mut dyn Write) -> Result<Option<PtOn>, ()> {
    if o.pt_ranges.is_none() && o.pt_elf.is_none() {
        return Ok(None);
    }
    let mib = o
        .pt_mib
        .as_deref()
        .map(|m| strtoull0(m.as_bytes()))
        .unwrap_or(256);
    let cov = outdir.join("..").join("cov");
    let mut p = PtOn {
        lo: [0; 2],
        hi: [0; 2],
        mib,
        elf: None,
        ptmulti: None,
        save: o.pt_save,
        cov: cov.clone(),
    };
    if let Some(r) = &o.pt_ranges {
        let Some(v) = crate::pt::parse_ranges(r) else {
            let _ = writeln!(diag, "hvx_explore: bad HVX_EXPLORE_PT_RANGES");
            return Err(());
        };
        for (i, (lo, hi)) in v.iter().enumerate() {
            p.lo[i] = *lo;
            p.hi[i] = *hi;
        }
        p.ptmulti = o.ptmulti.clone();
        let _ = std::fs::create_dir(&cov);
        let _ = writeln!(
            diag,
            "hvx_explore: whole-system coverage on: {} range(s), {} MiB buffer, decoder {}",
            v.len(),
            mib,
            p.ptmulti
                .as_deref()
                .unwrap_or("(none: edges via ptedges.h)")
        );
    } else if let Some(e) = &o.pt_elf {
        let elf = std::fs::read(e).ok().and_then(|b| crate::pt::load_elf(&b));
        let Some(elf) = elf else {
            let _ = writeln!(diag, "hvx_explore: cannot load {e}");
            return Err(());
        };
        p.lo[0] = elf.lo;
        p.hi[0] = elf.hi;
        let _ = std::fs::create_dir(&cov);
        let _ = writeln!(
            diag,
            "hvx_explore: coverage on: text [{:#x}, {:#x}), {} MiB buffer",
            elf.lo, elf.hi, mib
        );
        if !crate::pt::DECODER {
            let _ = writeln!(
                diag,
                "hydo-hvx: built without the cargo feature pt (libipt): traces are taken, not decoded"
            );
        }
        p.elf = Some(elf);
    }
    o.pt_trace_vm = true;
    Ok(Some(p))
}

fn mem_available_mib() -> Option<u64> {
    let t = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = t
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some(kb / 1024)
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
    let mode = a[1].clone();
    let outdir = PathBuf::from(&a[5]);
    let _ = std::fs::create_dir(&outdir);
    let pt = match pt_setup(&mut o, &outdir, &mut io.diag) {
        Ok(p) => p,
        Err(()) => return 1,
    };
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
    let boot_line = {
        let ncpus = if o.vcpus > 1 && !o.park { o.vcpus } else { 1 };
        let mut scratch = vec![0u8; (ram_mib << 20) as usize];
        let mut w = Vec::new();
        boot::load_bzimage_n(
            &mut scratch,
            image.bytes(),
            initrd.bytes(),
            "console=ttyS0",
            ncpus,
            &mut w,
        )
        .ok()
    };
    let mut core = Core {
        e: std::sync::Arc::new(Env {
            mem_size: ram_mib << 20,
            image,
            initrd,
            base,
            disk_len,
            p2_off,
            p2_len,
            cmdbase,
            base_runs,
        }),
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
        boot_line,
        prep: None,
        prep_on: false,
        ph: [0.0; 21],
        ph_t0: 0.0,
        ph_calls: 0,
        ph_extra: String::new(),
        ph_rsplit: false,
        line_buf: Vec::new(),
        pt,
        sim_dropped: false,
        spare: Vec::new(),
        reuse_on: false,
        fresh_tpl: None,
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
    core.prep_on = core.o.prepare;
    core.reuse_on = core.o.reuse
        && core.o.vcpus == 1
        && core.pt.is_none()
        && !core.o.pt_trace_vm
        && core.e.p2_off.is_multiple_of(4096)
        && core.e.p2_len.is_multiple_of(4096);
    if core.reuse_on {
        // the pool replaces the prepared VM
        core.prep_on = false;
    }
    core.start_prep();
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
