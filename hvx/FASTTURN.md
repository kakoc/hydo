# Cheaper vCPU turn ends, and seeded burst turns (2026-10-24)

M74 found that a vCPU turn end costs about 0.8 ms of host time, so short turns (100 to
1,000 branches), which are needed to hit narrow race windows, make a run 10–50 times
slower. This work measured where that time goes, cut what could be cut without losing
exactness, and added an opt-in burst turn mode so that dense switching is affordable on
average.

Developed on a side-by-side module `hvxft.ko` (`/dev/hvxft`, built by
`hvx/fastturn/build-hvxft.sh` in `~/hydo-ft` on the bare-metal test machine). The main `hvx.ko` was
not touched or reloaded. The side module was unloaded at the end.

## 1. Where a turn end's time goes

New diagnostic counters (`HVX_GET_FTSTAT` / `HVX_RESET_FTSTAT`, ioctls 60/61, host-side
only, never in guest state, a snapshot or a fingerprint) split every `hvx_run_vcpu` call
into its parts with the host TSC, count landing steps, and record the PMU overflow skid
(branches retired past the programmed overflow before the VM exit). At VM release one
`TURNSTAT` line per vCPU goes to dmesg. `hvx/fastturn/ft_cost.c` runs two vCPUs through a
loop of N branches each in turns of Q branches and prints the breakdown.

Breakdown per turn end, before any change, Q = 1,000, the tightest loop (`dec rcx; jnz`,
2 instructions per branch), box under load (other sessions' VMs), host cycles at 3.8 GHz:

| part | cycles | µs |
|---|---|---|
| whole turn end (one `hvx_run_vcpu` call) | 825,000 | 217.6 |
| landing single-steps (432 MTF steps of ~1,880 cycles) | 811,000 | 214.0 |
| free-running guest | 3,000 | 0.8 |
| `setup_vmcs` + INVEPT | 3,400 | 0.9 |
| VMXON + VMCLEAR + VMPTRLD | 890 | 0.2 |
| PMU take-over / give-back | 1,230 / 1,140 | 0.3 / 0.3 |
| guest MSR + FPU load / save + `save_sregs` | 980 / 1,560 | 0.3 / 0.4 |
| VMCLEAR + VMXOFF + IRQs on + `kernel_fpu_end` | 870 | 0.2 |
| `kernel_fpu_begin` + IRQs off | 240 | 0.06 |
| scheduler between turns (`hvx_sched_run` minus the call) | under 1,000 | < 0.3 |

So **98% of a turn end is the landing**: the PMU is programmed to overflow 256 branches
(`HVX_PMU_MARGIN`) before the turn end, and the remaining branches are single-stepped with
the Monitor Trap Flag. Everything else (VMXON/VMXOFF, VMCS switch and rebuild, MSR and PMU
reprogramming) is about 11,000 cycles, 3 µs, 1.5%. The number of steps is
(256 − skid) × (instructions per branch): 432 for the tight loop, about 1,250 for a loop
of 10 instructions per branch, about 1,500 for a load/store loop, about 1,350 in a Linux
boot and about 1,780 in the four-service stack. Of a step's ~1,880 cycles, ~970 are the
VMRESUME-to-exit round trip itself (measured around the trampoline); the rest is the run
loop's software.

### The overflow skid (what a smaller margin must cover)

| guest code | skid max | mean | overflows |
|---|---|---|---|
| `dec rcx; jnz` (1 taken branch per cycle) | 40 | 40 | 40,000 |
| 8 dependent adds + `dec; jnz` | 5 | 4 | 40,000 |
| load / add / store / `dec; jnz` | 9–10 | 7 | 40,000 |
| 16 never-taken `test; jnz` + `dec; jnz` (densest branch stream) | 67–68 | 35 | 40,000 |
| a cache-missing pointer chase, then 150 (or 200, 300) independent never-taken branches | **107** | 33–63 | 40,000 each |
| real Linux, 2-vCPU boot | 49 | 3–19 | ~1,800 |

The skid is about 25–40 cycles of retirement after the overflow: dense branches retire up
to 4 per cycle when retirement was stalled behind a cache miss, which gives the largest
value seen, 107, the same for bursts of 150, 200 and 300 branches. The 256-branch margin is
2.4 times that; 160 is 1.5 times; 128 is 1.2 times.

## 2. Changes (module `hvx/hvx.c`, ABI `hvx/hvx_abi.h`)

None of them changes where a turn ends or any guest-visible result.

1. **PMC0 is not re-programmed between landing steps.** A step after a step keeps the
   counter running from the value read at the exit (it is ~2^30 branches from overflow)
   instead of writing −2^30 again; WRMSR is serializing. The count per entry is now
   `PMC0(exit) − PMC0(entry)`, the same number. (`pmu_base`, `pmu_cur`, `pmu_carry`.)
2. **No `PERF_GLOBAL_STATUS` read after a landing step**: a step's counter cannot have
   overflowed, so only an exit after a free-running entry can carry our overflow.
3. **`PROC_EXEC_CTL` is cached** in the run loop (`pcc`), saving two VMREADs per entry.
4. **The VMX capability MSRs (0x480–0x490) are read once at module load** and served from
   a cache by `rd()`; `setup_vmcs` read about a dozen of them per call.
5. **A separate landing margin for turn ends** (`HVX_SET_TURNCTL.margin`, default 256 =
   unchanged). Ticks keep 256. A smaller margin cuts the steps proportionally; it is
   opt-in because it narrows the safety factor over the skid (see above).

Changes 1–3 cut a landing step from ~1,840 to ~1,560 cycles (15%) in an interleaved A/B on
one loaded module (the `ft_opt` module parameter turns 1, 2 and 4 off for this; default on;
`hvx/fastturn/ab.sh`). Change 4 cut `setup_vmcs` from ~3,400 to ~2,700 cycles.

### Not done: keeping each vCPU's VMCS loaded and launched between turns

Every turn is one full VMXON…VMXOFF section with a VMCLEAR, VMPTRLD and `setup_vmcs`
rebuild. Keeping VMX on across the turns of one `HVX_RUN` would save at most the ~3 µs of
fixed cost per turn end, 1.5% at margin 256 and about 3% at margin 160, but it would hold
the host CPU with preemption off across turns (today the host may run between any two
turns), and it would skip the canonical rebuild of guest state that M34 relies on
(unusable-segment canonicalisation, a fresh interruptibility state). Measured too small to
be worth that risk; not attempted.

### Other things looked at

- **Re-arming the counter inside the margin** (overflow again R − m branches before the
  target) does not help: the final free-running stretch still needs a margin above the
  largest skid, so the steps left over are the same as with that margin directly.
- **Stepping by branch instead of by instruction** (TF + DEBUGCTL.BTF) would cut steps
  about 5 times but means intercepting #DB and hiding TF from the guest's PUSHF, interrupt
  frames and SYSCALL; not attempted.

## 3. Burst turns (`HVX_SET_TURNCTL`, opt-in)

New ioctls `HVX_SET_TURNCTL` / `HVX_GET_TURNCTL` (62/63, `struct hvx_turnctl`), only on a
VM with the scheduler (EINVAL otherwise, so a 1-vCPU VM refuses them). `struct hvx_sched`
and every existing ioctl keep their layout. Everything in the struct is policy, like the
turn length and seed: nothing is state, so snapshots need nothing new.

The rule: the VM-wide branch clock is cut into windows of `W = max(burst_window, quantum,
burst_len)` branches. Window w holds one burst iff `h % 1000 < burst_permille`, with
`h = splitmix64(burst_seed + (w+1)·GOLDEN)`; the burst covers `[w·W + o, w·W + o + B)` with
`o = (h >> 16) % (W − B + 1)`. A turn that starts inside a burst is `burst_q` branches long
(cut at the burst's end); any other turn has its usual length (`quantum`, or the seeded
length) but ends where the next burst starts if that comes first. So the turn lengths are a
pure function of the policy and the VM branch count at each turn's start. That count
(`hvx_sched.pmu_total`) is scheduler state, so replay and branch-from-snapshot reproduce
the turns; the bursts sit at fixed points of guest time, not of the turn index, so they do
not move when an earlier turn is cut differently. (The request said "a pure function of
the seed and the turn index"; a turn-index rule needs a burst counter in the scheduler
state, which would change `struct hvx_sched`. The branch clock gives the same
reproducibility with no new state.) `GET_TURNCTL` also reports `burst_turns` and `clipped`
(statistics, zeroed by SET).

**Tokens.** `hvx_explore` host tokens, applied for the workload only like `q=`:
`qb=<B>:<s>:<permille>` with optional `qbs=<hex seed>` (default 1), and `qm=<margin>`.
RESULT adds `qbn=<turns started in a burst>`. On a 1-vCPU engine they give
`ERROR harness q=/qs=/qb=/qm= need several vCPUs`; `BRANCH` refuses them like `q=`.
`explore.py --bursts` (needs `--turns`) gives half of the schedules a `qb=` token from its
own random stream (B in 2,000..200,000, s in 20..1,000, permille 50/100/250/500/1000);
the shrinker drops it, halves permille, halves B. `explore.py --turn-margin N` passes
`HVX_TURN_MARGIN=N` to the engine (the margin is not part of the schedule, since it must not
change results). `hvx_stack_verify` takes `HVX_QUANTUM`, `HVX_TURN_MARGIN`,
`HVX_BURST=B:s:permille[:seed]`, `HVX_QUANTUM_AFTER=<console string>` (apply them only from
that point) and `HVX_FTSTAT=1` (print the cost statistics); all off by default.

## 4. Tests

All on `/dev/hvxft` with the final module.

- **Stage-1/2 tests with their recorded numbers** (`hvx/fastturn/stage1.sh`):
  `hvx_sched_pmu_test` (turn hash `97b3468c16d739fa`, landing hashes `f841184e6fdde8b5` /
  `19337dfb08c526f9`, fingerprints `95e6c9a02d5d584d`, `05521a9492a3ce9f`,
  `b544e114dc426e85`, snapshot `73a465516c4e0094`), `hvx_sched_race_test` (grid
  `8cb7c61a2393f44e`), `hvx_sched_sweep_test 2000` (`39971eed9c2f35fe`), `hvx_sipi_test`
  (`eef5c1ebd4df2e7d` / `c8d015ab3b57252d`): **ALL PASS with the default margin and ALL
  PASS with `FT_MARGIN=128`** (`hvx_sched_util.h` applies it to every scheduler VM the tests
  create), so turn ends land on the same branch counts whatever the margin.
- **`hvx_sched_burst_test`** (new, PASS, 4 s): two vCPUs, Q = 100,000, bursts B = 20,000,
  s = 97, permille 300, ticks every 99,991 branches. (A) the turn count, turns started in a
  burst and long turns cut at a burst (2,747 / 2,691 / 13) equal a userspace model of the
  rule; 3 runs identical, fp `0b93f018cff50590`, no overshoots. (B) stop at a turn inside a
  burst (turn 915) and one outside (1,060): save, finish; restore into the same VM and into
  a fresh VM with the policy set again: all equal. (C) the same at mid-turn stops after 7
  and 23 ticks. (D) without bursts: 52 turns, another fingerprint; `SET_TURNCTL` leaves
  `GET_SCHED` unchanged; a 1-vCPU VM refuses it with EINVAL. (E) margins 256, 160 and 128
  give the same turn hash and fingerprint.
- **2-vCPU Linux boot** `hvx_smp_boot_verify` (default turns): two fresh boots, branch at
  the marker and branch at turn T/2 all `6584c954c2839491`, PASS (also before the cuts, with
  the diagnostics only).
- **Regression gate** `SIDE_TMP=/var/tmp/fastturn-side HVX_DEV=/dev/hvxft
  hvx/race/side-metal.sh`: **ALL PASS (26 checks)**, every fingerprint unchanged, both
  boot verifies included (no disk `7a5559028ffbf52c`, with disk `36583d533d7c989b`).
- **Pinned explorer reproducers** through the new `hvx_explore` on `/dev/hvxft`:
  `explore-disk.img` `2403499c9a3f8419` (VIOLATION, order 11), `explore-fix-disk.img`
  `f87130453609a5d6` (OK), both unchanged.
- **hvx_explore with the new tokens** (`hvx/fastturn/explore_smoke.sh`, image
  `explore-binlog-ret-xr-nq-disk.img`, `--vcpus 2`): `disk=none dseed=0 qb=20000:100:250
  qbs=5 | run` twice from a fresh boot: `7ab4cf9d8ed60bd7` both, OK 30/30, `qn=7976
  qbn=2825` (2,825 of 7,976 workload turns started in a burst; workload 6.2–6.9 s); the same
  with `qm=160`: `7ab4cf9d8ed60bd7` again; on a 1-vCPU engine: `ERROR harness
  q=/qs=/qb=/qm= need several vCPUs`.
- **2-vCPU stack sanity** (`hvx_stack_verify HVX_VCPUS=2 sanity` on
  `stack-binlog-ret-disk.img`) with default turns: `05915e49a5506c58` (the M69 value), OK
  30/30. With q = 1,000 from "provisioning synced" to the end: `6cff32e69d62e546` in all
  nine runs (main module, side module with the cuts on and off, margins 256 and 160). With
  bursts: `d8afbf8d9fce8a70` at margins 256 and 160.
- **explore.py**: `turns_ident.py` against main's explore.py: 179,616 schedule strings
  identical without `--turns` (and the `--turns` checks unchanged); new
  `hvx/explore/bursts_ident.py`: with `--turns --bursts` every field but `qb` equals the
  `--turns` schedule (2,000 schedules, 48.9% get `qb`), strings round-trip, shrink
  candidates as documented. @@EXPLORE@@

## 5. Before and after

The box was shared throughout. The numbers marked *quiet* were taken while the other
session's agents (`~/hydo-asw`, `~/hydo-pf`) held back (`/var/tmp/hydo-QUIET`,
`hvx/fastturn/quiet.sh`), but two VMs of a third session and a native 3-thread workload kept
running (load average about 8 on the 4-core, 8-thread box), so even the quiet numbers vary
by about 10–15%. Everything else was measured under heavier load (load 10–29).

**Per turn end, hand-written loop, Q = 1,000** (`ft_cost`, µs per turn end):

| loop | before | cuts only | cuts + margin 160 | cuts + margin 128 |
|---|---|---|---|---|
| `dec; jnz` (load, interleaved A/B) | 214 | 181 | 103 | 75 |
| 10 instructions per branch (load) | 1,203 (2 calls/turn) | 1,004 | 623 | 493 |
| load/store loop (load) | 737 | 609 | 374 | 295 |

**2-vCPU stack, q = 1,000 from "provisioning synced" to the end** (about 58,500 turn
ends; the boot before it runs with default turns and takes ~23–30 s; *quiet*):

| run | q=1000 part | whole run | cycles per step | µs per turn end |
|---|---|---|---|---|
| main module (old code) | — | 75.5 s (a first run: 111.7 s) | — | ~0.8 |
| side, cuts off (`ft_opt=0`), margin 256 | 52.8 / 53.2 s | 76.7 / 76.3 s | 1,892 / 1,905 | 0.90 |
| side, cuts on, margin 256 | 45.1 / 50.9 / 45.7 s | 68.9 / 77.1 / 74.7 s | 1,613 / 1,810 / 1,634 | 0.78–0.87 |
| side, cuts on, margin 160 | 28.7 / 28.3 s | 51.0 / 51.0 s | 1,637 / 1,612 | 0.49 |
| default turns + bursts (10% of 2^20-windows hold 20,000 branches of 100-branch turns), margin 256 / 160 | 0.89 / 0.69 s | 27.8 / 23.3 s | 2,121 / 1,645 | — |

So on the real workload the per-step cuts give about 10% (between 5 and 15% depending on
the run), and margin 160 gives another factor 1.6: a turn end costs about 0.49 ms instead
of about 0.8 ms. With bursts, the run takes about 730 extra turns, ~1,100 of them 100
branches long, in under a second; q = 100 for the whole stretch would be about 580,000
turn ends of ~0.3 ms each, roughly 3 minutes.

**2-vCPU Linux boot** (`hvx_smp_boot_verify HVX_MODE=one`, *quiet*): main 1.38 / 0.94 s,
side 1.24 / 0.86 s (default turns: most turns end at HLT; the landings are ticks).

## 6. Open problems and what is not verified

- **The cost is still the landing.** At margin 256 a turn end in the stack is ~1,780 MTF
  steps; each costs ~1,600 cycles, of which ~1,000 are the hardware VM round trip. Much
  shorter turn ends need fewer steps: a smaller margin (only safe above the skid), or a
  different stepping mechanism (branch stepping with TF/BTF).
- **A margin below 256 is opt-in, not the default.** The largest skid seen was 107 branches
  on a crafted worst case, 49 in Linux. It was measured on one CPU (Xeon E3-1270 v6) under
  host load; nothing proves a larger skid cannot happen (an SMI, a different
  microarchitectural stall). An overshoot is counted (`hvx_sched.overshoots`) but makes the
  run irreproducible. None was seen in any run here.
- **Not merged into the main `hvx.ko`**: only the side module ran this code.
  `deploy-metal.sh` was not run; the side-metal gate stands in for it.
- The per-step A/B was taken under load; the gains vary from 5 to 15% between runs.
- Bursts were tested on hand-written guests and with one stack run per margin; they were
  not measured for finding races (that is what they are for).
- The `ft_opt` parameter and the diagnostic counters stay in the module (they cost about
  three RDTSCs per entry).

## Files

`hvx/hvx.c`, `hvx/hvx_abi.h` (ioctls 60–63, `struct hvx_ftstat`, `struct hvx_turnctl`),
`hvx/hvx_sched_burst_test.c`, `hvx/hvx_sched_util.h` (`FT_MARGIN`),
`hvx/hvx_stack_verify.c` (turn knobs, `HVX_FTSTAT`), `hvx/hvx_explore.c` (`qb=`, `qbs=`,
`qm=`), `hvx/explore/explore.py` (`--bursts`, `--turn-margin`), `hvx/explore/hvm_backend.py`
(drops `qb` on the Mac), `hvx/explore/bursts_ident.py`, and `hvx/fastturn/`:
`build-hvxft.sh`, `Makefile.hvxft`, `ft_cost.c`, `ab.sh`, `stage1.sh`, `meas.sh`, `meas2.sh`,
`quiet.sh`, `waitdone.sh`. Box results: `/var/tmp/fastturn`.
