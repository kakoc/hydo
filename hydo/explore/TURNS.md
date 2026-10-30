# Several vCPUs in the explorer (SMP Stage 3, 2026-10-22)

Stage 3 of `hv/hvx/SMP_PLAN.md`: the harness and explorer side of the in-kernel vCPU
scheduler that Stage 1 (`hv/hvx/SMP_STAGE1.md`, Milestone 66) added to `hvx.ko`. No module
change: `hvx.c`, `hvx_abi.h` and `hvx_boot.h` are untouched (another agent owns them for
Stage 2). Real Linux cannot use a second vCPU yet (no INIT/SIPI, the MP table lists one
CPU), so this is plumbing that is tested now on what a two-vCPU VM can do today, and
end to end once Stage 2 lands.

## What was built

**Engines** (`hv/hvx/hvx_stack_verify.c`, which `hv/hvx/hvx_explore.c` includes):

- The vCPU count: `hvx_explore --vcpus N` (or `HVX_EXPLORE_VCPUS=N`),
  `hvx_stack_verify` with `HVX_VCPUS=N`. Default 1. With N > 1, `boot_setup` creates the
  VM with `n_vcpus = N` and `HVX_VM_SMP_SCHED` (it needs the PMU clock; without one the
  engine stops with a clear message instead of falling back to the exit clock).
- Snapshots: `smp_get` / `smp_set` take and restore vCPUs 1..N-1 (`HVX_SET_VCPU` +
  `HVX_GET/SET_SNAPSHOT`) and the scheduler (`HVX_GET/SET_SCHED`), in the ABI's order
  (every vCPU first, then the scheduler, which carries the VM-wide clock and the per-vCPU
  cr2, halted and re-injection state). Used by S0, S1 and branch points in `hvx_explore`
  and by both branch checks of `hvx_stack_verify`. vCPU 0 is handled exactly as before.
- Fingerprints: `fnv_smp` hashes vCPUs 1..N-1 (the same canonical fields as vCPU 0) and
  the scheduler STATE: current vCPU, turn in progress, turn end, turn count, THROTTLE
  cadence, turn hash, the VM clock, and the per-vCPU records. It leaves out the policy
  (quantum, seed, pause_k, stop_turn), as tick-jitter policy is left out, and the
  statistics. It is called only when N > 1.
- Parking: until real Linux can start vCPUs 1..N-1, `boot_setup` parks them: IF=0 and
  halted. The scheduler only wakes a halted vCPU for a deliverable interrupt, which needs
  IF=1, so they never run. `HVX_VCPU_PARK=0` leaves them as the module created them;
  that is the setting to use once Stage 2 gives them a real wait-for-SIPI state.

**Schedule tokens** (host part, applied for the workload only, like tick jitter):

| token | meaning |
|---|---|
| `q=<branches>` | every turn is this many guest conditional branches (module default 2^20) |
| `qs=<hex>` | seeded turn lengths in [1, q]: `1 + splitmix64(seed + (t+1)*GOLDEN) % q`, t = turn number (hvx.c `hvx_sched_len`); with no `q=` the VM's current length is used |

At the start of the workload the engine reads the scheduler state, remembers the old
length and seed and the turn count, and sets the new ones; at the end of the workload
(right after the workload fingerprint) it puts the old policy back. The RESULT line then
carries ` qn=<turns taken during the workload>` after the tj/xt fields. Without the tokens
the line is unchanged. On a one-vCPU VM the tokens give the verdict
`ERROR harness q=/qs= need several vCPUs (--vcpus N > 1)`; they are never ignored. A
`BRANCH` request refuses them, like the other timing tokens.

**No explicit form.** `HVX_SET_SCHED` takes one length and one seed for every turn; there
is no per-turn list in the ABI, so there is no `qx=` token. A list could be emulated from
userspace with `stop_turn` (stop after turn k, set the next length, continue), but every
such stop is an extra return to userspace, and with a run budget (`rb=`, branching) return
points move the end state (the stop-point problem of M27/M43). ABI wishes for the module
owner, below.

**explore.py**:

- `--vcpus N` (1..8): the engine gets `--vcpus N` only when N > 1, so the default command
  line is unchanged (`engine_args()`; the hand-reproduction line in report.md includes it).
  Refused with `--backend hvm`.
- `--turns` (needs `--vcpus N > 1`): `add_turns` gives half of the schedules turn tokens
  from their own random stream (salt `0x7A4E5C0FFEE7A4E5`), so every other field equals the
  schedule without `--turns`. The length mix is rr's chaos mode, per schedule: about 80%
  long (2^19..2^21 branches, around the module's 2^20), about 10% at most 100, about 10% in
  101..10,000; half of them seeded (`qs=`). Measured over 8,000 schedules: 50.9% get
  tokens; of those 9.4% <= 100, 10.2% in 101..10,000, 80.4% long, 49.5% seeded.
  The mix is per schedule, not per turn, because the module draws every turn from one
  uniform range [1, q]; rr mixes per turn.
- `parse` / `render` / RESULT parsing (`qn=` -> `res["qn"]`); shrink candidates: drop the
  tokens, then make seeded turns fixed, then lengthen the turns 8x toward 2^20.
- `hvm_backend.hvm_supported` drops `q` (the Mac has no PMU clock).

## Tests and numbers (bare-metal test machine, main `hvx.ko` srcversion C907B1D09F5370F6CC93B45)

**One vCPU is byte-identical.**

- `turns_ident.py <main's explore.py> <new explore.py> <main's hvm_backend.py>`: 179,616
  schedule strings identical without `--turns`, among them the 8,000 of seeds 1-1000 x
  random/swarm x `--timing` x `--new-faults`, 102,400 from all four generators x every
  combination of the other streams (new faults, timing, concurrency, preempt, buggify,
  more faults, durable-only) x seeds 1-200, and 69,216 from all eleven sweeps x their
  stream combinations; finding classes under both knob rules, shrink candidates and
  engine arguments identical too; `hvm_supported` identical apart from dropping `q`.
- The two pinned reproducers through the new `hvx_explore` (`one`): explore-disk.img
  `2403499c9a3f8419`, explore-fix-disk.img `f87130453609a5d6` (both unchanged).
- Serve mode, main's binary and explore.py vs the new ones side by side
  (`--seeds 1-8 --strategy swarm --timing --new-faults --concurrency --preempt`,
  explore-binlog-ret-disk.img): all 8 schedule strings, fingerprints, verdicts and
  reproduction fingerprints identical. (Three of the eight are
  `ERROR harness tick jitter (needs the PMU clock)` on both: a schedule with `pp=` and
  `tj=` whose jitter delay exceeds the shortened period. That is an old interaction,
  not part of this work, and the message is misleading.)

**Several vCPUs, hand-written guest** (`turns_selftest.c`, two vCPUs selling seats with a
check-then-act race; PASS):

- A: stop at turn 85 of 171 (Q=1000, seed 1, 5 sales lost), snapshot both vCPUs + the
  scheduler with the helpers, finish; restore into a fresh VM, finish: straight,
  continued and restored all `facc75da92390ff2`.
- B: stop in the middle of a turn (in_turn=1, vCPU 1 running; ticks every 997 branches,
  HVX_RUNC budget): all three `90a12a5b3a92dcae`.
- C: changing only vCPU 1's rax changes the fingerprint.
- D: a parked vCPU 1 never runs (0 turns, 0 branches) while vCPU 0 sells all 400 seats.
- E: seeded turn lengths are reproducible (`c3c97ade6ee0d8a6` twice) and differ from
  fixed ones.

**Several vCPUs, real Linux with vCPU 1 parked** (explore-binlog-ret-disk.img,
`hvx_explore --vcpus 2`):

- The stack boots to S0 and S1 and runs the workload. `disk=none dseed=0 | run`: fresh
  boot (`one`) `18679b2fcca1a7bd` = two serve runs from the two-vCPU S1 snapshot
  `18679b2fcca1a7bd`, OK 30/30.
- `q=5000 qs=1f`: `b1f8e4eb2e4bf363`, qn=25729, in both serve runs and in `one`.
- `q=100`: `bbd590d75369c188`, qn=569,959, 205 s for the workload (against about 5 s with
  the default length): fine turns are expensive even with one runnable vCPU.
- A power cut with turn tokens (`disk=cut dseed=0 q=50000 | run K=kafka@5+2 C=20`, so the
  recovery VM is restored from the two-vCPU S0): `d8285e10641a7ff5` twice, qn=84,405.
- `BRANCH` at `@3` with 2 variants: the base equals the standalone `rb=16` schedule
  (`baffd76879f6ab3f`); variants 1 and 2 equal their standalone schedules
  (`06953dc13ffab309`, `5906694785364a89`).
- The two-vCPU fingerprints differ from the one-vCPU ones, as they must (the scheduler
  state is hashed and turn ends change where run calls return): the same plain schedule
  with one vCPU is `7582bbbec0d478d5`, also OK 30/30, also about 5 s.
- On a one-vCPU engine, `q=5000` gives `ERROR harness q=/qs= need several vCPUs
  (--vcpus N > 1)`; a malformed `q=0` is rejected as a bad request.
- `hvx_stack_verify` with `HVX_VCPUS=2`, mode `fault` (`hydo_kill=kafka@10`, two solo
  boots plus a branch from the snapshot before the fault, which now covers both vCPUs and
  the scheduler): all four `15be998ebba5667e`, OK 30/30, "mode=fault PASS".

- The same `hvx_stack_verify` fault check with one vCPU, new binary and main's binary side
  by side: both `dff8953006949951` for all four fingerprints.

## What waits for Linux SMP (Stage 2)

- Every test above has vCPU 1 parked or runs a hand-written guest. That the snapshots and
  fingerprints are complete for a real Linux using two vCPUs (per-CPU timers, IPIs,
  device interrupts routed to vCPU 1, a vCPU halted in the middle of a turn) is not shown.
- Turn tokens have not been measured as a way to find races in the stack; with one
  runnable vCPU they only move where turns end, not which thread runs.
- When Stage 2 lands: set `HVX_VCPU_PARK=0` (or remove parking) if the module gives the
  other vCPUs a wait-for-SIPI state; re-run `turns_selftest`, the serve-vs-`one` and
  branch checks above with the guest using both vCPUs; then compare `--turns` against
  `--timing` / `--preempt` on the race test and the stack (SMP_PLAN.md Stage 3).

## ABI wishes (for the module owner; not implemented)

1. An explicit per-turn list in `struct hvx_sched` (for example up to 32 `{turn, length}`
   pairs, like `tjx=`), so the shrinker can reduce a failing seeded schedule to the few
   turns that matter.
2. A per-turn length mix inside the module (for example `short_permille`, `short_max`,
   `mid_permille`, `mid_max`), so one run mixes long and short turns as rr's chaos mode
   does, instead of one uniform range per run.
3. Optionally a starve mode (one vCPU is skipped for k turns) and PCT-style priorities.

## Files

`hv/hvx/hvx_stack_verify.c` (`g_vcpus`, `smp_get`, `smp_set`, `fnv_smp`, `park_aps`, vCPU
count in `boot_setup`, the two branch checks), `hv/hvx/hvx_explore.c` (`--vcpus`, `q=`/`qs=`,
snapshots), `hydo/explore/explore.py`, `hydo/explore/hvm_backend.py`,
`hydo/explore/turns_selftest.c` (build: `cc -O2 -o turns_selftest turns_selftest.c` in
`hydo/explore`), `hydo/explore/turns_ident.py`. Box results: `/var/tmp/turns`.
