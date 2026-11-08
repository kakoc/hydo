//! The hvx engine's options (hydo-hvx): the variables the deleted C engine `hvx_explore.c` and
//! `hvx_stack_verify.c` read, read as they read them, plus hydo-hvx's own (`HYDO_HVX_*`). Text
//! fields are numbers the engine reads with C's `strtoull` at the point of use.

use crate::registry::*;
use crate::Env;

/// The hvx engine's options. hydo-hvx re-exports it as `hydo_hvx::engine::Options`.
#[derive(Clone, Debug)]
pub struct Options {
    /// `HVX_DEV` (empty or unset: `/dev/hvx`).
    pub dev: String,
    /// The vCPU count (set by the engine from `--vcpus` / `HVX_EXPLORE_VCPUS`; 1 here).
    pub vcpus: u32,
    /// `HVX_VCPU_PARK`, set and not 0.
    pub park: bool,
    /// `HVX_CPU_V3`, set and not 0.
    pub cpu_v3: bool,
    /// `HVX_CPU_CACHE`, set and not 0 (M195: the CPUID cache leaves, `guest.cpu_cache`).
    pub cpu_cache: bool,
    /// `HVX_PMU_CLOCK`, not 0 (default on).
    pub pmu_clock: bool,
    /// `HVX_PAUSE_EXIT`, not 0 (default on).
    pub pause_exit: bool,
    /// `HVX_PT_TRACE_VM`, set.
    pub pt_trace_vm: bool,
    /// `HVX_PAUSE_K`.
    pub pause_k: Option<String>,
    /// `HVX_EXPLORE_WALL_SECS` (C `atof`; not positive: 900).
    pub wall_secs: f64,
    /// `HVX_TURN_MARGIN`.
    pub turn_margin: Option<String>,
    /// `HVX_QUANTUM`.
    pub quantum: Option<String>,
    /// `HVX_BURST`.
    pub burst: Option<String>,
    /// `HVX_QUANTUM_AFTER`, set.
    pub quantum_after: bool,
    /// `HVX_FTSTAT`, set.
    pub ftstat: bool,
    /// `HVX_EXPLORE_CMDLINE_EXTRA`.
    pub cmdline_extra: Option<String>,
    /// `HVX_EXPLORE_PHASES` set and not 0, or `HVX_EXPLORE_DIRTY`.
    pub phases: bool,
    /// `HVX_EXPLORE_NO_UNSYNC`, set.
    pub no_unsync: bool,
    /// `HVX_EXPLORE_DUMP`.
    pub dump: Option<String>,
    /// `HVX_EXPLORE_DUMP_HASHONLY`, set.
    pub dump_hashonly: bool,
    /// `HVX_EXPLORE_DURABLE_OUT`.
    pub durable_out: Option<String>,
    /// `HVX_EXPLORE_BOOTCON`, set.
    pub bootcon: bool,
    /// `HVX_CONSOLE_OUT`.
    pub console_out: Option<String>,
    /// `HVX_DRAIN_NO_RETRY`, set.
    pub drain_no_retry: bool,
    /// `HVX_EXPLORE_VCPUSTAT`, set.
    pub vcpustat: bool,
    /// `HVX_EXPLORE_RAM_MIB` (read by the engine when `--ram-mib` is not given).
    pub ram_mib_env: Option<String>,
    /// `HVX_EXPLORE_VCPUS` (read by the engine when `--vcpus` is not given).
    pub vcpus_env: Option<String>,
    /// `HVX_EXPLORE_PT_ELF`: trace this program's text (coverage).
    pub pt_elf: Option<String>,
    /// `HVX_EXPLORE_PT_RANGES`: trace up to two IP ranges instead.
    pub pt_ranges: Option<String>,
    /// `HVX_EXPLORE_PTMULTI`: the external decoder for PT_RANGES traces.
    pub ptmulti: Option<String>,
    /// `HVX_EXPLORE_PT_MIB`: the trace buffer (default 256 MiB).
    pub pt_mib: Option<String>,
    /// `HVX_EXPLORE_PT_SAVE`: keep the raw trace as `<tag>.pt`.
    pub pt_save: bool,
    /// `HVX_EXPLORE_DIRTY=1` (with PHASES): pages of RAM and of the data disk that differ from S1.
    pub dirty: bool,
    /// `HVX_EXPLORE_ZSKIP` (with PHASES): hash RAM both ways at the workload's end and compare.
    pub zskip: bool,
    /// `HVX_EXPLORE_RETLOG`: one `RET` line per run call on stderr.
    pub retlog: bool,
    /// `HVX_SIM_DRAIN_DROP=<marker>`: fail the first console read that shows the marker, once.
    pub sim_drain_drop: Option<String>,
    /// `HYDO_HVX_REAPER=0`: tear old VMs down in line, as the C engine did (default: on a thread).
    pub reaper: bool,
    /// `HYDO_HVX_CHECK_ZERO=1`: verify that a fresh VM's RAM and disk are zero before a restore.
    pub check_zero: bool,
    /// `HYDO_HVX_PREPARE=0`: make every fresh VM when it is needed (default in serve mode: the
    /// next one is made on a helper thread while a run runs).
    pub prepare: bool,
    /// The host-call port (M138; on by default since M140, `HYDO_HVX_HOSTCALL=0` turns it off):
    /// every VM is made with HVX_VM_HOSTCALL, and when the module has the port (its HC ioctls
    /// answer) a controlled boot of an agent image gets `hydo.hc=1` on its kernel command line.
    /// An agent that uses it (hydo-agent 0.3.0 and later) sends its lines through the port, and
    /// the host's lines reach it there; a module without the port or an older agent stays on the
    /// serial line, unchanged.
    pub hostcall: bool,
    /// `HYDO_HVX_REUSE=1` (M138): keep a used VM and bring it back to the next run's snapshot in
    /// place (HVX_RESET_VM and the dirty-page log of a module that has them) instead of making
    /// a fresh VM.
    pub reuse: bool,
    /// `HYDO_HVX_CHECK_RESET=1`: after every in-place restore, compare all of RAM and the disk
    /// with what a fresh VM would hold, and end the run with an error on a difference.
    pub check_reset: bool,
}

/// C's `atof` on the number prefix (as `hydo_hvx::cstr::atof`).
fn atof(s: &str) -> f64 {
    let t = s.trim_start();
    let end = t
        .char_indices()
        .take_while(|(i, c)| {
            c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '-' || *c == '+'))
        })
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    t[..end].parse().unwrap_or(0.0)
}

impl Options {
    pub fn from_env(e: &Env) -> Options {
        let get = |k: &str| e.text(k);
        let wall = get(HVX_EXPLORE_WALL_SECS)
            .map(|v| atof(&v))
            .filter(|&v| v > 0.0)
            .unwrap_or(900.0);
        let dirty = e.set_not_0(HVX_EXPLORE_DIRTY);
        Options {
            dev: e
                .nonempty(HVX_DEV)
                .unwrap_or_else(|| crate::engine::DEFAULT_DEV.into()),
            vcpus: 1,
            park: e.set_not_0(HVX_VCPU_PARK),
            cpu_v3: e.set_not_0(HVX_CPU_V3),
            cpu_cache: e.set_not_0(HVX_CPU_CACHE),
            pmu_clock: e.not_0(HVX_PMU_CLOCK),
            pause_exit: e.not_0(HVX_PAUSE_EXIT),
            pt_trace_vm: e.is_set(HVX_PT_TRACE_VM),
            pause_k: get(HVX_PAUSE_K),
            wall_secs: wall,
            turn_margin: get(HVX_TURN_MARGIN),
            quantum: get(HVX_QUANTUM),
            burst: get(HVX_BURST),
            quantum_after: e.is_set(HVX_QUANTUM_AFTER),
            ftstat: e.is_set(HVX_FTSTAT),
            cmdline_extra: get(HVX_EXPLORE_CMDLINE_EXTRA),
            phases: e.set_not_0(HVX_EXPLORE_PHASES) || dirty,
            no_unsync: e.is_set(HVX_EXPLORE_NO_UNSYNC),
            dump: get(HVX_EXPLORE_DUMP),
            dump_hashonly: e.is_set(HVX_EXPLORE_DUMP_HASHONLY),
            durable_out: get(HVX_EXPLORE_DURABLE_OUT),
            bootcon: e.is_set(HVX_EXPLORE_BOOTCON),
            console_out: get(HVX_CONSOLE_OUT),
            drain_no_retry: e.is_set(HVX_DRAIN_NO_RETRY),
            vcpustat: e.is_set(HVX_EXPLORE_VCPUSTAT),
            ram_mib_env: get(HVX_EXPLORE_RAM_MIB),
            vcpus_env: get(HVX_EXPLORE_VCPUS),
            pt_elf: get(HVX_EXPLORE_PT_ELF),
            pt_ranges: get(HVX_EXPLORE_PT_RANGES),
            ptmulti: get(HVX_EXPLORE_PTMULTI),
            pt_mib: get(HVX_EXPLORE_PT_MIB),
            pt_save: e.is_set(HVX_EXPLORE_PT_SAVE),
            dirty,
            zskip: e.is_set(HVX_EXPLORE_ZSKIP),
            retlog: e.is_set(HVX_EXPLORE_RETLOG),
            sim_drain_drop: e.nonempty(HVX_SIM_DRAIN_DROP),
            reaper: e.not_0(HYDO_HVX_REAPER),
            check_zero: e.set_not_0(HYDO_HVX_CHECK_ZERO),
            prepare: e.not_0(HYDO_HVX_PREPARE),
            hostcall: e.not_0(HYDO_HVX_HOSTCALL),
            reuse: e.set_not_0(HYDO_HVX_REUSE),
            check_reset: e.set_not_0(HYDO_HVX_CHECK_RESET),
        }
    }
}
