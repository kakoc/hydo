//! The inventory: every `HYDO_*`, `HVX_*` and `HVM_*` name the Rust crates use, with its owner,
//! kind, type, default, readers and documentation. [`VARIABLES`] is the one list; each entry also
//! becomes a constant of the same name (`hydo_config::registry::HVX_DEV == "HVX_DEV"`), which is
//! how code outside this crate names a variable. `INVENTORY.md` next to this crate is this list
//! printed by [`inventory_markdown`]; a test fails when the two differ.
//!
//! Only names of the kinds [`Kind::User`] and [`Kind::Developer`] are read from hydo's own
//! environment, and only by [`crate::Env::process`]. The other kinds are documented here so the
//! list is complete: test switches read by test code, the guest interface (names hydo hands to
//! the guest's commands, or that guest programs read), variables of an image's
//! `/etc/hydo/manifest.sh` that the host reads back, and names hydo sets for a program it starts.

/// Who owns a variable (which typed group of [`crate::Config`] carries it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// [`crate::engine::Engine`]: which hvx engine runs, the device, the module lock, the program.
    Engine,
    /// [`crate::hvx::Options`]: the hvx engine's own switches (hydo-hvx).
    HvxEngine,
    /// [`crate::hvx::Options`]: the hvx engine's diagnostics (hydo-hvx), for developers.
    HvxDiagnostics,
    /// [`crate::run::Paths`]: where images and the shared-lock wrapper are.
    Paths,
    /// [`crate::run::Pool`]: the explorer's worker pool.
    Pool,
    /// [`crate::build::Build`]: inputs of an image build (hydo-build).
    Build,
    /// [`crate::hvm::Engine`] and [`crate::hvm::Options`]: the Mac engine (hydo-engine, hydo-hvm).
    Hvm,
    /// [`crate::kvm::Engine`] and [`crate::kvm::Options`]: the KVM engine (hydo-engine, hydo-kvm).
    Kvm,
    /// [`crate::run::Control`]: the host controller of agent images (hydo-control).
    Control,
    /// [`crate::run::Cli`]: the `hydo` command.
    Cli,
    /// [`crate::tools::Tools`]: helper programs (hydo-nix-fetch, hydo-grpc-gen).
    Tools,
    /// Switches of the workspace's tests and examples (read by test code only).
    Tests,
    /// The guest interface: set for guest commands or read by guest programs.
    Guest,
    /// Shell variables of an image's `/etc/hydo/manifest.sh`.
    Image,
    /// Set by hydo for a program it starts.
    Passed,
}

/// What kind of name it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// User-facing configuration, read from hydo's environment (through [`crate::Env`]).
    User,
    /// Developer-only configuration (diagnostics, a tree's paths, checks), read from hydo's
    /// environment (through [`crate::Env`]).
    Developer,
    /// A test or example switch, read by test code (`tests/`, `examples/`) only.
    Test,
    /// The guest interface: hydo never reads it from its own environment.
    Guest,
    /// A variable of an image's `/etc/hydo/manifest.sh` (not an environment variable).
    Image,
    /// Set by hydo in the environment of a program it starts; hydo does not read it.
    Passed,
}

impl Kind {
    /// Read from hydo's own environment (by [`crate::Env::process`]).
    pub fn is_host_config(self) -> bool {
        matches!(self, Kind::User | Kind::Developer)
    }
}

/// One entry of the inventory.
#[derive(Clone, Copy, Debug)]
pub struct Variable {
    pub name: &'static str,
    pub group: Group,
    pub kind: Kind,
    /// Its type and how a value is read (for example "flag: set and not `0`").
    pub ty: &'static str,
    /// Its default ("" when there is none or it is unset by default).
    pub default: &'static str,
    /// Who reads (or, for [`Kind::Guest`] and [`Kind::Passed`], who sets) it.
    pub readers: &'static str,
    /// What it does.
    pub doc: &'static str,
}

/// A variable removed in M144 or later, with why.
#[derive(Clone, Copy, Debug)]
pub struct Removed {
    pub name: &'static str,
    pub why: &'static str,
}

macro_rules! variables {
    ($( $(#[doc = $doc:literal])* $name:ident: $group:ident, $kind:ident, $ty:literal, $default:literal, $readers:literal; )*) => {
        $(
            $(#[doc = $doc])*
            pub const $name: &str = stringify!($name);
        )*
        /// Every variable, in this file's order.
        pub const VARIABLES: &[Variable] = &[
            $( Variable {
                name: stringify!($name),
                group: Group::$group,
                kind: Kind::$kind,
                ty: $ty,
                default: $default,
                readers: $readers,
                doc: concat!($($doc, "\n",)*),
            }, )*
        ];
    };
}

variables! {
    // ------------------------------------------------------------------ Engine
    /// Where the hvx engine runs: `inproc` on a thread of this process, `process` (or `c`) as
    /// the program `hydo-hvx`; anything else: in this process exactly when this is Linux,
    /// `HYDO_SUDO` is not `1` and this process can read and write the device.
    HYDO_ENGINE: Engine, User, "word: inproc | process | c", "auto", "hydo-engine";
    /// `1`: the engine program runs under sudo even when the user can open the device (the
    /// release's `libexec/hvx-run` reads it too; `hydo doctor` reports it).
    HYDO_SUDO: Engine, User, "flag: exactly `1`", "", "hydo-engine, hydo-cli, libexec/hvx-run";
    /// The hvx device (another name: a side module). An empty value means the default.
    HVX_DEV: Engine, User, "path", "/dev/hvx", "hydo-engine, hydo-hvx, hydo-cli, libexec/hvx-run";
    /// The module lock file every run holds shared (a reload holds it exclusively). An empty
    /// value means the default (in `$SUDO_USER`'s home under sudo).
    HYDO_LOCK: Engine, User, "path", "~/.hvx-module.lock", "hydo-engine, hydo-cli, libexec/hvx-run";
    /// The engine program started when the engine does not run in this process (the name is
    /// the C engine's, deleted in M137).
    HYDO_HVX_EXPLORE: Engine, User, "path", "<root>/target/release/hydo-hvx, else <root>/bin/hydo-hvx", "hydo-engine";

    // ------------------------------------------------------------------ HvxEngine
    /// `0` turns the host-call port off (M138; on by default since M140).
    HYDO_HVX_HOSTCALL: HvxEngine, User, "flag: not `0`", "on", "hydo-hvx";
    /// `1` (not `0`): keep a used VM and reset it in place for the next run (M138).
    HYDO_HVX_REUSE: HvxEngine, User, "flag: set and not `0`", "off", "hydo-hvx";
    /// Not `0`: after every in-place reset, compare RAM and disk with a fresh VM's.
    HYDO_HVX_CHECK_RESET: HvxEngine, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// Not `0`: check that a fresh VM's RAM and disk are zero before a restore.
    HYDO_HVX_CHECK_ZERO: HvxEngine, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// `0`: make every fresh VM when it is needed (default: the next one on a helper thread).
    HYDO_HVX_PREPARE: HvxEngine, Developer, "flag: not `0`", "on", "hydo-hvx";
    /// `0`: tear old VMs down in line (default: on a helper thread).
    HYDO_HVX_REAPER: HvxEngine, Developer, "flag: not `0`", "on", "hydo-hvx";
    /// A run's wall-clock budget in seconds (C `atof`; not positive: the default).
    HVX_EXPLORE_WALL_SECS: HvxEngine, Developer, "number (C atof)", "900", "hydo-hvx";
    /// Not `0`: park idle vCPUs (several vCPUs).
    HVX_VCPU_PARK: HvxEngine, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// Not `0`: the guest CPU is x86-64-v3 (as `--cpu v3`).
    HVX_CPU_V3: HvxEngine, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// `0` turns the PMU branch clock off.
    HVX_PMU_CLOCK: HvxEngine, Developer, "flag: not `0`", "on", "hydo-hvx";
    /// `0` turns PAUSE exits off.
    HVX_PAUSE_EXIT: HvxEngine, Developer, "flag: not `0`", "on", "hydo-hvx";
    /// Set: trace the VM with Processor Trace (HVX_VM_PT_TRACE).
    HVX_PT_TRACE_VM: HvxEngine, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// The PAUSE-exit clock step (C strtoull).
    HVX_PAUSE_K: HvxEngine, Developer, "integer (C strtoull)", "the module's", "hydo-hvx";
    /// The vCPU turn-end margin (M85; `--turn-margin` of the explorer sets it).
    HVX_TURN_MARGIN: HvxEngine, Developer, "integer (C strtoull)", "the module's", "hydo-hvx, hydo-explore";
    /// The vCPU turn length in branches (several vCPUs).
    HVX_QUANTUM: HvxEngine, Developer, "integer (C strtoull)", "the schedule's", "hydo-hvx";
    /// Burst turns (M85).
    HVX_BURST: HvxEngine, Developer, "integer (C strtoull)", "off", "hydo-hvx";
    /// Set: apply `HVX_QUANTUM` after the boot only.
    HVX_QUANTUM_AFTER: HvxEngine, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// Extra words on the guest kernel's command line.
    HVX_EXPLORE_CMDLINE_EXTRA: HvxEngine, Developer, "text", "", "hydo-hvx";
    /// Set: keep the disk's volatile write cache out of snapshots (no HVX_GET/SET_UNSYNC).
    HVX_EXPLORE_NO_UNSYNC: HvxEngine, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// The guest RAM in MiB when `--ram-mib` is not given (256-3072).
    HVX_EXPLORE_RAM_MIB: HvxEngine, Developer, "integer", "1024", "hydo-hvx";
    /// The vCPU count when `--vcpus` is not given.
    HVX_EXPLORE_VCPUS: HvxEngine, Developer, "integer", "1", "hydo-hvx";

    // ------------------------------------------------------------------ HvxDiagnostics
    /// Not `0`: print the run's phases with their times (also on with `HVX_EXPLORE_DIRTY`).
    HVX_EXPLORE_PHASES: HvxDiagnostics, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// Not `0` (with PHASES): count the RAM and disk pages that differ from S1.
    HVX_EXPLORE_DIRTY: HvxDiagnostics, Developer, "flag: set and not `0`", "off", "hydo-hvx";
    /// Set (with PHASES): hash RAM both ways at the workload's end and compare.
    HVX_EXPLORE_ZSKIP: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// Set: one `RET` line per run call on stderr.
    HVX_EXPLORE_RETLOG: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// Set: print the fast-turn statistics.
    HVX_FTSTAT: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// A directory: dump the final state (RAM, disk) there.
    HVX_EXPLORE_DUMP: HvxDiagnostics, Developer, "path", "", "hydo-hvx";
    /// Set (with DUMP): write only the hashes.
    HVX_EXPLORE_DUMP_HASHONLY: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// A file: write the durable disk after a run there.
    HVX_EXPLORE_DURABLE_OUT: HvxDiagnostics, Developer, "path", "", "hydo-hvx";
    /// Set: keep the boot's console.
    HVX_EXPLORE_BOOTCON: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// A file: copy the console there.
    HVX_CONSOLE_OUT: HvxDiagnostics, Developer, "path", "", "hydo-hvx";
    /// Set: do not retry a netem drain.
    HVX_DRAIN_NO_RETRY: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// Set: print per-vCPU statistics.
    HVX_EXPLORE_VCPUSTAT: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// Coverage: trace this program's text (needs the `pt` build).
    HVX_EXPLORE_PT_ELF: HvxDiagnostics, Developer, "path", "", "hydo-hvx";
    /// Coverage: trace up to two IP ranges instead.
    HVX_EXPLORE_PT_RANGES: HvxDiagnostics, Developer, "text: ranges", "", "hydo-hvx";
    /// Coverage: the external decoder for PT_RANGES traces.
    HVX_EXPLORE_PTMULTI: HvxDiagnostics, Developer, "path", "", "hydo-hvx";
    /// Coverage: the trace buffer in MiB.
    HVX_EXPLORE_PT_MIB: HvxDiagnostics, Developer, "integer", "256", "hydo-hvx";
    /// Set (coverage): keep the raw trace as `<tag>.pt`.
    HVX_EXPLORE_PT_SAVE: HvxDiagnostics, Developer, "flag: set (any value)", "off", "hydo-hvx";
    /// A console marker: fail the first console read that shows it, once (a test of the drain).
    HVX_SIM_DRAIN_DROP: HvxDiagnostics, Developer, "text; empty = unset", "", "hydo-hvx";

    // ------------------------------------------------------------------ Paths
    /// The folder with `hydo/` and `hv/` for the programs `hydo-explore` and `hydo-bench`. An
    /// empty value means the default.
    HYDO_ROOT: Paths, Developer, "path", "the first parent of the program with hydo/ and hv/, else the source tree", "hydo-explore, hydo-bench";
    /// The images, `bzImage` and `initramfs-kafka.cpio.gz` (the `hydo` command sets the
    /// release's `guest/`). An empty value means the default.
    HYDO_BUILD: Paths, User, "path", "~/hydo/build", "hydo-explore, hydo-bench";
    /// The shared-lock wrapper the engine program is started through (the `hydo` command sets
    /// the release's `libexec/hvx-run`). An empty value means the default.
    HYDO_HVX_RUN: Paths, User, "path", "~/bin/hvx-run", "hydo-explore, hydo-bench";

    // ------------------------------------------------------------------ Pool
    /// While this file exists, the explorer starts no new VM (waits, checking every 30 s, at
    /// most 2 hours): a quiet window on a shared machine.
    HYDO_QUIET_FILE: Pool, User, "path", "", "hydo-explore, hydo-bench";
    /// A result cache: a deterministic exploration resumes from the results recorded there.
    HYDO_EXPLORE_CACHE: Pool, User, "path", "", "hydo-explore, hydo-bench";
    /// The explorer's tests only (M145): a scripted engine program that answers every schedule as a
    /// pure function of its text, instead of the host controller over a real engine
    /// (`hydo_explore::scripted`).
    HYDO_SCRIPTED_ENGINE: Pool, Developer, "path", "", "hydo-explore";

    // ------------------------------------------------------------------ Hvm
    /// Which Mac engine runs: `inproc` the Rust engine in this process (one VM per process; the
    /// process needs the hypervisor entitlement); else the program `hydo-hvm`. (`c`, the C hvmx,
    /// was removed with the C engine in M147.)
    HYDO_HVM: Hvm, User, "word: inproc", "the program hydo-hvm", "hydo-engine, hydo-cli";
    /// The Mac engine program (an empty value means the default).
    HYDO_HVM_ENGINE: Hvm, User, "path", "<root>/target/release/hydo-hvm, else <root>/bin/hydo-hvm", "hydo-engine";
    /// The data disk (virtio-blk); hydo-engine sets it for an image with a `-disk.img`.
    HVM_DISK: Hvm, User, "path", "", "hydo-hvm (set by hydo-engine)";
    /// `hydo-hvm hvm` writes the durable disk image here at its end (flushed bytes only).
    HVM_DISK_OUT: Hvm, Developer, "path", "", "hydo-hvm hvm";
    /// Guest RAM in MiB (the engine: 256..4096, default 1024; `hydo-hvm hvm`: default 256);
    /// `--ram-mib` wins.
    HVM_MEM_MB: Hvm, User, "number", "1024 (hydo-hvm hvm: 256)", "hydo-hvm";
    /// Virtual ticks of the 24 MHz counter per VM exit.
    HVM_STEP: Hvm, Developer, "number", "64", "hydo-hvm";
    /// The exit budget of `hydo-hvm hvm`.
    HVM_MAX_EXITS: Hvm, Developer, "number", "200000000", "hydo-hvm hvm";
    /// The engine's exit budget per phase of a run.
    HVM_RUN_EXITS: Hvm, Developer, "number", "50000000", "hydo-hvm";
    /// Set: copy the console to stdout as it is written.
    HVM_ECHO: Hvm, Developer, "flag: set", "", "hydo-hvm";
    /// One line per VM exit into this file (and the interrupted PC at each acknowledge).
    HVM_TRACE: Hvm, Developer, "path", "", "hydo-hvm";
    /// With HVM_TRACE: log the guest u64 at this RAM offset at every exit.
    HVM_TRACE_WATCH: Hvm, Developer, "number", "", "hydo-hvm";
    /// Set: log unknown system-register traps.
    HVM_TRACE_SYSREG: Hvm, Developer, "flag: set", "", "hydo-hvm";
    /// The guest kernel's command line (the engine appends ` quiet loglevel=1`).
    HVM_BOOTARGS: Hvm, Developer, "text", "console=ttyS0 ... random.trust_bootloader=on", "hydo-hvm";
    /// Seconds of exit-free host time after which a spin with no provable loop ends the run.
    HVM_SPIN_SECS: Hvm, Developer, "number", "10", "hydo-hvm";
    /// Milliseconds of exit-free host time before the spin observer looks (a hint only).
    HVM_SPIN_MS: Hvm, Developer, "number", "5", "hydo-hvm";
    /// `0`: no spin observer (the bare watchdog).
    HVM_SPIN_OBSERVE: Hvm, Developer, "flag: not `0`", "on", "hydo-hvm";
    /// The spin observer's log file.
    HVM_SPLOG: Hvm, Developer, "path", "", "hydo-hvm";
    /// `hydo-hvm hvm`: forced preemption points for the whole run (`<hex seed>,<points>,<first>,<last>`).
    HVM_PCT: Hvm, Developer, "text", "", "hydo-hvm hvm";
    /// `hydo-hvm hvm`: tick jitter for the whole run (`<hex seed>,<permille>,<max>[,<lo>,<hi>]`).
    HVM_TJ: Hvm, Developer, "text", "", "hydo-hvm hvm";
    /// Deliver IRQs by emulating the exception entry (`0`: the vCPU's IRQ line).
    HVM_SOFT_IRQ: Hvm, Developer, "number", "1 (hydo-hvm hvm: 0)", "hydo-hvm";
    /// `hydo-hvm hvm`: write `<prefix>.regs` and `<prefix>.ram` at the stop.
    HVM_DUMP: Hvm, Developer, "path", "", "hydo-hvm hvm";
    /// The disk's fault seed (the engine: the flush-aware class at probability 0 when unset).
    HVM_FAULT: Hvm, Developer, "number", "", "hydo-hvm";
    /// The disk's fault classes (`delay,error,drop,reorder,torn,unflushed` or a number).
    HVM_FAULT_CLASSES: Hvm, Developer, "list", "error", "hydo-hvm";
    /// The disk faults' probability in percent.
    HVM_FAULT_PROB: Hvm, Developer, "number", "100", "hydo-hvm";
    /// The longest disk delay in exits.
    HVM_FAULT_MAXDELAY: Hvm, Developer, "number", "8", "hydo-hvm";
    /// Set: attach the virtio-net device.
    HVM_NET: Hvm, Developer, "flag: set", "", "hydo-hvm";
    /// Scripted inbound frames (`[u16 big-endian length][frame]...`).
    HVM_NET_RX: Hvm, Developer, "path", "", "hydo-hvm";
    /// Virtual ticks between two delivered inbound frames.
    HVM_NET_RX_PACE: Hvm, Developer, "number", "4800000", "hydo-hvm";
    /// Captured outbound frames are written here at the end (`hydo-hvm hvm`).
    HVM_NET_TX_OUT: Hvm, Developer, "path", "", "hydo-hvm hvm";
    /// The network fault seed.
    HVM_NET_FAULT: Hvm, Developer, "number", "", "hydo-hvm";
    /// The network fault classes (`drop,corrupt,dup` or a number).
    HVM_NET_FAULT_CLASSES: Hvm, Developer, "list", "drop", "hydo-hvm";
    /// The network faults' probability in percent.
    HVM_NET_FAULT_PROB: Hvm, Developer, "number", "100", "hydo-hvm";
    /// Where the engine makes (and unlinks at once) its snapshot files.
    HVM_SNAP_DIR: Hvm, User, "path", "/tmp/hvm-snap", "hydo-hvm";
    /// Set: per-component hashes of a run's end state into `<outdir>/<tag>.diag`.
    HVMX_DIAG: Hvm, Developer, "flag: set", "", "hydo-hvm";
    /// With HVMX_DIAG: raw copies of these 1 MiB chunks of RAM (comma-separated offsets).
    HVMX_DIAG_CHUNKS: Hvm, Developer, "list", "", "hydo-hvm";
    /// Set: read the registers back after every restore and report differences.
    HVMX_CHECK_RESTORE: Hvm, Developer, "flag: set", "", "hydo-hvm";
    /// Set: do not mlock guest RAM.
    HVMX_NO_MLOCK: Hvm, Developer, "flag: set", "", "hydo-hvm";

    // ------------------------------------------------------------------ Kvm
    /// Where the KVM engine runs: `process` as the program `hydo-kvm`; else on a thread of this
    /// process (KVM allows several VMs in one process).
    HYDO_KVM: Kvm, User, "word: process", "in this process", "hydo-engine";
    /// The KVM engine program (an empty value means the default).
    HYDO_KVM_ENGINE: Kvm, Developer, "path", "<root>/target/release/hydo-kvm, else <root>/bin/hydo-kvm", "hydo-engine";
    /// The KVM engine's guest kernel (else the twin of the image's recorded x86_64 kernel,
    /// `bzImage-kvm`, found by sha256 where the image's kernel is looked up).
    HYDO_KVM_KERNEL: Kvm, User, "path", "bzImage-kvm, by sha256", "hydo-engine, hydo-cli";
    /// The KVM device.
    HYDO_KVM_DEV: Kvm, Developer, "path", "/dev/kvm", "hydo-kvm, hydo-cli";
    /// Nanoseconds of the guest's clock per VM exit.
    HYDO_KVM_STEP: Kvm, Developer, "number", "2500", "hydo-kvm";
    /// The engine's exit budget per phase of a run.
    HYDO_KVM_RUN_EXITS: Kvm, Developer, "number", "50000000", "hydo-kvm";
    /// Seconds of host time without a VM exit after which the run ends as a SPIN.
    HYDO_KVM_SPIN_SECS: Kvm, Developer, "number", "10", "hydo-kvm";
    /// Milliseconds of host time without a VM exit before the spin observer looks for a cycle
    /// (a hint only: what it decides does not depend on when it looks).
    HYDO_KVM_SPIN_MS: Kvm, Developer, "number", "5", "hydo-kvm";
    /// One line per VM exit into this file.
    HYDO_KVM_TRACE: Kvm, Developer, "path", "", "hydo-kvm";
    /// Set: copy the console to stdout as it is written (the program).
    HYDO_KVM_ECHO: Kvm, Developer, "flag: set", "", "hydo-kvm";
    /// Set: per-component hashes of a run's end state into `<outdir>/<tag>.diag`.
    HYDO_KVM_DIAG: Kvm, Developer, "flag: set", "", "hydo-kvm";
    /// With HYDO_KVM_DIAG: the first this many MiB of guest RAM are written next to each
    /// fingerprint (`<outdir>/<tag>.<phase>.ram`).
    HYDO_KVM_DIAG_RAM: Kvm, Developer, "number", "0", "hydo-kvm";
    /// Set: log every MSR access and I/O port the engine's model does not know.
    HYDO_KVM_TRACE_UNKNOWN: Kvm, Developer, "flag: set", "", "hydo-kvm";
    /// `kvm`: the `hydo` command runs x86_64 images on the KVM engine instead of hvx (Linux;
    /// `--backend kvm` does the same for one command).
    HYDO_BACKEND: Kvm, User, "word: hvx | kvm", "hvx", "hydo-cli";

    // ------------------------------------------------------------------ Control
    /// `0`: the host does not run a SPIN of an agent image on hvm or kvm again in the rewind mode
    /// (`hydo-hvm hvm` reads it too: `1` turns the rewind mode on).
    HVM_SPIN_REWIND: Control, User, "flag: exactly `0` turns it off", "on", "hydo-control, hydo-hvm";

    // ------------------------------------------------------------------ Cli
    /// `linux` or `mac`: run the `hydo` command as the other platform's (the CLI test checks
    /// the other platform's commands that need no engine).
    HYDO_CLI_PLATFORM: Cli, Developer, "word: linux | mac", "this binary's platform", "hydo-cli";
    /// A directory read in place of `/` for the host's kernel-module state (`/proc/modules`,
    /// `/proc/locks`, `/sys/module`, `/dev`, `/etc/udev/rules.d`): the CLI test sets an empty one,
    /// so its recorded answers are those of a host without hvx, KVM or the udev rule.
    HYDO_CLI_HOST_ROOT: Cli, Developer, "path; empty = `/`", "/", "hydo-cli";
    /// The install cache (Milestone 162, docs/design/install-cache.md): `store/` (release
    /// archives and guest kernels by sha256), `versions/<version>/` (releases `hydo setup --get`
    /// installed), `bin/hydo` (the current one). An empty value means the default.
    HYDO_HOME: Cli, User, "path", "~/.hydo", "hydo-cli";
    /// Where releases are fetched from: `<value>/v<version>/<file>` (hydo), and
    /// `<value>/skill-v<version>/<file>` (the onboarding skill, `hydo skill update`). HTTPS only
    /// (plain `http://` only to a loopback host). An empty value means the default.
    HYDO_RELEASE_URL: Cli, User, "URL", "https://github.com/kakoc/hydo/releases/download", "hydo-cli";
    /// Where `hydo skill update` lists the releases to find the newest skill release (tags
    /// `skill-v<version>`): a GitHub releases API URL answering a JSON list. HTTPS only (plain
    /// `http://` only to a loopback host). An empty value means the default.
    HYDO_RELEASE_API: Cli, User, "URL", "https://api.github.com/repos/kakoc/hydo/releases", "hydo-cli";
    /// Set and not `0`: never use the network (`hydo setup`, the kernel lookup of runs, `hydo skill
    /// update`); what the install cache does not have is refused with its URL (`--offline` does
    /// the same).
    HYDO_OFFLINE: Cli, User, "flag: set and not `0`", "", "hydo-cli";
    /// `warn`: a plan whose `hydo_version:` this hydo does not satisfy builds with a warning.
    HYDO_VERSION_CHECK: Build, User, "word: warn", "refuse", "hydo-build (through hydo-manifest)";

    // ------------------------------------------------------------------ Build
    /// The product's `onboard/` directory (MANIFEST.md, ../VERSION).
    HYDO_ONBOARD: Build, Developer, "path", "the release's, else the source tree's", "hydo-build";
    /// The x86_64 busybox (the `hydo` command sets the release's); on arm64, the host busybox
    /// that lists the applets.
    HYDO_BUSYBOX: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The arm64 busybox.
    HYDO_BUSYBOX_ARM64: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The x86_64 `hvx_seedrng`.
    HYDO_SEEDRNG: Build, User, "path; empty = default", "<build dir>/hvx_seedrng", "hydo-build";
    /// The arm64 `hvx_seedrng` (staged only when set).
    HYDO_SEEDRNG_ARM64: Build, User, "path; empty = unset", "not staged", "hydo-build";
    /// The `mke2fs` that writes the images.
    HYDO_MKE2FS: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The `faketime` that runs `mke2fs` (the release's `libexec/faketime` reads it too).
    HYDO_FAKETIME: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build, libexec/faketime";
    /// The iproute2 closure for network faults (x86_64).
    HYDO_IPROUTE2: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The iptables closure for the freeze (x86_64).
    HYDO_IPTABLES: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The guest kernel modules (x86_64).
    HYDO_MODROOT: Build, User, "path; empty = default", "the pinned /nix/store path", "hydo-build";
    /// The x86_64 `hydo_fc` (the freeze helper).
    HYDO_FCBIN: Build, User, "path; empty = default", "<build dir>/hydo_fc", "hydo-build";
    /// The arm64 `hydo_fc`.
    HYDO_FCBIN_ARM64: Build, User, "path; empty = default", "<build dir>/hydo_fc-arm64", "hydo-build";
    /// The x86_64 `hydo-world`.
    HYDO_WORLDBIN: Build, User, "path; empty = default", "<build dir>/hydo-world", "hydo-build";
    /// The arm64 `hydo-world`.
    HYDO_WORLDBIN_ARM64: Build, User, "path; empty = default", "<build dir>/hydo-world-arm64", "hydo-build";
    /// The x86_64 `hydo-read`.
    HYDO_READ_BIN: Build, User, "path; empty = default", "<build dir>/hydo-read", "hydo-build";
    /// The arm64 `hydo-read`.
    HYDO_READ_BIN_ARM64: Build, User, "path; empty = default", "<build dir>/hydo-read-arm64", "hydo-build";
    /// The x86_64 `hydo-agent` (its sha256 must be the pinned one).
    HYDO_AGENT_BIN: Build, User, "path; empty = default", "<build dir>/hydo-agent", "hydo-build";
    /// The arm64 `hydo-agent`.
    HYDO_AGENT_BIN_ARM64: Build, User, "path; empty = default", "<build dir>/hydo-agent-arm64", "hydo-build";
    /// `1`: stage an agent whose sha256 is not the pinned one (with a warning).
    HYDO_AGENT_UNPINNED: Build, Developer, "flag: exactly `1`", "off", "hydo-build";
    /// A directory holding `nix/store/...` for arm64 store files (the Mac release's `guest/tc`).
    HYDO_ARM64_STORE: Build, User, "path; empty = the host's /nix/store", "", "hydo-build";
    /// The arm64 `tc`; empty: an image without network faults.
    HYDO_TC_ARM64: Build, User, "path (empty is a value)", "the pinned /nix/store path", "hydo-build";
    /// The `debugfs` that sets the image's owners and modes.
    HYDO_DEBUGFS: Build, User, "path; empty = default", "next to mke2fs", "hydo-build";
    /// Set (not empty): keep the build user's file owners in a Linux release build.
    HYDO_KEEP_OWNERS: Build, User, "flag: set and not empty", "off", "hydo-build";

    // ------------------------------------------------------------------ Tools
    /// The binary cache `hydo-nix-fetch` reads.
    HYDO_NIX_CACHE: Tools, Developer, "URL", "https://cache.nixos.org", "hydo-nix-fetch";
    /// The default of `hydo-grpc-gen -arch`. An empty value means the default.
    HYDO_GRPC_ARCH: Tools, User, "word: amd64 | arm64 | amd64,arm64", "amd64", "hydo-grpc-gen";
    /// The default of `hydo-grpc-gen -client` (the prebuilt clients).
    HYDO_GRPC_CLIENT: Tools, User, "path", "", "hydo-grpc-gen";

    // ------------------------------------------------------------------ Tests
    /// `1`: rewrite a test's recorded answers (goldens) from this run's output.
    HYDO_BLESS: Tests, Test, "flag", "off", "many crates' tests";
    /// `1`: copy the current schema to the released one (only when a release ships).
    HYDO_RELEASE_SCHEMA: Tests, Test, "flag", "off", "hydo-results, hydo-agent-proto tests";
    /// Keep the Compose test's scratch files here.
    HYDO_COMPOSE_KEEP: Tests, Test, "path", "", "hydo-compose tests";
    /// Where the KVM ABI probe finds `linux/kvm.h` (the Linux UAPI headers).
    HYDO_KVM_HEADERS: Tests, Test, "path", "/usr/include", "hydo-kvm tests";
    /// Dump the controller tests' transcripts here.
    HYDO_CONTROL_DUMP: Tests, Test, "path", "", "hydo-control tests";
    /// Set: the controller tests' full seed ranges.
    HYDO_CONTROL_FULL: Tests, Test, "flag: set", "off", "hydo-control tests";
    /// Set: the explorer's identity corpus, quick part only.
    HYDO_CORPUS_QUICK: Tests, Test, "flag: set", "off", "hydo-explore tests";
    /// Set: the explorer's identity test in full.
    HYDO_IDENT_FULL: Tests, Test, "flag: set", "off", "hydo-explore tests";
    /// Set: write comments naming the recorder into the corpus goldens.
    HYDO_GOLDEN_FROM: Tests, Test, "flag: set", "off", "hydo-explore tests";
    /// Dump a golden comparison's inputs here.
    HYDO_GOLDEN_DUMP: Tests, Test, "path", "", "hydo-composer, hydo-errors tests";
    /// The saved error-log corpus to compare.
    HYDO_ERRLOG_CORPUS: Tests, Test, "path", "", "hydo-errors tests";
    /// `1`: the scripted engine program (the test binary run as the engine).
    HYDO_FAKE_ENGINE: Tests, Test, "flag", "", "hydo-explore tests";
    /// Another scripted engine program.
    HYDO_FAKE_ENGINE_BIN: Tests, Test, "path", "the test binary", "hydo-explore tests";
    /// Another explorer to run on the scripted engine.
    HYDO_FAKE_EXPLORER: Tests, Test, "command", "hydo-explore", "hydo-explore tests";
    /// Not empty: the scripted engine prints history records.
    HYDO_FAKE_HIST: Tests, Test, "flag: not empty", "", "hydo-explore tests";
    /// The history checker's dump directory.
    HYDO_HIST_DUMP: Tests, Test, "path", "", "hydo-history tests";
    /// Set: the history checker's full corpus.
    HYDO_HIST_FULL: Tests, Test, "flag: set", "off", "hydo-history tests";
    /// The deleted C engine's source, to re-record the parser's answers.
    HYDO_HVX_C_SRC: Tests, Test, "path", "", "hydo-hvx tests";
    /// A directory with the deleted C Mac engine's sources, to re-record the layout probe's answers.
    HYDO_HVM_C_SRC: Tests, Test, "path", "", "hydo-hvm tests";
    /// The Python used to re-record the old answers (before M107).
    HYDO_RECORD_PY: Tests, Test, "path", "", "hydo-composer, hydo-errors tests";
    /// Set: re-record the CLI's answers with the old Python CLIs.
    HYDO_RECORD_PYTHON: Tests, Test, "flag: set", "", "hydo-cli tests";
    /// The corpus for re-recording with Python.
    HYDO_RECORD_PY_CORPUS: Tests, Test, "path", "", "hydo-errors tests";
    /// The work directory for re-recording with Python.
    HYDO_RECORD_WORK: Tests, Test, "path", "", "hydo-composer, hydo-errors tests";
    /// Set: print the fakes test's transcripts.
    HYDO_SHOW: Tests, Test, "flag: set", "", "hydo-control tests";
    /// The skills-ref program for the skill check.
    HYDO_SKILLS_REF: Tests, Test, "path", "on PATH", "hydo-agents tests";
    /// The hydo-world program for the cross-check.
    HYDO_WORLD_BIN: Tests, Test, "path", "built", "hydo-world tests";
    /// Set: the Mac lock-step check prints its values instead of comparing them (M145).
    HVM_LOCKSTEP_RECORD: Tests, Test, "flag: set", "", "hydo-engine examples";
    /// The cross-check's first port.
    HYDO_XCHECK_PORT: Tests, Test, "integer", "", "hydo-world tests";
    /// A live etcd for hydo-read's store tests.
    HYDO_TEST_ETCD: Tests, Test, "address", "skipped", "hydo-read tests";
    /// A live Kafka for hydo-read's store tests.
    HYDO_TEST_KAFKA: Tests, Test, "address", "skipped", "hydo-read tests";
    /// A live MySQL for hydo-read's store tests.
    HYDO_TEST_MYSQL: Tests, Test, "address", "skipped", "hydo-read tests";
    /// A live PostgreSQL for hydo-read's store tests.
    HYDO_TEST_POSTGRES: Tests, Test, "address", "skipped", "hydo-read tests";
    /// A live Redis for hydo-read's store tests.
    HYDO_TEST_REDIS: Tests, Test, "address", "skipped", "hydo-read tests";
    /// A port nothing listens on, for hydo-read's store tests.
    HYDO_TEST_DEADPORT: Tests, Test, "address", "skipped", "hydo-read tests";

    // ------------------------------------------------------------------ Guest
    /// The workload's step count (for invariants).
    HYDO_STEPS: Guest, Guest, "integer", "", "set by hydo-control; read by the project's commands";
    /// The acknowledged ids (for invariants and the recovery).
    HYDO_ACKED: Guest, Guest, "list", "", "set by hydo-control; read by commands, hydo-read, hydo-world, hydo-grpc";
    /// The outcomes of the workload's steps.
    HYDO_OUTCOMES: Guest, Guest, "list", "", "set by hydo-control; read by commands, hydo-read, hydo-grpc";
    /// The failed ids.
    HYDO_FAILED: Guest, Guest, "list", "", "set by hydo-control; read by commands, hydo-read";
    /// The service a command is about.
    HYDO_SVC: Guest, Guest, "name", "", "set by hydo-control";
    /// The service's log file.
    HYDO_LOG: Guest, Guest, "path", "", "set by hydo-control";
    /// The first step of a workload range.
    HYDO_FROM: Guest, Guest, "integer", "", "set by hydo-control";
    /// The last step of a workload range.
    HYDO_TO: Guest, Guest, "integer", "", "set by hydo-control";
    /// A knob's value.
    HYDO_VALUE: Guest, Guest, "text", "", "set by hydo-control";
    /// A composed command's id.
    HYDO_CMD: Guest, Guest, "id", "", "set by hydo-control (the composer)";
    /// A composed command's entry.
    HYDO_CMD_ID: Guest, Guest, "id", "", "set by hydo-control (the composer)";
    /// A composed command's round.
    HYDO_ROUND: Guest, Guest, "integer", "", "set by hydo-control (the composer)";
    /// A composed command's seed.
    HYDO_SEED: Guest, Guest, "integer", "", "set by hydo-control (the composer); read by hydo-grpc";
    /// A composed check's id.
    HYDO_CHECK: Guest, Guest, "id", "", "set by hydo-control (the composer)";
    /// The agent's console device (hydo-agent, a guest program with pinned bytes).
    HYDO_AGENT_TTY: Guest, Guest, "path", "", "hydo-agent (guest)";
    /// hydo-world's record log.
    HYDO_WORLD_LOG: Guest, Guest, "path", "", "hydo-world (guest)";
    /// hydo-world's control socket.
    HYDO_WORLD_SOCK: Guest, Guest, "path", "", "hydo-world (guest)";
    /// hydo-grpc: the call records file.
    HYDO_GRPC_CALLS: Guest, Guest, "path", "", "hydo-grpc (guest)";
    /// hydo-grpc: write records to the console.
    HYDO_GRPC_CONSOLE: Guest, Guest, "flag", "", "hydo-grpc (guest)";
    /// hydo-grpc: the console device.
    HYDO_GRPC_CONSOLE_DEV: Guest, Guest, "path", "", "hydo-grpc (guest)";
    /// hydo-grpc: the generated driver's directory.
    HYDO_GRPC_DIR: Guest, Guest, "path", "", "hydo-grpc (guest)";
    /// hydo-grpc: the request stream's seed.
    HYDO_GRPC_SEED: Guest, Guest, "integer", "", "hydo-grpc (guest)";
    /// hydo-grpc: the state file.
    HYDO_GRPC_STATE: Guest, Guest, "path", "", "hydo-grpc (guest)";
    /// hydo-grpc: the server address.
    HYDO_GRPC_TARGET: Guest, Guest, "address", "", "hydo-grpc (guest)";

    // ------------------------------------------------------------------ Image
    /// The image's `tc` (manifest.sh), read back by the controller.
    HYDO_TC: Image, Image, "path", "", "written by hydo-build; read by hydo-control, hydo-engine";
    /// The image's `ip` (manifest.sh).
    HYDO_IP: Image, Image, "path", "", "written by hydo-build; read by hydo-control";
    /// The image's `iptables` (manifest.sh).
    HYDO_IPT: Image, Image, "path", "", "written by hydo-build; read by hydo-control";
    /// The image's history checks (manifest.sh).
    HYDO_CHECKS: Image, Image, "list", "", "written by hydo-build; read by hydo-control";

    // ------------------------------------------------------------------ Passed
    /// `hydo-bench rebaseline`'s work folder, for its staging script
    /// (`hydo/onboard/rebaseline/stage.sh`, M164).
    HYDO_REBASE_WORK: Passed, Passed, "path", "", "set by hydo-bench rebaseline for hydo/onboard/rebaseline/stage.sh";
    /// The folder of the inputs the staging script makes or fetches once (M164).
    HYDO_REBASE_INPUTS: Passed, Passed, "path", "", "set by hydo-bench rebaseline for hydo/onboard/rebaseline/stage.sh";
    /// The platform being re-baselined: `hvx`, `hvm` or `kvm` (M164).
    HYDO_REBASE_PLATFORM: Passed, Passed, "text", "", "set by hydo-bench rebaseline for hydo/onboard/rebaseline/stage.sh";
    /// The unpacked release whose guest files the staging script's prefix uses (M164).
    HYDO_REBASE_RELEASE: Passed, Passed, "path", "", "set by hydo-bench rebaseline for hydo/onboard/rebaseline/stage.sh";
}

/// The variables removed in M144 and later.
pub const REMOVED: &[Removed] = &[
    Removed {
        name: "HYDO_SNAP_DIR",
        why: "the snapshot directory of the deleted C engine; hydo-hvx keeps snapshots in memory (M132, M137)",
    },
    Removed {
        name: "HVX_SNAP_DIR",
        why: "passed to the engine program for the deleted C engine; hydo-hvx never read it (the C verify harnesses in hv/hvx read their own)",
    },
    Removed {
        name: "HVX_SNAP_MALLOC",
        why: "passed to the engine program for the deleted C engine; hydo-hvx never read it",
    },
    Removed {
        name: "HYDO_SNAPSHOTS",
        why: "chose between file and memory snapshots of the deleted C engine; it changed only the pool's printed line",
    },
    Removed {
        name: "HYDO_RAM_MIB",
        why: "M145: the guest RAM of the engine check example (engine_check.rs), deleted with the shell images' plain runs",
    },
    Removed {
        name: "HYDO_BENCH_KIND",
        why: "M145: hydo-bench's image kind; it runs the manifest-built agent images only (the stack kind, shell-init images, was deleted)",
    },
    Removed {
        name: "HYDO_M59_SUFFIX",
        why: "M145: the m59 suffix of the deleted stack kind of hydo-bench's images",
    },
    Removed {
        name: "HYDO_CLI",
        why: "told the explorer program it was started by the deleted Python CLIs; `hydo explore` calls the explorer in its own process",
    },
];

/// The variable named `name`.
pub fn find(name: &str) -> Option<&'static Variable> {
    VARIABLES.iter().find(|v| v.name == name)
}

fn doc_line(d: &str) -> String {
    d.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The inventory as Markdown (the text of `INVENTORY.md`).
pub fn inventory_markdown() -> String {
    let mut s = String::new();
    s += "# hydo's environment variables\n\n";
    s += "Generated from `hydo/crates/hydo-config/src/registry.rs` by \
          `hydo_config::registry::inventory_markdown` (M144); `cargo test -p hydo-config` fails \
          when this file differs, and `HYDO_BLESS=1` rewrites it. How to add an option: \
          `hydo/crates/hydo-config/src/lib.rs`.\n\n";
    let count = |k: Kind| VARIABLES.iter().filter(|v| v.kind == k).count();
    s += &format!(
        "{} user-facing and {} developer-only variables are read from hydo's environment, only \
         by `hydo_config::Env::process` at a program's start; {} test switches are read by test \
         code; {} names are the guest interface, {} are variables of an image's manifest.sh and \
         {} are set for programs hydo starts. {} variables were removed (M144, M145). Shell scripts \
         (`hv/`, `hydo/dist/`, build scripts) and the C programs have their own variables, not \
         listed here.\n",
        count(Kind::User),
        count(Kind::Developer),
        count(Kind::Test),
        count(Kind::Guest),
        count(Kind::Image),
        count(Kind::Passed),
        REMOVED.len()
    );
    let groups = [
        (Group::Engine, "Engine (`hydo_config::engine::Engine`)"),
        (
            Group::HvxEngine,
            "The hvx engine (`hydo_config::hvx::Options`)",
        ),
        (
            Group::HvxDiagnostics,
            "The hvx engine's diagnostics (`hydo_config::hvx::Options`)",
        ),
        (Group::Paths, "Paths (`hydo_config::run::Paths`)"),
        (
            Group::Pool,
            "The explorer's pool (`hydo_config::run::Pool`)",
        ),
        (
            Group::Hvm,
            "The Mac engine (`hydo_config::hvm::Engine`, `hydo_config::hvm::Options`)",
        ),
        (
            Group::Kvm,
            "The KVM engine (`hydo_config::kvm::Engine`, `hydo_config::kvm::Options`)",
        ),
        (
            Group::Control,
            "The host controller (`hydo_config::run::Control`)",
        ),
        (Group::Cli, "The `hydo` command (`hydo_config::run::Cli`)"),
        (Group::Build, "Image builds (`hydo_config::build::Build`)"),
        (
            Group::Tools,
            "Helper programs (`hydo_config::tools::Tools`)",
        ),
        (Group::Tests, "Test switches (read by test code only)"),
        (
            Group::Guest,
            "The guest interface (never read from hydo's environment)",
        ),
        (
            Group::Image,
            "Variables of an image's /etc/hydo/manifest.sh (not environment variables)",
        ),
        (Group::Passed, "Set for programs hydo starts"),
    ];
    for (g, title) in groups {
        s += &format!("\n## {title}\n\n");
        s += "| Variable | Kind | Type | Default | Readers | What it does |\n";
        s += "|---|---|---|---|---|---|\n";
        for v in VARIABLES.iter().filter(|v| v.group == g) {
            s += &format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                v.name,
                match v.kind {
                    Kind::User => "user",
                    Kind::Developer => "developer",
                    Kind::Test => "test",
                    Kind::Guest => "guest",
                    Kind::Image => "image",
                    Kind::Passed => "passed",
                },
                v.ty.replace('|', "\\|"),
                if v.default.is_empty() {
                    "-".to_string()
                } else {
                    v.default.to_string()
                },
                v.readers,
                doc_line(v.doc).replace('|', "\\|")
            );
        }
    }
    s += "\n## Removed\n\n| Variable | Why |\n|---|---|\n";
    for r in REMOVED {
        s += &format!("| `{}` | {} |\n", r.name, r.why);
    }
    s
}
