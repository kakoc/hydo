//! hydo — a deterministic KVM hypervisor.
//!
//! Subcommands:
//!   run    <guest.bin>   Boot a guest and stream its console.
//!
//! More subcommands (verify, snapshot) arrive in later milestones.

mod boot;
mod det;
mod detlinux;
mod linux;
mod memory;
mod pmu;
mod pv;
mod serial;
mod state;
mod uart;
mod verify;
mod vmm;

use std::path::PathBuf;
use std::process::ExitCode;

use vmm::{BootConfig, ExitReason, GuestMode, Vmm};

const DEFAULT_MEM_MB: usize = 64;
const REAL_LOAD_ADDR: u64 = 0x1000;
const LONG_LOAD_ADDR: u64 = 0x10_0000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: hydo <run|verify|resume> [options]");
        return ExitCode::FAILURE;
    }
    let result = match args[1].as_str() {
        "run" => cmd_run(&args[2..]),
        "verify" => cmd_verify(&args[2..]),
        "resume" => cmd_resume(&args[2..]),
        "trace" => cmd_trace(&args[2..]),
        "pmuprobe" => cmd_pmuprobe(&args[2..]),
        "boot-linux" => cmd_boot_linux(&args[2..]),
        "boot-verify" => cmd_boot_verify(&args[2..]),
        "boot-step" => cmd_boot_step(&args[2..]),
        "boot-probe" => cmd_boot_probe(&args[2..]),
        other => Err(format!("unknown subcommand: {other}")),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("hydo: error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Tiny flag parser: collects `--key value` pairs and bare positionals.
struct Args {
    positionals: Vec<String>,
    flags: std::collections::HashMap<String, String>,
}

impl Args {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut positionals = Vec::new();
        let mut flags = std::collections::HashMap::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if let Some(key) = a.strip_prefix("--") {
                // A flag whose next token is another flag (or absent) is a bare
                // boolean and stores "true"; otherwise it consumes the value.
                match args.get(i + 1) {
                    Some(v) if !v.starts_with("--") => {
                        flags.insert(key.to_string(), v.clone());
                        i += 2;
                    }
                    _ => {
                        flags.insert(key.to_string(), "true".to_string());
                        i += 1;
                    }
                }
            } else {
                positionals.push(a.clone());
                i += 1;
            }
        }
        Ok(Args { positionals, flags })
    }

    fn u64_flag(&self, key: &str, default: u64) -> Result<u64, String> {
        match self.flags.get(key) {
            None => Ok(default),
            Some(v) => parse_u64(v).ok_or_else(|| format!("bad number for --{key}: {v}")),
        }
    }
}

/// Resolve `--mode real|long` (default real) and its default load address.
fn parse_mode(a: &Args) -> Result<(GuestMode, u64), String> {
    let mode = match a.flags.get("mode").map(|s| s.as_str()) {
        None | Some("real") => GuestMode::Real,
        Some("long") => GuestMode::Long,
        Some(other) => return Err(format!("unknown --mode {other} (use real|long)")),
    };
    let default_addr = match mode {
        GuestMode::Real => REAL_LOAD_ADDR,
        GuestMode::Long => LONG_LOAD_ADDR,
    };
    let load_addr = a.u64_flag("load-addr", default_addr)?;
    Ok((mode, load_addr))
}

/// Retry an operation that may hit the intermittent "PMU failed to schedule"
/// condition. A successful run is deterministic; retrying from scratch simply
/// waits for a run where the perf event schedules onto the PMU.
fn with_pmu_retry<T>(mut f: impl FnMut() -> Result<T, String>) -> Result<T, String> {
    const MAX_ATTEMPTS: usize = 12;
    for attempt in 1..=MAX_ATTEMPTS {
        match f() {
            Err(e) if e.contains("pmu counter failed to schedule") => {
                eprintln!("[hydo] PMU did not schedule (attempt {attempt}); retrying");
                continue;
            }
            other => return other,
        }
    }
    Err("PMU counter failed to schedule after repeated retries".into())
}

fn parse_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

fn cmd_run(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let guest: PathBuf = a
        .flags
        .get("guest")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("run: need a guest blob (--guest <path>)")?
        .into();
    let mem_mb = a.u64_flag("mem", DEFAULT_MEM_MB as u64)? as usize;
    let (mode, load_addr) = parse_mode(&a)?;
    let cfg = BootConfig {
        load_addr,
        mem_size: mem_mb * 1024 * 1024,
        seed: a.u64_flag("seed", 0)?,
        mode,
        timer_period: a.u64_flag("timer", 0)?,
        insn_clock: a.flags.contains_key("insn-clock"),
        fast: a.flags.contains_key("fast"),
    };
    let seed = cfg.seed;

    let snapshot_path = a.flags.get("snapshot");
    let snapshot_at = a.flags.get("snapshot-at").map(|s| parse_u64(s));
    if let Some(None) = snapshot_at {
        return Err("--snapshot-at needs a number".into());
    }

    // Boot and run, retrying on the intermittent PMU-scheduling failure (fast mode).
    let (vm, reason, pmu_counts) = with_pmu_retry(|| {
        let mut vm = Vmm::boot(&guest, cfg).map_err(|e| e.to_string())?;

        // Optional PMU diagnostic: measure guest-only retired instructions,
        // branches, and cycles across the whole run.
        let pmu_counters = if a.flags.contains_key("pmu") {
            match open_pmu() {
                Ok(c) => {
                    for ctr in &c {
                        ctr.reset();
                        ctr.enable();
                    }
                    Some(c)
                }
                Err(e) => {
                    eprintln!("[hydo] --pmu unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };

        // If asked to snapshot mid-run, step to that tick, save, then continue.
        let reason = match snapshot_at {
            Some(Some(at)) => match vm.step(at).map_err(|e| e.to_string())? {
                vmm::StepOutcome::Paused => {
                    let mid = vm.snapshot().map_err(|e| e.to_string())?;
                    if let Some(path) = snapshot_path {
                        mid.save(std::path::Path::new(path)).map_err(|e| e.to_string())?;
                        eprintln!(
                            "[hydo] mid-run snapshot at tick {at} -> {path} (fingerprint {})",
                            mid.fingerprint_hex()
                        );
                    }
                    vm.run().map_err(|e| e.to_string())?
                }
                vmm::StepOutcome::Exited(r) => r,
            },
            _ => vm.run().map_err(|e| e.to_string())?,
        };

        let pmu_counts = pmu_counters.map(|c| {
            for ctr in &c {
                ctr.disable();
            }
            [
                c[0].read().unwrap_or(0),
                c[1].read().unwrap_or(0),
                c[2].read().unwrap_or(0),
            ]
        });
        Ok((vm, reason, pmu_counts))
    })?;

    if let Some(counts) = pmu_counts {
        eprintln!(
            "[hydo] pmu guest counts: instructions={} branches={} cycles={}",
            counts[0], counts[1], counts[2]
        );
    }

    // Flush captured console output.
    let out = String::from_utf8_lossy(&vm.serial.out);
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
    if !vm.pv.checkpoints.is_empty() {
        let hexed: Vec<String> = vm.pv.checkpoints.iter().map(|v| format!("{v:#010x}")).collect();
        eprintln!("[hydo] checkpoints (seed={seed:#x}): {}", hexed.join(" "));
    }
    eprintln!("[hydo] virtual-clock ticks: {}", vm.pv.clock.now());
    if vm.insn_clock {
        eprintln!("[hydo] retired guest instructions: {}", vm.insn_count);
    }
    if vm.timer.period != 0 {
        let unit = if vm.insn_clock { "insns" } else { "exits" };
        eprintln!(
            "[hydo] timer interrupts delivered: {} (period {} {unit})",
            vm.timer.count, vm.timer.period
        );
        eprintln!("[hydo] injection offsets ({unit}): {:?}", vm.injection_log);
    }

    // Full-state fingerprint: a single 256-bit summary of registers, control
    // state, FPU, all guest RAM, and device state.
    let snap = vm.snapshot().map_err(|e| e.to_string())?;
    eprintln!("[hydo] state fingerprint: {}", snap.fingerprint_hex());
    // Save the *final* snapshot only when no mid-run cut was requested.
    if snapshot_at.is_none() {
        if let Some(path) = snapshot_path {
            snap.save(std::path::Path::new(path)).map_err(|e| e.to_string())?;
            eprintln!("[hydo] snapshot written to {path}");
        }
    }
    eprintln!("[hydo] guest exited: {reason:?}");

    Ok(match reason {
        ExitReason::Shutdown(code) if code != 0 => ExitCode::from(code as u8),
        _ => ExitCode::SUCCESS,
    })
}

/// Load a snapshot from disk and continue execution to completion. Restores the
/// full machine (registers, control state, pending events, RAM, devices, timer),
/// so a run snapshotted mid-flight in one process finishes deterministically in
/// another — disk-backed time travel.
fn cmd_resume(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let path = a
        .flags
        .get("snapshot")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("resume: need a snapshot file (--snapshot <path>)")?;

    let snap = state::Snapshot::load(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let mem_size = snap.mem.len();
    eprintln!(
        "[hydo] loaded snapshot {path}: fingerprint {} ({} MiB RAM)",
        snap.fingerprint_hex(),
        mem_size / (1024 * 1024)
    );

    let mut vm = Vmm::with_seed(mem_size, 0).map_err(|e| e.to_string())?;
    vm.restore(&snap).map_err(|e| e.to_string())?;

    let reason = vm.run().map_err(|e| e.to_string())?;

    let out = String::from_utf8_lossy(&vm.serial.out);
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
    let fin = vm.snapshot().map_err(|e| e.to_string())?;
    eprintln!("[hydo] resumed to completion: {reason:?}");
    eprintln!("[hydo] final fingerprint: {}", fin.fingerprint_hex());
    Ok(ExitCode::SUCCESS)
}

/// Diagnostic: single-step a guest and print the (exit, rip) trace.
fn cmd_trace(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let guest: PathBuf = a
        .flags
        .get("guest")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("trace: need a guest blob (--guest <path>)")?
        .into();
    let mem_mb = a.u64_flag("mem", DEFAULT_MEM_MB as u64)? as usize;
    let (mode, load_addr) = parse_mode(&a)?;
    let steps = a.u64_flag("steps", 40)? as usize;
    let cfg = BootConfig {
        load_addr,
        mem_size: mem_mb * 1024 * 1024,
        seed: a.u64_flag("seed", 0)?,
        mode,
        timer_period: 0,
        insn_clock: false,
        fast: false,
    };
    let mut vm = Vmm::boot(&guest, cfg).map_err(|e| e.to_string())?;
    let trace = vm.trace_singlestep(steps).map_err(|e| e.to_string())?;
    for (i, (label, rip)) in trace.iter().enumerate() {
        println!("  step {i:>4}  rip={rip:#010x}  {label}");
    }
    println!("  ({} steps traced)", trace.len());
    Ok(ExitCode::SUCCESS)
}

/// Diagnostic: free-run a guest and stop every `--period` instructions via a PMU
/// overflow signal, printing the counter value at each stop (to observe skid).
fn cmd_pmuprobe(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let guest: PathBuf = a
        .flags
        .get("guest")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("pmuprobe: need a guest blob (--guest <path>)")?
        .into();
    let mem_mb = a.u64_flag("mem", DEFAULT_MEM_MB as u64)? as usize;
    let (mode, load_addr) = parse_mode(&a)?;
    let period = a.u64_flag("period", 5000)?;
    let stops = a.u64_flag("stops", 6)? as usize;
    let cfg = BootConfig {
        load_addr,
        mem_size: mem_mb * 1024 * 1024,
        seed: a.u64_flag("seed", 0)?,
        mode,
        timer_period: 0,
        insn_clock: false,
        fast: false,
    };
    let mut vm = Vmm::boot(&guest, cfg).map_err(|e| e.to_string())?;
    let s = vm.pmu_overflow_probe(period, stops).map_err(|e| e.to_string())?;
    println!("period={period} stops (PMU insns at each overflow): {s:?}");
    Ok(ExitCode::SUCCESS)
}

/// Boot a real Linux kernel (bzImage) to a serial console. This is the
/// non-deterministic bring-up path: KVM in-kernel irqchip + PIT, host clock.
///
///   hydo boot-linux --kernel <bzImage> [--initrd <cpio.gz>]
///                   [--mem MiB] [--cmdline "..."] [--max-exits N]
fn cmd_boot_linux(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let kernel: PathBuf = a
        .flags
        .get("kernel")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("boot-linux: need a kernel bzImage (--kernel <path>)")?
        .into();
    let initrd: Option<PathBuf> = a.flags.get("initrd").map(PathBuf::from);
    let mem_mb = a.u64_flag("mem", 512)? as usize;
    let default_cmd = "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off";
    let cmdline = a
        .flags
        .get("cmdline")
        .cloned()
        .unwrap_or_else(|| default_cmd.to_string());
    let max_exits = a.u64_flag("max-exits", 50_000_000)?;

    linux::boot_linux(
        &kernel,
        initrd.as_deref(),
        &cmdline,
        mem_mb * 1024 * 1024,
        max_exits,
    )
    .map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Boot a real Linux kernel TWICE under hydo's instruction clock and measure how
/// deep into boot the two runs stay bit-identical, then bisect to the exact first
/// divergence and diagnose its cause. This is the deterministic-boot harness.
///
///   hydo boot-verify --kernel <bzImage> [--initrd <cpio.gz>] [--mem MiB]
///                    [--det-level 0..3] [--cmdline "..."]
///                    [--interval INSNS] [--max-insns INSNS] [--ram]
fn cmd_boot_verify(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let kernel: PathBuf = a
        .flags
        .get("kernel")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("boot-verify: need a kernel bzImage (--kernel <path>)")?
        .into();
    let initrd: Option<PathBuf> = a.flags.get("initrd").map(PathBuf::from);
    let mem_mb = a.u64_flag("mem", 256)? as usize;
    let det_level = a.u64_flag("det-level", 3)? as u32;
    let det = detlinux::DetConfig::level(det_level);

    // Cmdline appropriate to the determinism level (cheap, high-value knobs).
    let default_cmd = match det_level {
        0 => "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off".to_string(),
        1 => "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off \
              random.trust_cpu=off random.trust_bootloader=off"
            .to_string(),
        _ => "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off \
              random.trust_cpu=off random.trust_bootloader=off \
              no-kvmclock tsc=reliable nohz=off"
            .to_string(),
    };
    let cmdline = a.flags.get("cmdline").cloned().unwrap_or(default_cmd);

    let interval = a.u64_flag("interval", 20_000_000)?;
    let max_insns = a.u64_flag("max-insns", 600_000_000)?;
    let with_ram = a.flags.contains_key("ram");

    let opts = crate::boot::LinuxBootOpts {
        kernel: &kernel,
        initramfs: initrd.as_deref(),
        cmdline: &cmdline,
        mem_size: mem_mb * 1024 * 1024,
    };
    detlinux::boot_verify(&opts, det, interval, max_insns, with_ram).map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Diagnostic: verify the PMU fast-path landing is reproducible at a fixed budget.
fn cmd_boot_probe(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let kernel: PathBuf = a
        .flags
        .get("kernel")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("boot-probe: need a kernel bzImage (--kernel <path>)")?
        .into();
    let initrd: Option<PathBuf> = a.flags.get("initrd").map(PathBuf::from);
    let mem_mb = a.u64_flag("mem", 256)? as usize;
    let det_level = a.u64_flag("det-level", 3)? as u32;
    let budget = a.u64_flag("budget", 50_000_000)?;
    let reps = a.u64_flag("reps", 4)? as u32;
    let cmdline = a
        .flags
        .get("cmdline")
        .cloned()
        .unwrap_or_else(|| "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off".to_string());
    let opts = crate::boot::LinuxBootOpts {
        kernel: &kernel,
        initramfs: initrd.as_deref(),
        cmdline: &cmdline,
        mem_size: mem_mb * 1024 * 1024,
    };
    detlinux::boot_probe(&opts, detlinux::DetConfig::level(det_level), budget, reps)
        .map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Diagnostic: single-step one Linux boot and print its RIP trajectory.
fn cmd_boot_step(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let kernel: PathBuf = a
        .flags
        .get("kernel")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("boot-step: need a kernel bzImage (--kernel <path>)")?
        .into();
    let initrd: Option<PathBuf> = a.flags.get("initrd").map(PathBuf::from);
    let mem_mb = a.u64_flag("mem", 256)? as usize;
    let det_level = a.u64_flag("det-level", 3)? as u32;
    let max_steps = a.u64_flag("max-steps", 200_000)?;
    let print_every = a.u64_flag("every", 10_000)?;
    let cmdline = a
        .flags
        .get("cmdline")
        .cloned()
        .unwrap_or_else(|| "console=ttyS0 earlyprintk=ttyS0 loglevel=8 nokaslr acpi=off".to_string());
    let opts = crate::boot::LinuxBootOpts {
        kernel: &kernel,
        initramfs: initrd.as_deref(),
        cmdline: &cmdline,
        mem_size: mem_mb * 1024 * 1024,
    };
    detlinux::boot_step_debug(&opts, detlinux::DetConfig::level(det_level), max_steps, print_every)
        .map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

/// Open the three guest-only diagnostic counters: instructions, branches, cycles.
fn open_pmu() -> std::io::Result<[pmu::PerfCounter; 3]> {
    Ok([
        pmu::PerfCounter::open_guest(pmu::TYPE_HARDWARE, pmu::HW_INSTRUCTIONS)?,
        pmu::PerfCounter::open_guest(pmu::TYPE_HARDWARE, pmu::HW_BRANCHES)?,
        pmu::PerfCounter::open_guest(pmu::TYPE_HARDWARE, pmu::HW_CYCLES)?,
    ])
}

fn cmd_verify(args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args)?;
    let guest: PathBuf = a
        .flags
        .get("guest")
        .cloned()
        .or_else(|| a.positionals.first().cloned())
        .ok_or("verify: need a guest blob (--guest <path>)")?
        .into();
    let mem_mb = a.u64_flag("mem", DEFAULT_MEM_MB as u64)? as usize;
    let (mode, load_addr) = parse_mode(&a)?;
    let cfg = BootConfig {
        load_addr,
        mem_size: mem_mb * 1024 * 1024,
        seed: a.u64_flag("seed", 0)?,
        mode,
        timer_period: a.u64_flag("timer", 0)?,
        insn_clock: a.flags.contains_key("insn-clock"),
        fast: a.flags.contains_key("fast"),
    };

    println!("hydo verify: {}", guest.display());
    println!(
        "  mode={mode:?}  mem={mem_mb}MiB  load={load_addr:#x}  seed={:#x}  timer={}  clock={}\n",
        cfg.seed,
        cfg.timer_period,
        if cfg.insn_clock { "instructions" } else { "exits" }
    );

    let report = with_pmu_retry(|| verify::verify(&guest, cfg))?;

    for c in &report.checks {
        let mark = match (c.critical, c.passed) {
            (true, true) => "PASS",
            (true, false) => "FAIL",
            (false, true) => "PASS",
            (false, false) => "note",
        };
        println!("  [{mark}] {:<18} {}", c.name, c.detail);
    }
    println!();
    if report.deterministic() {
        let critical = report.checks.iter().filter(|c| c.critical).count();
        println!("DETERMINISTIC: all {critical} determinism checks passed.");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("NON-DETERMINISTIC: at least one determinism check failed.");
        Ok(ExitCode::FAILURE)
    }
}
