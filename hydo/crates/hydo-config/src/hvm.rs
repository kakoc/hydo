//! The Mac engine (M143): which engine runs ([`Engine`], read by hydo-engine) and the options of
//! the Rust engine hydo-hvm ([`Options`]): the variables the C engine (`hv/hvm/hvm.c`, `virtio.c`
//! and `explore/serve.c`, deleted in M147) read, read as they read them. Text fields are numbers
//! the engine reads with C's `strtoull`/`atoi`/`atof` at the point of use, as the C did.

use crate::registry::*;
use crate::Env;
use std::path::PathBuf;

/// Which Mac engine runs (`HYDO_HVM`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Choice {
    /// The Rust engine as the program `hydo-hvm` (one process per worker), the default.
    #[default]
    Program,
    /// `inproc`: the Rust engine on a thread of this process (one VM per process; the process
    /// needs the hypervisor entitlement).
    InProcess,
}

/// The Mac engine group (hydo-engine).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    /// `HYDO_HVM`: `inproc`, else the program (`c`, the C engine until M147, is the program too).
    pub choice: Choice,
    /// `HYDO_HVM_ENGINE`: the engine program; None (unset or empty): by [`Engine::choice`].
    pub program: Option<PathBuf>,
}

impl Engine {
    pub fn from_env(e: &Env) -> Engine {
        Engine {
            choice: match e.text(HYDO_HVM).as_deref() {
                Some("inproc") => Choice::InProcess,
                _ => Choice::Program,
            },
            program: e.path_nonempty(HYDO_HVM_ENGINE),
        }
    }
}

/// The Rust Mac engine's options (hydo-hvm), as the C engine read them with `getenv`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `HVM_MEM_MB`: guest RAM in MiB (the engine: 256..4096, default 1024; `hydo-hvm hvm`: default 256).
    pub mem_mb: Option<String>,
    /// `HVM_STEP`: virtual ticks per exit (default 64).
    pub step: Option<String>,
    /// `HVM_MAX_EXITS`: hvm's exit budget (default 2e8).
    pub max_exits: Option<String>,
    /// `HVM_RUN_EXITS`: the engine's exit budget per phase (default 50,000,000).
    pub run_exits: Option<String>,
    /// `HVM_ECHO`, set: the console copied to stdout as it is written.
    pub echo: bool,
    /// `HVM_TRACE`: one line per exit into this file.
    pub trace: Option<String>,
    /// `HVM_TRACE_WATCH`: with HVM_TRACE, the guest u64 at this RAM offset at every exit.
    pub trace_watch: Option<String>,
    /// `HVM_TRACE_SYSREG`, set: log unknown system-register traps.
    pub trace_sysreg: bool,
    /// `HVM_BOOTARGS`: the kernel command line (the engine appends its own words).
    pub bootargs: Option<String>,
    /// `HVM_SPIN_SECS`: give up on an exit-free spin after this much host time (default 10).
    pub spin_secs: Option<String>,
    /// `HVM_SPIN_MS`: exit-free host time before the spin observer looks (default 5).
    pub spin_ms: Option<String>,
    /// `HVM_SPIN_OBSERVE`: `0` turns the spin observer off.
    pub spin_observe: Option<String>,
    /// `HVM_SPIN_REWIND`: `1` always the rewind mode, `0` never, `auto` (unset) on a SPIN.
    pub spin_rewind: Option<String>,
    /// `HVM_SPLOG`: the spin observer's log file.
    pub splog: Option<String>,
    /// `HVM_PCT`: hvm's forced preemption points for the whole run.
    pub pct: Option<String>,
    /// `HVM_TJ`: hvm's tick jitter for the whole run.
    pub tj: Option<String>,
    /// `HVM_SOFT_IRQ`: deliver IRQs by emulating the exception entry (the engine: default 1; `hydo-hvm hvm`: 0).
    pub soft_irq: Option<String>,
    /// `HVM_DUMP`: hvm writes `<prefix>.regs` and `<prefix>.ram` at the stop.
    pub dump: Option<String>,
    /// `HVM_DISK`: the data disk (virtio-blk), set by hydo-engine for an image with one.
    pub disk: Option<String>,
    /// `HVM_DISK_OUT`: hvm writes the durable disk image here at its end.
    pub disk_out: Option<String>,
    /// `HVM_FAULT`: the disk fault seed (the engine sets the flush-aware class at probability 0
    /// when a disk is attached and this is unset).
    pub fault: Option<String>,
    /// `HVM_FAULT_CLASSES`.
    pub fault_classes: Option<String>,
    /// `HVM_FAULT_PROB`.
    pub fault_prob: Option<String>,
    /// `HVM_FAULT_MAXDELAY`.
    pub fault_maxdelay: Option<String>,
    /// `HVM_NET`, set: attach the virtio-net device.
    pub net: bool,
    /// `HVM_NET_RX`: scripted inbound frames.
    pub net_rx: Option<String>,
    /// `HVM_NET_RX_PACE`: virtual ticks between delivered frames.
    pub net_rx_pace: Option<String>,
    /// `HVM_NET_TX_OUT`: captured outbound frames written here at the end.
    pub net_tx_out: Option<String>,
    /// `HVM_NET_FAULT`.
    pub net_fault: Option<String>,
    /// `HVM_NET_FAULT_CLASSES`.
    pub net_fault_classes: Option<String>,
    /// `HVM_NET_FAULT_PROB`.
    pub net_fault_prob: Option<String>,
    /// `HVM_SNAP_DIR`: where the engine's snapshot files are made and unlinked (default /tmp/hvm-snap).
    pub snap_dir: Option<String>,
    /// `HVMX_DIAG`, set: per-component hashes of a run's end state into `<tag>.diag`.
    pub diag: bool,
    /// `HVMX_DIAG_CHUNKS`: with HVMX_DIAG, raw copies of these 1 MiB chunks of RAM.
    pub diag_chunks: Option<String>,
    /// `HVMX_CHECK_RESTORE`, set: read the registers back after a restore and compare.
    pub check_restore: bool,
    /// `HVMX_NO_MLOCK`, set: do not lock guest RAM.
    pub no_mlock: bool,
}

impl Options {
    pub fn from_env(e: &Env) -> Options {
        let t = |k: &str| e.text(k);
        Options {
            mem_mb: t(HVM_MEM_MB),
            step: t(HVM_STEP),
            max_exits: t(HVM_MAX_EXITS),
            run_exits: t(HVM_RUN_EXITS),
            echo: e.is_set(HVM_ECHO),
            trace: t(HVM_TRACE),
            trace_watch: t(HVM_TRACE_WATCH),
            trace_sysreg: e.is_set(HVM_TRACE_SYSREG),
            bootargs: t(HVM_BOOTARGS),
            spin_secs: t(HVM_SPIN_SECS),
            spin_ms: t(HVM_SPIN_MS),
            spin_observe: t(HVM_SPIN_OBSERVE),
            spin_rewind: t(HVM_SPIN_REWIND),
            splog: t(HVM_SPLOG),
            pct: t(HVM_PCT),
            tj: t(HVM_TJ),
            soft_irq: t(HVM_SOFT_IRQ),
            dump: t(HVM_DUMP),
            disk: t(HVM_DISK),
            disk_out: t(HVM_DISK_OUT),
            fault: t(HVM_FAULT),
            fault_classes: t(HVM_FAULT_CLASSES),
            fault_prob: t(HVM_FAULT_PROB),
            fault_maxdelay: t(HVM_FAULT_MAXDELAY),
            net: e.is_set(HVM_NET),
            net_rx: t(HVM_NET_RX),
            net_rx_pace: t(HVM_NET_RX_PACE),
            net_tx_out: t(HVM_NET_TX_OUT),
            net_fault: t(HVM_NET_FAULT),
            net_fault_classes: t(HVM_NET_FAULT_CLASSES),
            net_fault_prob: t(HVM_NET_FAULT_PROB),
            snap_dir: t(HVM_SNAP_DIR),
            diag: e.is_set(HVMX_DIAG),
            diag_chunks: t(HVMX_DIAG_CHUNKS),
            check_restore: e.is_set(HVMX_CHECK_RESTORE),
            no_mlock: e.is_set(HVMX_NO_MLOCK),
        }
    }
}
