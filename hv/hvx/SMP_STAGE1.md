# SMP Stage 1: several vCPUs in exact branch-counted turns (2026-10-22)

Stage 1 of `hv/hvx/SMP_PLAN.md`: hand-written guests only, no real Linux SMP, no explorer
work. Developed on the side-by-side module `hvxsmp.ko` (`/dev/hvxsmp`, built by
`hv/hvx/smp/build-hvxsmp.sh` in `~/hydo-smp` on the bare-metal machine). The main `hvx.ko` was
never touched. `hvxsmp` was unloaded at the end.

## What was built (`hv/hvx/hvx.c`, `hv/hvx/hvx_abi.h`)

- **Opt-in:** `HVX_VM_SMP_SCHED` (flags bit 5) together with `n_vcpus > 1`. It requires
  `HVX_VM_PMU_CLOCK` (CREATE_VM returns EINVAL otherwise). Every new path in the run loop
  sits behind `const bool sched = vm->sched_on`.
- **In-kernel round-robin:** one `HVX_RUN` / `HVX_RUNC` (no single-step) runs all vCPUs
  (`hvx_sched_run`). Each turn is one `hvx_run_vcpu` call, so the host CPU is released
  between turns and all save/restore code is the one-vCPU code. Turn length is `quantum`
  guest conditional branches, or with `seed != 0` a seeded length in [1, quantum].
- **Exact turn ends:** the PMU landing target is min(next tick, turn end). The turn ends at
  the first boundary after the target branch, only if there is no STI/MOV-SS shadow and no
  half-delivered event; otherwise the vCPU single-steps on (`deferred` counts this).
- **THROTTLE / max_insns stops** in scheduler mode also wait for a clean boundary, and the
  THROTTLE cadence (`guard`) is scheduler state, so stops never depend on where an ioctl
  returned. A turn switch adds 1 to the cadence. The safety cap no longer returns a
  THROTTLE at a host-timed point in scheduler mode (it forces a preempt re-entry instead).
- **One VM-wide clock:** virtual TSC, TSC step/mode, instruction clock, PMU branch total and
  next tick live in `vm->sched` and are copied into the running vCPU. Each vCPU's own
  branches are counted in `br_own`. SET_TSC / SET_SNAPSHOT / SET_XTICK / RUNC reset update it.
- **HLT** ends the turn; the vCPU is `halted` (RIP stays on the HLT) and is skipped until
  an interrupt is deliverable to it (IF=1, nothing in service, a due one-shot or, for
  vCPU 0, a device source); waking moves RIP past the HLT. When every vCPU is halted the
  clock jumps to the earliest LAPIC deadline / device completion / scheduled injection;
  with nothing armed HVX_RUN returns the HLT.
- **PAUSE:** with `pause_k > 0` and `HVX_VM_PAUSE_EXIT`, K consecutive PAUSE exits end the
  turn. Hardware pause-loop exiting is not used.
- **Ioctls** `HVX_GET_SCHED` / `HVX_SET_SCHED` (53/54, `struct hvx_sched`): policy, state,
  VM clock, statistics, and per vCPU: branches, turns, cr2, halted, re-injection state.
  `stop_turn` makes HVX_RUN return `HVX_EXIT_TURN` (0xfffffffb) after that turn. `struct
  hvx_snapshot` is unchanged (cr2 and re-injection ride in the scheduler struct instead).
- **Fixes:** vCPUs other than 0 no longer set the BSP bit in IA32_APIC_BASE. The vCPU index
  is mixed into the tick-jitter and inserted-tick hashes as `idx * salt` (0 for vCPU 0, so
  one-vCPU guests are unchanged; with the scheduler the single tick grid uses index 0).
- **Two bugs found by the tests and fixed** (scheduler mode only):
  1. An IPI to a vCPU that is not running was stamped with that vCPU's stale copy of the
     clock, whose staleness depended on where the last ioctl returned: a mid-turn stop
     changed the final `inject_at_icount`. Posted interrupts now use the running vCPU's clock.
  2. After an emulated-and-skipped instruction the STI/MOV-SS blocking bit stayed in the
     VMCS. After a stepped-over `sti; hlt` it covered the `cli`, so the pending interrupt
     was not delivered (1 interrupt per run instead of 24). The shadow is now cleared, as
     KVM does. (The one-vCPU path still has this stale-shadow behaviour; changing it would
     move existing fingerprints.)

## Tests (`hv/hvx/hvx_sched_*_test.c`, helpers in `hv/hvx/hvx_sched_util.h`)

All on `/dev/hvxsmp`, bare-metal machine, final module unless noted.

- **(a) `hvx_sched_pmu_test` — PASS (6.0 s).** N0 = 150,000,000 and N1 = 50,000,017
  branches: own counts exact, total 200,000,017, 0 overshoots, 3 runs identical. Turn
  1,000,003, no ticks: 200 turns, turn hash `97b3468c16d739fa`. With tick period 999,983:
  200 ticks on one grid, landing hashes `f841184e6fdde8b5` / `19337dfb08c526f9`, fp
  `95e6c9a02d5d584d`. Turn 10,007: 2,699 turns, 270 ticks, fp `05521a9492a3ce9f`. Seeded
  turns ≤ 50,000: 1,087 turns, fp `b544e114dc426e85`. Stop at turn 2,693 of 5,386, save,
  finish, restore, finish: both equal the straight run (`73a465516c4e0094`).
  (This run was on the module before the two fixes; not re-run after them.)
- **(b) `hvx_sched_sweep_test`** (MOV-SS shadow, PAUSE in a MOV-SS shadow, 700-byte
  `rep movsb`, IPIs, LAPIC timer, `sti; hlt`; ticks every 1,031 branches; pause_k = 1):
  - Version with a 37-branch inner loop, after both fixes — **PASS, 13 s:** Q = 1..2000,
    replay and branch at turn T/2 identical for all 2000 lengths (sweep hash
    `6a4b80718d606bdd`); branch at every turn boundary for Q = 13, 300, 1999 (159 checks)
    and at 159 mid-turn `max_insns` stops: 0 mismatches. 24 interrupts per run. But with only
    38 branches between turn-ending PAUSE/HLT, turns end by branch count only for Q < 38.
  - Version with a 2500-branch inner loop (so all Q ≤ 2000 end turns by count): Q = 1..30
    PASS (hash `2ca0e7dd2763f634`, Q=1 → 60,051 turns per run). **The full 1..2000 run did
    not finish:** at Q = 4 `open()` of the device failed with ENOMEM (see open problems).
  - `deferred` is 0 after fix 2: turns end right after a branch and emulated instructions
    now end their shadow, so the deferral path is only reachable through an interrupted
    event delivery. Before fix 2 it was exercised (24 deferrals per run) and Q = 1..20
    replay/branch passed then too.
- **(c) `hvx_sched_race_test` — PASS (3.3 s, before the fixes; not re-run after).** No
  ticks (period 2^40), window = 2 of 202 branches per sale, 400 seats. Serialized (Q = 2^30)
  loses 0. Lost sales by (Q; seeds 0..7): Q=50: 8 0 1 5 5 2 4 3; Q=203: 0 5 3 2 5 3 5 7;
  Q=1000: 0 5 2 3 0 0 9 4; Q=3000: 0 0 0 0 11 3 0 0; Q=20000: all 0. Every cell twice,
  identical; 21 of 40 cells lose sales; grid hash `8cb7c61a2393f44e`.
- **Unchanged paths on hvxsmp** (before the fixes): `hvx_pmu_test` PASS with land hash
  `86c4afcd0a799450`; `hvx_smp_test` MILESTONE 19 PASS with fp `d042bb01201c269d`.

## Follow-up (2026-10-22)

- `hvx_open` now allocates the VM struct with `kvzalloc` (freed with `kvfree`): about
  100 KiB, an order-5 block if contiguous, which failed on the fragmented host. No other
  behaviour change; nothing takes the physical address of the struct.
- On the final module (with both fixes and kvzalloc), test (a) PASS again, 6.6 s, same
  numbers (snapshot fp `73a465516c4e0094`, landing hashes unchanged); test (c) PASS again,
  3.3 s, same table and grid hash `8cb7c61a2393f44e`.
- Still NOT done: the long-loop sweep 1..2000 and the side-metal regression gate.

## NOT done / not verified

- **The regression gate `hv/hvx/race/side-metal.sh` (HVX_DEV=/dev/hvxsmp) was NOT run.** The
  one-vCPU byte-identity is argued from the code (every new path is behind `sched`, the
  refactored timer/device helpers are verbatim moves, index mixing adds 0 for vCPU 0), and
  two of its tests passed, but the 25 checks and both boot verifies are unverified.
  `side-metal.sh` now accepts `SIDE_TMP` for its binaries (default /tmp unchanged).
- Tests (a) and (c) were not re-run on the final module; (b) at Q = 1..2000 with the long
  inner loop is unfinished.
- No host load runs (20+ replays under CPU load), no device I/O from vCPU 1.

## Open problems

- **ENOMEM on open:** `hvx_open` kzallocs `struct hvx_vm` in one piece (64 KiB console, input
  and fuzz buffers, now also `struct hvx_sched`), a high-order allocation that fails when
  host memory is fragmented. Pre-existing hazard (see M57), slightly larger now. Fix:
  allocate the VM struct with kvzalloc, or the big buffers separately.
- The single-vCPU path keeps the stale STI/MOV-SS shadow after emulated instructions.

## Stage 2 (unchanged from the plan)

AP start-up (INIT/SIPI with unrestricted guest), MP table and CPUID topology for n > 1,
device interrupts routed by IOAPIC/MSI-X destination (today all go to vCPU 0), real
Linux SMP boot replay- and branch-identical without TSC warnings, then the stack. Clients
(explorer, verify harnesses) must snapshot every vCPU plus HVX_GET_SCHED.
