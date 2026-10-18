# Deterministic arm64 Linux on Apple Silicon (Hypervisor.framework)

**Result:** a real Linux 6.6.35 arm64 kernel boots to a busybox shell under `hvm`, a small VMM on Apple's Hypervisor.framework, on an M2 Pro (macOS 26.4.1). The run is **bit-identical** every time:
- 20 of 20 fresh boots give the same fingerprint;
- 15 of 15 boots with all 12 host cores saturated by `yes` give the same fingerprint as unloaded boots;
- branch-from-snapshot at three points (kernel init, first userspace line, mid-workload) reproduces the fresh boot exactly, including a restore into a **new** vCPU after RAM was overwritten;
- 1 GiB and 4 GiB guests are deterministic too.

The fingerprint covers the console, every readable guest register (GPRs, PC, CPSR, FPCR/FPSR, all 32 SIMD registers, 41 EL0/EL1 system registers), the VMM's device and clock state, and all guest RAM. This is the hvx M14–M16 result (deterministic Linux boot + branch), on a Mac.

It needed **one small guest-kernel change** (a Kconfig option, below) and no root, nothing installed on the Mac.

## Files

| File | What |
|---|---|
| `hvm.c` | the VMM (~680 lines of C): RAM, generated device tree, GICv2 + 16550 UART + PSCI emulation, exit-count clock, snapshot/branch, fingerprint, spin watchdog |
| `build.sh` | builds `probe` and `hvm` and ad-hoc signs them with the hypervisor entitlement |
| `linux/build-guest.sh` | builds the guest on a Linux box with nix: kernel `Image`, `initramfs.cpio.gz`, `initramfs-spin.cpio.gz`. Byte-reproducible (checked with a clean rebuild in a fresh directory) |
| `linux/apply-cntpct.py`, `linux/force-cntpct.patch` | the kernel change (script, and the resulting diff for reading) |
| `linux/hvm.config` | the kernel configuration, applied on top of `allnoconfig` |
| `linux/init`, `linux/cntvct.c`, `linux/spinwait.c` | the initramfs `/init`; a user program that reads the virtual counter directly; the spin-wait demonstration |

Pins: kernel source `/nix/store/czg3kwyz7v5i192myvg62i66n83s0940-linux-6.6.35.tar.xz` (nixpkgs `linux_6_6.src`), busybox `/nix/store/zbc5fx2yszji1a4aw2ff46m0p99c6yxb-busybox-static-aarch64-unknown-linux-musl-1.36.1`, cross gcc 13.2.0 from `pkgsCross.aarch64-multiplatform`. Outputs: `Image` sha256 `301883d2ffee760a60d0064a211edf8831b8c99d99e5851c97c9afc5366783d8`, `initramfs.cpio.gz` `cb1fdaa01568407c7fceb9607e9502be82e3c3b5ec7fbb4c50a5e4599e222d8d`, `initramfs-spin.cpio.gz` `fd5032c30c9ac4a1ae8182655b73c731f4e738f401f1fe41b6f7591a676324b3`.

## How to run

```sh
# on the build machine (Linux with Nix), in a scratch directory:
WORK=$PWD sh <repo>/hvx-mac/linux/build-guest.sh        # -> out/Image, out/initramfs*.cpio.gz
# on the Mac, in hvx-mac/ (copy out/* to guest/ first):
sh build.sh
./hvm run    guest/Image guest/initramfs.cpio.gz "hydo-boot-marker: launching shell"   # console + fingerprint
./hvm print  guest/Image guest/initramfs.cpio.gz "hydo-boot-marker: launching shell"   # fingerprint only
./hvm branch guest/Image guest/initramfs.cpio.gz "Unpacking initramfs" "hydo-boot-marker: launching shell"
```
Environment: `HVM_MEM_MB` (default 256), `HVM_STEP` (virtual ticks per exit, default 64), `HVM_ECHO=1`, `HVM_MAX_EXITS`, `HVM_SPIN_SECS` (give up on an exit-free spin after this much host time, default 10), `HVM_SPIN_MS` (exit-free host time before the spin observer looks, default 50), `HVM_SPIN_OBSERVE=0` (no observer), `HVM_SPIN_REWIND=1` (also resolve counter loops; slower), `HVM_SPLOG=<file>`, `HVM_BOOTARGS`, `HVM_TRACE=<file>` (one line per exit, plus the interrupted PC at every interrupt acknowledge), `HVM_DUMP=<prefix>` (registers and RAM at the stop), `HVM_TRACE_SYSREG=1`.

## Reference numbers (256 MiB guest)

Boot to the shell marker: **15,483 exits, 0.04 s of host time**, 1.04 s of virtual time (the `sleep 1` in `/init` is a clock jump at an idle WFI). Of the exits, 13,820 are MMIO (mostly the UART: about two per console byte), 1,098 trapped counter reads, 18 WFI, 2 HVC (PSCI), 2 unknown system registers (the OS lock registers, read as zero). 24 entries had the IRQ line up, and 450 instructions were single-stepped in 10 short stretches. The whole `hvm print` process takes 0.38 s, most of it hashing 256 MiB, and peaks at 366 MB of memory.

Fingerprint of the boot to `hydo-boot-marker: launching shell`, 256 MiB, `HVM_STEP=64`: **`81711ada76a4a236`** (console `2a96cf67c42848e2`, registers `2c8e7cfb0e47c363`, machine `76915b28260f515e`, RAM `6f18ed0cb6a235da`). 1 GiB: `4ffe2f2cc064708e`; 4 GiB: `ace950b0772ecdd5`. A user program reading `CNTVCT_EL0` directly sees `0xe1f00, 0xe27c0, 0xe2f80, 0xe3740`: the deterministic counter, through the kernel's trap handler.

## How determinism is obtained

1. **Clock = exit count.** Every exit advances virtual time by `HVM_STEP` ticks of the 24 MHz counter frequency the guest reads from `CNTFRQ_EL0`. The guest sees time only through trapped reads of `CNTPCT_EL0` and the trapped physical-timer registers `CNTP_CTL/CVAL/TVAL_EL0`, which `hvm` emulates. Host interrupts never surface as exits (probe 1b), so every exit happens at a deterministic instruction, and time is a pure function of the guest's own execution. An idle WFI jumps time to the timer deadline, like hvx's HLT rule.
2. **The guest uses only the physical counter.** The M1/M2 cannot trap the virtual counter `CNTVCT_EL0`, and stock arm64 Linux at EL1 uses it. The device tree lists only the physical timer interrupts, which makes Linux pick the physical **timer**; the **counter** needs the kernel change.
3. **Interrupts only where the guest takes them at once.** `hvm` emulates a GICv2 (distributor and CPU interface over MMIO) and raises the vCPU's IRQ line before an entry only if the GIC has a deliverable interrupt **and the guest's PSTATE.I is clear at that entry**, so the interrupt is taken at the entry instruction. An idle WFI runs with interrupts masked (arm64 `cpu_do_idle`). There `hvm` single-steps the guest, which is exact, up to the instruction that unmasks, and raises the line there; that took 10 stretches of 45 instructions on average in this boot.
4. **Devices are state machines driven only by exits.** The GIC, the UART and the timer live in one struct that the snapshot copies. The console bytes are part of the fingerprint.

## The walls, in the order they were hit

1. **The virtual counter leaked into the random-number generator before the timer driver ran.** Two boots matched in console and in every exit, but user-space addresses (ASLR) differed. The CRNG input pool already differed before the first exit: `add_device_randomness()` mixes `get_cycles()` while the device tree is parsed, long before early parameters are read, and `arch_timer_read_counter` defaults to the virtual counter. A boot parameter was therefore too late, so the change became a Kconfig option that sets the default reader at build time.
2. **Interrupts made pending while the guest had them masked were delivered at a host-timing-dependent instruction.** With the change above, three boots matched, then 12 boots gave 9 different RAM hashes, while the exit trace (index, syndrome, PC, CPSR, line state) was identical in every run. Logging the interrupted PC at each interrupt acknowledge showed the timer interrupt landing in `put_pid` in one run and in `wake_up_new_task` in another, after an entry where the line was up with PSTATE.I set. The framework evidently does not deliver it at the guest's unmask instruction. Fix: rule 3 above.
3. **Single-stepping a load-exclusive/store-exclusive loop never completes.** Each step clears the exclusive monitor, so `stlxr` always fails. The first fix for wall 2 hung in `ct_kernel_enter_state`'s `ldxr/stxr` loop. `allnoconfig` had turned off LSE atomics; with `CONFIG_ARM64_USE_LSE_ATOMICS=y` atomics are single instructions. `hvm` now stops with a clear message if a step ever lands on an exclusive load.
4. (Test-program issue, not hvm) Linux 6.6 cannot load an ELF segment with no file bytes (a `.bss`-only segment); 6.7 fixed this. The spin test keeps its flag in `.data`.

## The kernel change (`CONFIG_ARM_ARCH_TIMER_FORCE_CNTPCT`)

About 20 lines in `drivers/clocksource/arm_arch_timer.c` plus the Kconfig entry (`linux/force-cntpct.patch`). With the option set:
- the default counter reader is `arch_counter_get_cntpct` from the first instruction;
- the clocksource and `sched_clock` use the physical counter even without hyp mode;
- `CNTKCTL_EL1.EL0VCTEN` stays clear, so a user-space `mrs cntvct_el0` traps to the existing `cntvct_read_handler`, which returns the physical counter;
- the vDSO counter path is off, so `clock_gettime` is a system call.

With the option off the code is unchanged. The change is needed only because the M1/M2 lacks guest-visible `FEAT_ECV`; an M3/M4 with guest EL2 may make it unnecessary (`PROBE.md`). Upstreaming it is plausible, because it reuses the kernel's existing erratum path for trapping `CNTVCT`.

## Limits and what remains

- **A guest that loops without exits stops the clock** -- now resolved in most cases by `spin_observer.c` (see `spin/RESEARCH.md`, "In the main engine"); the rest of this item describes the bare watchdog (`HVM_SPIN_OBSERVE=0`). This is the PROBE.md blocker, now shown on real Linux. `/bin/spinwait` arms a 100 ms timer and spins in user space waiting for the signal. No exit happens, so the timer interrupt is never delivered and virtual time stays at 0.039 s. `hvm`'s watchdog reports it after `HVM_SPIN_SECS` of host time: `SPIN: no VM exit for 3 s ... pc 0x4001c0 cpsr 0x1000`. It then stops the run as invalid instead of hanging or producing a host-timed result. Code that polls the clock is fine, because every counter read is an exit. Pure memory-flag spins in user space, such as busy-wait locks or some JIT safepoint polls, are not. hvx solved this with the PMU branch clock (M33); there is no counter on the M2 to do the same.
- **Interrupt latency is coarser than on hardware.** An interrupt waits for the next exit where the guest has interrupts unmasked. Compute-bound code without exits delays the tick. This is deterministic but changes the guest's timing.
- **One vCPU.** Several vCPUs would need deterministic interleaving, which hvx does with an instruction quantum (M19); here there is no counter to define a quantum.
- **Devices for real services:** virtio-mmio block and network devices (the MMIO exits already carry everything a decoder needs), and a fault-injection layer like hvx's M22/M24. The MMIO hole is below RAM, at `0x08000000`–`0x09000fff`, so RAM up to 4 GiB already works, and more is a constant change.
- **Snapshot cost:** the branch copies all RAM in memory (fine at these sizes); a file-backed snapshot like hvx M32 would be the next step for big guests.
- **What is not captured** in the vCPU snapshot: registers the framework does not expose. The 41 captured registers were enough for every branch to match, but a guest using debug or PMU registers could need more.
