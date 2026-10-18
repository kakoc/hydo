# Exit-free spins on the M2: resolving them deterministically

## The problem

The Mac VMM (`hvx-mac/hvm.c`, CLAUDE.md M48) keeps a guest deterministic by acting only at
VM exits. Its clock advances by a fixed step per exit, and interrupts are delivered only at
an exit. The M2 gives the guest no instruction counter, so there is no way to stop it after
an exact number of instructions (`PROBE.md`).

A user-space loop that spins on a memory flag never exits:

```c
while (flag) ;
```

The thread that would clear the flag only runs after a timer interrupt preempts the
spinner, and that interrupt needs an exit. So virtual time stops and the guest hangs. A
host timer could force an exit with `hv_vcpus_exit`, but at a host-timed instruction that
differs between runs. `/bin/spinwait` in `guest/initramfs-spin.cpio.gz` shows the problem
on real Linux 6.6.35.

## The idea: a provable loop is idle

On one vCPU with no exits, nothing outside the guest can change guest memory. A loop that
returns to exactly the same register state without changing memory will therefore repeat
forever, and every pass is the same state. If the pending interrupt is delivered at one
canonical point of the loop, chosen from the loop's own states, the guest ends up in the
same state however late the loop was noticed. The host timer becomes a hint: it can only
decide when we look, never what happens.

`hvm-spin.c` implements this.
- **Kick:** after `HVM_SPIN_MS` (default 50 ms) of host time without an exit, a host
  thread calls `hv_vcpus_exit`. The vCPU stops with its state intact, which changes
  nothing for the guest.
- **Observe:** the VMM sets hardware breakpoint 0 at the kick PC and write-protects all of
  guest RAM at stage 2 (`hv_vm_protect`). On each breakpoint hit, it compares the full
  register state with the previous hit. The state covers X0–X30, PC, PSTATE (minus the
  software-step bit), SP_EL0/1, ELR, SPSR, FPCR, FPSR, the TPIDR registers and Q0–Q31. It
  also compares the pages written in between with their old bytes.
- **Prove:** if one pass returns to the same state and leaves memory as it was, the VMM
  single-steps one more full period (exactly, one instruction per step) and lists its
  states. It gives up on an exclusive store, `WFE`, `SEV` or `SEVL`.
- **Deliver:** it steps to the canonical state, the one with the smallest hash among
  states where IRQs are unmasked. Then it jumps virtual time to the timer deadline, as an
  idle `WFI` does, and resumes; the interrupt is taken at the next entry.
- **Otherwise:** if anything else happens (a real exit, no repeat within 64 hits, an
  unknown instruction), the guest is left exactly as free-running would leave it, and the
  kick interval doubles. After `HVM_SPIN_SECS` with no proof, the run is marked invalid
  instead of being fingerprinted.

**Cost:** a breakpoint hit costs a few microseconds (see the side findings), and between
hits the guest runs at full speed. A resolved spin costs one period of single steps. The
normal boot never reaches the observer: 0 kicks, 0 checks.

### Two determinism bugs found and fixed while building it

1. **The IRQ line at the kick point.** After a kick that changed nothing, the run loop
   recomputed the interrupt line. If the guest had unmasked IRQs in the meantime, the
   interrupt was delivered at the host-timed kick point. Fix: after a kick, a write-protect
   fault or a step-over, resume with the line the guest had at its last real entry.
2. **`MDSCR_EL1` save and restore.** The observer saved the guest's `MDSCR_EL1` at arm time
   and wrote it back at disarm, clobbering any write the guest made in between. Fix: while
   observing, guest accesses to `MDSCR_EL1` and the debug registers trap (EC 0x18); the
   VMM emulates them against the guest's own saved values (`ob_debug_reg`).

## Results (M2 Pro, macOS 26.4.1, Linux 6.6.35 guest)

Each variant (`spinvar.c`) is a static Linux program that arms a 100 ms interval timer
and busy-waits for its `SIGALRM` handler to clear a flag. The guest's own tick
interrupts are resolved along the way: 27 resolutions per run. The fingerprint covers
the console, all vCPU registers, the machine state and all 256 MiB of guest RAM. Each
cell below is from `out/variants.txt` (`hvm-spin`) and `out/rewind.txt` (`hvm-spin-count`
with `HVM_SPIN_REWIND=1`). The kick interval is the delay before the observer looks.

| Variant | Loop body | hvm-spin, kicks at 1/5/20/50/100 ms | hvm-spin-count rewind mode, kicks at 1/2/5/10/20/50/100 ms |
|---|---|---|---|
| `spin` (`/bin/spinwait`) | load | all `3a36c4ee97b37d37` | all `3a36c4ee97b37d37` |
| `load` | `ldr; cbnz` | all `dc4c8b73f35dfdf6` | all `dc4c8b73f35dfdf6` |
| `ldar` | load-acquire | all `3531e6205f0dd0b7` | all `3531e6205f0dd0b7` |
| `yield` | `ldr; yield; cbnz` | all `5d15cf8e746af740` | all `5d15cf8e746af740` |
| `samestore` | stores the same value each pass | all `fc2eb84419ca21a3` | all `fc2eb84419ca21a3` |
| `ldxr` | exclusive load `ldaxr` | all `84ac63b0d1f121d3` | all `84ac63b0d1f121d3` |
| `count` | `ldr; add x4,x4,#1; cbnz` | **invalid**, a different fingerprint per timing | **all `d8c2bb710b72b0db`**, 27 counter loops resolved per run |

- **Normal boot:** `81711ada76a4a236`, the M48 baseline, with `hvm-spin` and with
  `hvm-spin-count` in rewind mode, including 5 of 5 runs under load.
- **Branch from a snapshot:** across the counter loop (snapshot at `hydo-spin: start`,
  continue vs restore), `IDENTICAL`, `d8c2bb710b72b0db` (`out/load.txt`). The previous
  agent reported branch-identical results for spin, samestore and ldxr with `hvm-spin`.
- **Host load** (12 `yes` processes on the 12 cores), counter variant in rewind mode: 59 of
  60 runs gave `d8c2bb710b72b0db` (`out/load.txt`, `out/load30.txt`, `out/loadfresh.txt`).
  - **The one exception** was the first run after starting the load (kick 8 ms). It
    reported itself invalid, the run's own SPIN or abort check, and gave `7605756a3059e05d`.
    Its diagnostic was not captured, and 20 more runs at 8 ms, including 12 that started
    the load just before the run, did not reproduce it.
  - **Not a silent error:** it was flagged invalid, not fingerprinted as a result.

## Counter loops (`hvm-spin-count.c`, `HVM_SPIN_REWIND=1`)

`while (flag) n++;` never returns to the same state: n grows. The state at the kick is
S_entry + k·D, and k, the number of passes since the loop began, depends on when the kick
came. Picking a point "in the future" (for example, the next n that is a multiple of
2^30) is not sound: whether the kick comes before or after the first such point is
host-timed. A sound rule needs (1) a proof that the loop is infinite, and (2) a delivery
point counted from the loop's start, which needs the state at the start.

**(1) Proof.**
- **Two passes agree:** the full register state changes only in some X registers, by the
  same constant D per pass. These are the induction registers.
- **One stepped period checks out.** The VMM single-steps one period and decodes every
  instruction (`insn_reads`). It requires that:
  - no instruction reads an induction register, except the `add/sub Xi, Xi, #imm` that
    updates it;
  - memory is unchanged by the period;
  - the period ends at S + D exactly.
- **Why that proves it:** the branch conditions, load addresses and stored values then do
  not depend on the induction registers. Every later pass therefore follows the same path,
  reads the same memory and writes the same values, so the loop never ends without an
  interrupt.
- **Consequence:** every run that reaches the loop is eventually kicked. That is what
  makes the rule's outcome independent of the timing, as with the exact cycles.
- **Decoded instructions:** NOP, YIELD, B, B.cond, CBZ/CBNZ, TBZ/TBNZ, LDR/STR (unsigned
  immediate), LDAR, ADD/SUB immediate, ADDS/SUBS immediate or register. Anything else
  fails the proof, and the run stays invalid, as before.

**(2) Delivery.**
- **Checkpoint:** at every real exit the VMM saves the registers and the machine state.
  It write-protects the pages the guest wrote since the previous exit, so the first write
  to a page faults at stage 2, the page's bytes from that exit are kept, and the page is
  unprotected.
- **Rewind:** once a counter loop is proven, the VMM copies those bytes back and restores
  the checkpoint. That is the state at the last real exit E.
- **Replay and deliver:** it re-executes with hardware breakpoint 0 at the loop's canonical
  PC p*, the smallest PC of the period with IRQs unmasked. At the first arrival there with
  IRQs unmasked, it jumps time to the deadline and delivers. Execution from E is
  deterministic and the breakpoint is exact (no skid, see below), so the delivery state is
  a function of the guest alone.
- **Safety check:** a real exit during the replay would mean nondeterminism, and the VMM
  stops with an error. It never happened.

**Cost:** the checkpoint runs at every exit. The normal boot took 1.25 s instead of
0.06 s: 15,483 checkpoints and 27,856 copy-on-write faults. That is why it is opt-in
(`HVM_SPIN_REWIND=1`). Without it, `hvm-spin-count` behaves exactly like `hvm-spin`.

**Semantics:** the loop now runs zero passes between the last exit and each delivered
tick. A counter loop that measures elapsed time in passes therefore sees a fixed,
deterministic number of passes per timer interrupt. That is a legal behaviour, but a
different one from real hardware.

**Not covered:**
- loops with more complex induction (loads from addresses that move each pass, a nested
  loop, more than about 4,096 instructions per period);
- loops that end on their own but run longer than the kick interval. Those are not
  infinite, so no rule applies. They resume and finish, and the kick interval backs off.

## Side findings (`bpprobe.c`, `out/bpprobe.txt`)

- **Breakpoints are exact.** 200,000 hits for 200,000 loop iterations, with no wrong PC or
  count (no skid). 211,919 hits/s, 4.72 µs per hit including the step-over, measured
  today; the previous agent measured 306,000/s on a quieter machine.
- **Watchpoints.** A load watchpoint hits every time (EC 0x34), 347,135 hits/s today
  (previously 407,000/s).
- **Write protection.** A store to a write-protected page exits with EC 0x24, WnR = 1 and
  DFSC 0x07 (permission fault). After unprotecting, the store completes when the vCPU
  resumes. The probe's own timing of protect plus unprotect of 256 MiB failed (a second
  `hv_vm_create` in the same process), so that number is missing.
- **`hv_vcpus_exit` is clean.** The vCPU stops mid-loop with its state intact, and
  resuming finishes the loop normally. Issued before a run, the next run returns at once
  with no progress, and the run after that proceeds.
- **Private API, UNDOCUMENTED and liable to break in any macOS update:**
  - `_hv_vcpu_get_control_field` / `_hv_vcpu_set_control_field` are exported by
    Hypervisor.framework. Field 0 reads `0x300001c0000`, which looks like the guest's
    HCR_EL2.
  - Setting bit 14 (TWE) makes `WFE` trap: EC 0x01, ISS bit 0 = 1. `YIELD` still does not
    trap.
  - Other private symbols found: `_hv_capability` (ids 2–22 return values),
    `_hv_vcpu_config_get/set_fgt_enabled`, `_hv_vcpu_get_context`.
  - None of these is used by `hvm.c`.

## Host PMU (`pmuprobe.c`, `README-pmu.md`)

The next probe asks whether Apple's host PMU, programmed through the private kperf
interface, counts the guest's instructions or branches exactly. Without root it confirms
that the frameworks, functions and events are present. The measurement needs
`sudo ./pmuprobe` and has not been run. Even a positive answer would not interrupt a
spinning guest by itself, because overflow interrupts are not delivered to a user
program (`README-pmu.md`).

## Open

- **A rare divergence under load.** The previous agent saw 1 run in 300 differ under load
  with `hvm-spin`, by 3 bytes of stale kernel stack, always giving `3820c07d53076f1e`.
  That is owned by the `mac-divergence` workstream and not chased here. The single invalid
  rewind-mode run above may or may not be related.
- **Whether real software hits counter loops at all.** Redis and PostgreSQL never tripped
  the watchdog in more than 70 boots (`hvx-mac/services/README.md`).
- **The host-PMU measurement** (root).
- **Moving into `hvm.c`.** The exact-cycle resolver and the counter-loop rewind still live
  in these side files (`hvm-spin.c`, `hvm-spin-count.c`); they are not in `hvm.c` or
  `hvmx` yet.

## Files

- `hvm-spin.c`: the exact-cycle resolver (the M48 `hvm.c` plus the observer).
- `hvm-spin-count.c`: `hvm-spin.c` plus the counter-loop rule, behind `HVM_SPIN_REWIND=1`.
  With the variable unset it behaves the same.
- `hvm-spin-v1-rollback.c`: the first version, superseded.
- `bpprobe.c`, `sguest.s`: breakpoint, watchpoint, write-protect, kick and private-API
  probes on tiny EL1 guests.
- `spinvar.c`: the spin variants. `rootfs/init*` are the guest init scripts; `guest/` holds
  the kernel and initramfs images (not committed, rebuilt from `hvx-mac/linux`).
- `pmuprobe.c`, `README-pmu.md`: the host-PMU probe.
- `out/`: the scripts and results quoted above.

Build:

```sh
/usr/bin/clang -O2 -Wall -arch arm64 -o hvm-spin-count hvm-spin-count.c -framework Hypervisor
codesign --force --entitlements ../hv.entitlements -s - hvm-spin-count
HVM_SPIN_REWIND=1 ./hvm-spin-count print guest/Image guest/initramfs-count.cpio.gz "hydo-boot-marker: launching shell"
```
