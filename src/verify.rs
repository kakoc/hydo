//! The determinism verification harness — the concrete proof that hydo executes
//! guests deterministically. It runs four independent checks and reports each:
//!
//!   1. Replay:        two full runs from the same image produce the identical
//!                     final state fingerprint and checkpoint trace.
//!   2. Round-trip:    snapshotting the final state and restoring it into a fresh
//!                     VM reproduces the same fingerprint.
//!   3. Branch:        pause mid-run, snapshot, and resume from that snapshot in a
//!                     *fresh* VM — it reaches the identical final state as the
//!                     original that ran straight through. This is the capability
//!                     that makes exploration/time-travel possible.
//!   4. Seed sensitivity: a different PRNG seed yields a different final state,
//!                     confirming the guest's randomness is actually being read
//!                     (i.e. the determinism is not because nothing varies).

use std::path::Path;

use crate::state::{hex, Snapshot};
use crate::vmm::{BootConfig, StepOutcome, Vmm};

pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    /// Critical checks decide the determinism verdict. Non-critical checks
    /// (e.g. seed-sensitivity) are informational and never fail a run that is
    /// genuinely deterministic but happens not to consume entropy.
    pub critical: bool,
    pub detail: String,
}

pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// The determinism verdict rests only on the critical checks.
    pub fn deterministic(&self) -> bool {
        self.checks.iter().filter(|c| c.critical).all(|c| c.passed)
    }
}

fn boot(guest: &Path, cfg: BootConfig) -> Result<Vmm, String> {
    Vmm::boot(guest, cfg).map_err(|e| e.to_string())
}

/// Run a fresh VM to completion; return its final snapshot.
fn run_to_end(guest: &Path, cfg: BootConfig) -> Result<Snapshot, String> {
    let mut vm = boot(guest, cfg)?;
    vm.run().map_err(|e| e.to_string())?;
    vm.snapshot().map_err(|e| e.to_string())
}

pub fn verify(guest: &Path, cfg: BootConfig) -> Result<Report, String> {
    let mut checks = Vec::new();

    // --- 1. Replay determinism ---
    let a = run_to_end(guest, cfg)?;
    let b = run_to_end(guest, cfg)?;
    let fp_a = a.fingerprint();
    let fp_b = b.fingerprint();
    checks.push(Check {
        name: "replay",
        critical: true,
        passed: fp_a == fp_b && a.device.checkpoints == b.device.checkpoints,
        detail: format!(
            "run1={} run2={} ({} checkpoints, {} insns, {} irqs)",
            &hex(&fp_a)[..16],
            &hex(&fp_b)[..16],
            a.device.checkpoints.len(),
            a.device.insn_count,
            a.device.timer_count,
        ),
    });

    // --- 2. Snapshot round-trip ---
    let mut vm = boot(guest, cfg)?;
    vm.restore(&a).map_err(|e| e.to_string())?;
    let round = vm.snapshot().map_err(|e| e.to_string())?;
    let fp_round = round.fingerprint();
    checks.push(Check {
        name: "round-trip",
        critical: true,
        passed: fp_round == fp_a,
        detail: format!("restored={} original={}", &hex(&fp_round)[..16], &hex(&fp_a)[..16]),
    });

    // --- 3. Branch from a mid-run snapshot ---
    // The run length in the active clock's units; cut at the half-way point.
    let total = if cfg.insn_clock {
        a.device.insn_count
    } else {
        a.device.clock_ticks
    };
    let cut = (total / 2).max(1);

    // Original VM: run to the cut, snapshot, then finish in place.
    let mut orig = boot(guest, cfg)?;
    let at_cut = orig.step(cut).map_err(|e| e.to_string())?;
    let mid = orig.snapshot().map_err(|e| e.to_string())?;
    let fin_orig = match at_cut {
        StepOutcome::Paused => {
            orig.run().map_err(|e| e.to_string())?;
            orig.snapshot().map_err(|e| e.to_string())?
        }
        StepOutcome::Exited(_) => orig.snapshot().map_err(|e| e.to_string())?,
    };

    // Fresh VM: restore the mid snapshot and finish.
    let mut branch = boot(guest, cfg)?;
    branch.restore(&mid).map_err(|e| e.to_string())?;
    branch.run().map_err(|e| e.to_string())?;
    let fin_branch = branch.snapshot().map_err(|e| e.to_string())?;

    let fp_orig = fin_orig.fingerprint();
    let fp_branch = fin_branch.fingerprint();
    checks.push(Check {
        name: "branch",
        critical: true,
        passed: fp_orig == fp_branch && fp_branch == fp_a,
        detail: format!(
            "cut@{cut}/{total}  straight={} branched={}",
            &hex(&fp_orig)[..16],
            &hex(&fp_branch)[..16],
        ),
    });

    // --- 4. Seed sensitivity (informational) ---
    // Compare *guest-observable* output (checkpoints + console), not the full
    // fingerprint: the seed is baked into the hashed PRNG state, so the
    // fingerprint would differ even for a guest that never reads randomness. This
    // way the check honestly reflects whether the guest consumed entropy.
    let other = BootConfig {
        seed: cfg.seed ^ 0xDEAD_BEEF,
        ..cfg
    };
    let c = run_to_end(guest, other)?;
    let observable_differs =
        a.device.checkpoints != c.device.checkpoints || a.device.serial_out != c.device.serial_out;
    checks.push(Check {
        name: "seed-sensitivity",
        critical: false,
        passed: observable_differs,
        detail: if observable_differs {
            format!(
                "observable output varies with seed ({} checkpoints)",
                a.device.checkpoints.len()
            )
        } else {
            "guest consumes no entropy; observable output identical".to_string()
        },
    });

    Ok(Report { checks })
}
