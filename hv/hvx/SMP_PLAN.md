# Several virtual CPUs per guest: review and plan (2026-10-21)

Written from three read-only investigations: the existing code (`hv/hvx/hvx.c`, `hvx_abi.h`,
`hvx_boot.h`, `hvx_smp_test.c`, `hvx_stack_verify.c`, `hvx_explore.c`), a design for
repeatable switching, and published practice (the prior art below). Nothing
was changed or run. Line numbers refer to `main` on 2026-10-21.

## Why do it

Today every race hydo finds needs the guest's own scheduler to switch threads, which happens
only at timer interrupts. That is why a race window of plain computation is so hard to hit
(Milestone 43: hits are isolated and depend on the guest's scheduler accounting; Milestone 53:
on the Mac such a window is never hit). With several virtual CPUs that run in turns of an
exact number of guest branches, a turn boundary can land anywhere, including in the middle of
plain computation, without the guest's scheduler deciding anything. rr (the record-and-replay
debugger) and Meta's Hermit use the same idea for threads in one process.

**Limit.** The CPUs still run one at a time, so execution is sequentially consistent: bugs that
need x86 store-buffer reordering (a missing `mfence` in lock-free code) stay out of reach.
Simulating that would mean trapping every store (CoreDet's approach, up to 8x slower). Say so
in the write-up and point lock-free code to model checkers such as Nidhugg or GenMC.

## What exists (Milestone 19 and later)

- **Per vCPU** (`struct hvx_vcpu`, hvx.c:729-819): VMCS, registers, sregs, MSRs, x2APIC with
  a TSC-deadline timer, virtual TSC, icount, the pending-interrupt slot and overflow set, the
  PMU clock state (`pmu_total`, `pmu_next`), cr2, re-injection state.
- **IPIs** between vCPUs work (`hvx_lapic_send_ipi`, hvx.c:2080); TLB-shootdown and
  call-function IPIs are plain fixed IPIs, so they would work too.
- **Scheduling** is userspace round-robin only (`HVX_SET_VCPU` + `HVX_RUNC`), with
  Monitor-Trap-Flag single steps in `hvx_smp_test.c`. Every switch rebuilds the VMCS.

## What is missing, and the flaws found

1. **Starting the other CPUs.** INIT/SIPI is dropped (hvx.c:2140); the MP table lists one CPU
   (hvx_boot.h:312); the module has no real-mode support (no "unrestricted guest" control).
2. **Every vCPU claims to be the boot CPU**: `APICBASE_DEFAULT` with the BSP bit is seeded for
   all vCPUs, and a guest write is forced back to it (hvx.c:195, 2101-2105).
3. **No shared clock.** Each vCPU has its own virtual TSC, its own PMU ticks and its own idle
   jump (hvx.c:4531, 5042-5048, 5119), so the CPUs' clocks drift apart, which Linux's TSC
   checks will see.
4. **Device completions mix two clocks**: `completion_due` comes from the kicking vCPU's
   icount (hvx.c:2785, 3697, 3839) but is compared with vCPU 0's (hvx.c:4594, 4634).
5. **All device interrupts go to vCPU 0** (hvx.c:4594-4681); the IOAPIC and MSI-X destinations
   are ignored (hvx.c:4160, 4223). Linux allocates interrupt vectors per CPU, so an interrupt
   meant for CPU 1 delivered to CPU 0 is a spurious "no irq handler for vector".
6. **Tick jitter and inserted ticks** depend only on the tick number (hvx.c:4255, 4316), so
   every vCPU would get the same pattern.
7. **Snapshots and fingerprints cover vCPU 0 only** in `hvx_explore` (`snap_take`,
   hvx_explore.c:314) and `hvx_stack_verify` (`fingerprint`, hvx_stack_verify.c:415). Cr2,
   re-injection state and the current vCPU are not in the snapshot struct.

## Plan

**Gate for every stage:** one-vCPU guests stay byte-identical, shown by `deploy-metal.sh`
ALL PASS with unchanged boot fingerprints and the two pinned explorer reproducers. All new
behaviour sits behind an opt-in flag plus `n_vcpus > 1`.

### Stage 1: switching between vCPUs, on hand-written guests

- **Switch in the kernel run loop**, not in userspace. One `HVX_RUN` drives every vCPU, and
  the scheduler state lives in `struct hvx_vm`. A switch costs a VMCLEAR/VMPTRLD plus the
  existing `setup_vmcs` rebuild (a few microseconds). The deciding reason is repeatability:
  turn boundaries must depend only on guest state, never on where an `HVX_RUN` returned (the
  stop-point problem of Milestones 27 and 43). In userspace every client (explorer, verify
  harnesses, loader) would need the same scheduler.
- **Exact turns:** reuse the PMU clock's landing (overflow early, then single steps, plus the
  REP-string hop) with target = min(next tick, end of turn). Switch only at a clean boundary:
  no STI/MOV-SS shadow and no half-delivered event.
- **One clock for the VM:** virtual TSC, tick grid, tick jitter and inserted ticks become
  functions of the total branches retired by all vCPUs. Device completions use the same clock.
  Serialized execution means the TSC only moves forward across CPUs.
- **Blocked vCPUs:** HLT ends the turn and the vCPU is skipped until an interrupt is posted
  to it. Time may jump forward only when every vCPU is halted, to the earliest event across
  all vCPUs and devices.
- **Spinning:** count PAUSE exits (`HVX_VM_PAUSE_EXIT` exists); K PAUSEs with no other exit
  in between end the turn. Do not use the hardware pause-loop exiting: its window is in host
  cycles, which is not deterministic. Spins without PAUSE still end at the turn limit.
- **Snapshot:** new `HVX_GET/SET_SCHED` ioctls for the scheduler state (current vCPU, branches
  left, turn number, halted set, shared clock); the existing per-vCPU snapshot gains cr2 and the
  re-injection state at the end of the struct. Fingerprints hash vCPUs 1..n-1 only when n > 1.
- **Small fixes:** clear the BSP bit on the other vCPUs; mix the vCPU index into the jitter and
  inserted-tick hashes, as 0 for vCPU 0.
- **Proof:**
  - a two-vCPU version of `hvx_pmu_test`: per-vCPU branch counts add up exactly, and the
    landing hash is identical;
  - a two-vCPU guest with `sti;hlt`, `mov ss` and long `rep movsb`, with the turn length
    swept from 1 to 2000: replay and branch-from-snapshot identical at every turn;
  - the race test with its two threads on two vCPUs and the tick period made huge, so no tick
    falls in the window. The lost update must be a pure function of (turn length, seed), and
    turns alone must hit a plain-computation window.

### Stage 2: real Linux with 2-4 vCPUs, then the stack

- **AP start-up.** Recommended: standard INIT then SIPI, as KVM does. It needs the
  "unrestricted guest" control and real-mode segment set-up in `setup_vmcs`, with the IA-32e
  entry control derived from the guest's EFER; the bare-metal machine's Xeon supports it. The alternative, the
  ACPI multiprocessor wake-up mailbox, keeps the CPUs in 64-bit mode but means turning ACPI
  on (the guests run `acpi=off`) and polling a mailbox in RAM, and its availability outside TDX
  in Linux 6.6 is unconfirmed.
- **MP table** entries per CPU, CPUID topology leaves (gated on n > 1).
- **Device interrupts routed by** the IOAPIC and MSI-X destination.
- **Faster lock waits (optional):** advertise KVM's `PV_UNHALT` (a waiter halts and the
  unlocker wakes it) and `PV_SCHED_YIELD` (a vCPU that sends an IPI yields to the target).
- **Proof:** an SMP Linux boot that is replay- and branch-identical, with no "TSC unstable"
  messages; then the four-service stack's checks with 2 vCPUs, and its slowdown measured.

### Stage 3: explorer

- Schedule tokens: a fixed turn length; seeded turn lengths, mixed as in rr's chaos mode (most
  turns long, about 10% at most 100 branches, about 10% at most 10,000); an explicit list for
  the shrinker; optionally PCT-style random vCPU priorities with a few change points, and a
  "starve one vCPU for a while" mode. Turn boundaries can also be placed at exits hvx already
  sees that mark communication (IPI writes, EOI, HLT, PAUSE, device access).
- Measure against tick jitter and inserted ticks on the race test, and on the stack.

## Cost

Wall time is at least the sum of all vCPUs' work, so at least N times slower than parallel
hardware for N vCPUs, plus one landing per turn (about 1,500 single steps, roughly 1-2 ms;
measure it). With turns of about 2^20 branches the landings add about 50-100%. Fine turns are
expensive, so the default should be long turns with a seeded minority of short ones.

## Top risks to repeatability, and the test for each

1. A switch at an unclean boundary: the turn-length sweep of Stage 1.
2. Per-vCPU clocks leaking through (device completions, TSC drift): disk I/O started by
   vCPU 1, replay and branch identical; an SMP Linux log with no TSC warnings.
3. Host timing reaching scheduler decisions (the PREEMPT/STALL exits, the safety cap, a
   throttle counter per vCPU): 20+ replays under host CPU load; explorer branch from S1 equal
   to a fresh boot.
4. An incomplete snapshot: snapshot at every turn boundary and at mid-turn returns; restore
   then finish equal to the straight run.
5. PMU accounting across switches (a stale overflow, reading the counter after
   reprogramming): the two-vCPU `hvx_pmu_test`.

## Prior art (links in the research report)

- **rr:** threads run one at a time, in turns of 2.5 M conditional branches; chaos mode uses
  short turns and random priorities.
- **QEMU:** record and replay forces single-threaded round-robin across vCPUs.
- **CoreDet, Kendo, Hermit:** deterministic multithreading.
- **PCT, PCTCP, POS:** probabilistic schedulers. Their change points sit at synchronization
  operations in real programs.
