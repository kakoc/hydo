# Can hydo's determinism model run natively on Apple Silicon? (Hypervisor.framework probe)

**Machine:** Mac14,10, Apple M2 Pro, macOS 26.4.1 (25E253). The host CPU reports `FEAT_ECV` (`sysctl hw.optional.arm.FEAT_ECV = 1`). Nested virtualization (guest EL2) is **not** supported on this machine (`hv_vm_config_get_el2_supported` = 0; Apple supports it on M3 and later).

**How:** `probe.c` + `guest.s` (this directory). `./build.sh` compiles them and ad-hoc signs them with the `com.apple.security.hypervisor` entitlement; `./probe <mode>` runs one probe in its own process. The guests are tiny hand-written AArch64 programs running at EL1 with the MMU off. Raw output of one full pass is in `results.txt`. No root was used and nothing was installed.

## Summary

The framework gives us almost everything hvx's model needs. Exits are deterministic, the physical counter and timer trap to us, interrupts can be injected at an exact instruction, snapshots are complete, and single-stepping is exact. **The one thing missing is a fast, deterministic way to stop a guest that makes no exits.** The guest PMU traps (there is nothing to count with), and the host-side counters need root (untested). So the M16-style "exit-count clock" is possible, but the M33 PMU branch clock is not. A guest kernel must also be set up to use the physical counter, because stock arm64 Linux uses the untrappable virtual counter.

| # | Capability | Result | Evidence |
|---|---|---|---|
| 1 | VM lifecycle, HVC and MMIO exits | **Works** | ~1.3 M exits/s (0.76 µs/exit). A store and a load to an unmapped IPA exit as data aborts (EC 0x24) with the IPA, the register number (ISS.SRT) and the direction, so an MMIO decoder needs no instruction fetch. |
| 1b | Host interrupts while the guest runs | **Invisible (good)** | A 3·10⁹-iteration loop with no exits ran to completion with 0 early returns from `hv_vcpu_run`, 3 times. Userspace only sees exits the guest causes, plus `hv_vcpus_exit` if we call it. |
| 2a | Guest read of `CNTPCT_EL0` (physical counter) | **Traps** | Every read exits with EC 0x18, ISS 0x32f801 (op2=1). The host chooses the value, like hvx's RDTSC exiting. |
| 2b | Guest read of `CNTVCT_EL0` (virtual counter) | **Does not trap** | No exit. The header defines `CNTVCT_EL0 = mach_absolute_time() - vtimer_offset`, and the guest sees `FEAT_ECV = 0` in `ID_AA64MMFR0_EL1`, so there is no way to make it trap without guest EL2. |
| 2c | Driving `CNTVCT` deterministically through the offset | **Does not work** | Setting the offset before each entry so the counter should read exactly `5000 + 1000k`: the guest saw the target plus 10–78 ticks (24 MHz, so 0.4–3 µs), and only 69–1071 of 2000 reads matched between two runs. |
| 2d | Physical timer registers (`CNTP_CVAL/CTL/TVAL`) | **All trap** | Writes to CVAL and CTL and reads of TVAL and CTL exit with EC 0x18. We can emulate the whole physical timer on our own clock. |
| 2e | Virtual timer (`CNTV_*`) | **Real time, not trapped** | An expired CNTV gives `HV_EXIT_REASON_VTIMER_ACTIVATED`, but it fires on host time. A deterministic guest must not use it. |
| 3a | Guest PMU (`PMCR_EL0`, `PMCCNTR_EL0`) | **Traps, nothing to count** | Both reads exit with EC 0x18. The guest sees `PMUVer = 0`. The header says accesses trap when EL2 is disabled; with EL2 enabled (M3+ only) the framework *emulates* the PMU, and whether those emulated counters count guest instructions exactly is unknown. |
| 3b | Host-side counters (kperf/kpc) counting the vCPU thread | **Not tested** | Configuring kpc needs root, which I did not have (`sudo` asks for a password). Even if it works, it would count per thread with an overflow interrupt of unknown skid, so it is not an exact landing point. |
| 4 | Single-step (software step trapped to the host) | **Works, exact** | `hv_vcpu_set_trap_debug_exceptions(true)` + `MDSCR_EL1.SS|KDE` + `PSTATE.SS`: each instruction exits (EC 0x32). A loop of exactly 1 + 100·2 instructions counted **201 and 201**. Rate ≈ **420 k steps/s**. |
| 5a | Interrupt injection at an exact instruction | **Works** | Step k instructions, then `hv_vcpu_set_pending_interrupt(IRQ)`. The IRQ is taken at that instruction: for k = 20, 21, 22, 23 the interrupted PC and the guest's counter register are identical in two runs and move by one instruction per k. |
| 5b | GIC | **Available** | `hv_gic_create` succeeds (macOS 15+ API; distributor 64 KiB, redistributor region 32 MiB). The GIC has state save/restore calls (`hv_gic_state_create`/`get_data`, `hv_gic_set_state`). Not exercised further. |
| 6 | Snapshot / restore | **Works, bit-identical** | GPRs, PC, CPSR, FPCR/FPSR, all 32 Q registers and 45 of 45 probed EL1/EL0 system registers are readable and writable. A full run, a snapshot taken mid-run then finished, and a restore into a **new** vCPU then finished all give fingerprint `b77d1f06f35361be` (registers + 1 MiB RAM). |
| 7a | Hardware random number (`RNDR`) | **Absent (good)** | The guest sees `ID_AA64ISAR0_EL1` RNDR = 0, and executing `RNDR` is UNDEFINED (`ESR_EL1` EC 0). `hv_vcpu_set_sys_reg` on that ID register returns success but the guest still reads the original value. |
| 7b | `WFI` | **Traps** | EC 0x01. An idle guest gives us a deterministic exit, so we can jump the clock to the next deadline as hvx does. |
| 7c | `WFE`, `YIELD` | **Do not trap** | `sevl; wfe; wfe; yield` ran with no exit. There is no equivalent of hvx's PAUSE exiting, so spin-waits stay invisible. |
| 7d | Other ID values | Fixed | Guest `MIDR_EL1 = 0x610f0000` (Apple), the `ID_AA64*` registers are constant across runs. |

## Which hydo clocks are possible

- **Exit-count clock (hvx's default, M4/M16): yes, with a configured guest kernel.** Time comes only from trapped reads of `CNTPCT_EL0`, a timer we emulate through the trapped `CNTP_*` registers, and interrupts injected only at exits, which are deterministic because host interrupts never surface. Stock arm64 Linux running at EL1 without hyp mode chooses the **virtual** timer and the `CNTVCT` counter (`drivers/clocksource/arm_arch_timer.c` in v6.6: `arch_timer_select_ppi` returns `ARCH_TIMER_VIRT_PPI` when hyp mode is unavailable, and `arch_counter_register` then uses `arch_counter_get_cntvct`). So the guest kernel needs a small patch: use the physical timer and counter, turn off the vDSO counter path, and clear EL0 access to `CNTVCT`. The kernel already has an EL0-counter trap-and-emulate path for errata (`arch_timer_this_cpu_has_cntvct_wa`), which can be reused so that user programs reading the counter directly still get our deterministic value. This patch is not written or tested here.
- **Single-step instruction clock (hvx's MTF clock, M5): yes, exact.** 420 k instructions/s is far too slow for a Linux boot (~10⁸–10⁹ instructions), but fine for landing on an exact instruction over short distances, and for hand-written guests.
- **PMU branch clock (M33): no.** The guest PMU traps with nothing behind it on this machine. M3+ with EL2 enabled emulates a PMU, but whether its counts are exact and guest-only is unknown and would need an M3 machine to probe. Host kpc needs root and is untested.

## The biggest blocker

**A guest that spins without exits cannot be interrupted deterministically.** In hvx the PMU branch clock delivers ticks at exact instructions even into an exit-free loop (M33); without it, M33 found real livelocks: a JIT-compiled JVM thread polling the clock never exited, virtual time froze, and the guest hung. On Apple Silicon with a single vCPU:
- spin-waits do not exit (`WFE`/`YIELD` are invisible), and
- an asynchronous kick (`hv_vcpus_exit`) lands at a host-time-dependent instruction, and there is no counter to land precisely afterwards.

**Workarounds, none proven:**
1. Make every clock read an exit (the counter patch above, like hvx's `tsc=reliable` cure in M33). This fixes clock-polling loops, which was the M33 case, but not a pure spin on a memory flag.
2. Guest-kernel-side preemption for the remaining cases is not available without counting.
3. On M3+ with guest EL2 enabled, probe the framework's emulated PMU (and `CNTHCTL_EL2`, which the API exposes there) for an exact guest instruction count with overflow. This is the one hardware route left; it needs an M3/M4 Mac.
4. Fall back to single-step only around a known spin (hybrid run: free-run, then step). This needs a deterministic trigger to switch to stepping, which is the same missing piece.

## What a deterministic Linux boot on HVF would need

- an arm64 kernel (our own build) patched for the physical counter and timer, as above, with `nokaslr`-style fixed layout, booted at EL1 with a device tree we generate;
- a GIC through `hv_gic_*` (macOS 15+), with its state in the snapshot, or our own minimal interrupt controller emulated on trapped MMIO;
- virtio-mmio devices (console, block, network) emulated on MMIO data-abort exits, which carry everything a decoder needs;
- an emulated physical timer on the exit-count clock, `WFI` exits jumping time to the next deadline, and a seeded random seed for the guest (no `RNDR`);
- a liveness answer for exit-free spins (the blocker above); until then, workloads that spin in user space without syscalls or clock reads may hang, as in M33.

## Not covered

- kpc host counters (needs root).
- EL0 (user mode) behaviour of the trapped `CNTPCT` read, and the guest-kernel counter patch.
- M3/M4 machines (guest EL2, emulated PMU, guest `CNTHCTL_EL2`).
- Exit latency comparison with hvx on the bare-metal machine, which was not measured here.
