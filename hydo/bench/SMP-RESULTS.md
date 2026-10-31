# Two vCPUs on the race defects d8, d9, d12 (2026-10-23)

> Note (M107): `explore.py` and the other Python files named below were the explorer and tools of
> the time. The explorer is now the Rust crate `hydo/crates/hydo-explore` (`hydo explore`, or the
> `hydo-explore` program, with the same options); the Python files were deleted in M107 and can
> be restored from commit 2a58c4b (`git checkout 2a58c4b -- hydo/explore hydo/onboard hydo/history` in a scratch checkout).
> The commands below are records of how things were run and measured.
>
> Note (M108): the benchmark runner is now the Rust program `hydo-bench` (`hydo/crates/hydo-bench`):
> `python3 run.py ...` is `hydo-bench run ...` (bench_more_faults.py's cells are `hydo-bench run --more-faults
> --cf-range LO-HI ...`), `confirm.py` is `hydo-bench confirm`, `listrun.py` is `hydo-bench list`, `depth1map.py`,
> `d7prob.py` and `addbuggify.py` are `hydo-bench depth1map|d7prob|addbuggify`, and `--table DIR...` is
> `hydo-bench table DIR...`; the same options, outputs and printed lines. Not ported: `--strategy portfolio` and
> `--rescore` (one-time M56 re-scoring). The Python files, `mkpatches.py` and `smp_ident.py` were deleted in M108;
> restore one with `git show 705cf98:hydo/bench/<file>` (docs/HISTORY.md M108).

Question: do two virtual CPUs with branch-counted turns (Milestones 66 and 69) find the
benchmark's race defects faster, or find them where one vCPU does not?

Defects (Milestone 62, `README.md` "Harder defects d8–d12"): d8 no per-order lock, d9 shared
Redis connection without a lock (the order service panics), d12 shared Kafka key buffer. Control:
`explore-bench-m59-control-pin-disk.img`. All images are the `-pin` set
(`explore-bench-m59-<defect>-pin-disk.img`); main was not merged into this branch.

## Setup

- Bare-metal test machine, main `hvx.ko` srcversion `CA5BF8BF2A0D66B312A68EF` (not reloaded). Userspace
  built fresh from this branch in `~/hydo-bench2`. Results in `/var/tmp/bench2` on the box.
- `run.py` exactly as in M54/M62: 3 repetitions x at most 60 runs, early stop at the first
  detection, seeds `rep*1000+1..`, 1 worker, `--window 1` (the control VM is the second VM).
  A detection is a durable-settings VIOLATION whose schedule passes on the control; it is re-run
  once and its fingerprint compared.
- New: `run.py --vcpus N --turns` and `confirm.py --vcpus N` pass the vCPU count to the engine
  (every VM, the control's too) and add `explore.py`'s turn tokens (`q=`, `qs=`, own random
  stream, about half the schedules). Without the new flags the schedule strings are unchanged:
  `smp_ident.py` compares 5,760 strings (4 generators x new-faults/buggify/concurrency x the 180
  seeds) with the previous `run.py`, all identical; with `--turns` only the turn tokens are added.
- Strategy swarm. Cells: (a) 1 vCPU `--concurrency`, (b) 2 vCPUs `--turns --concurrency`,
  (c) 1 vCPU without concurrency, (d) 2 vCPUs `--turns` without concurrency.

## Hand-written triggers on 2 vCPUs (`confirm.py --vcpus 2`)

All three still fail, twice with the same fingerprint, and the control passes them. No trigger
needed adapting.

| defect | trigger | verdict | fingerprint (x2) | control |
|---|---|---|---|---|
| d8 | `Q=2d` | kafka order=0 present 2 times | `d12f064cc2b971f3` | OK `311544a2dc91d642` |
| d9 | `Q=4` | front door crashed: panic ... [15:10] | `0b022003ce23c090` | OK `fddf632a6334d324` |
| d12 | `Q=2` | kafka order=0 present 2 times | `480b84808a1c8a99` | OK `ce5301166cc1c114` |

## Cells (run of first detection per repetition; median)

| defect | (a) swarm, 1 vCPU, cc | (b) swarm, 2 vCPUs, turns, cc | (c) swarm, 1 vCPU, no cc | (d) swarm, 2 vCPUs, turns, no cc |
|---|---|---|---|---|
| d8 | 3/3 (5, 2, 46) med 5 | 3/3 (5, 2, 46) med 5 | 0/3 | 0/3 |
| d9 | 3/3 (5, 31, 1) med 5 | 3/3 (5, 31, 8) med 8 | 0/3 | 0/3 |
| d12 | 3/3 (5, 2, 1) med 2 | 3/3 (5, 2, 1) med 2 | 3/3 (23, 19, 1) med 19 | 3/3 (18, 13, 1) med 13 |
| detections | 9/9 | 9/9 | 3/9 | 3/9 |

Random strategy (optional; stopped early on request):
- (rb) random, 2 vCPUs, turns, cc: d8 (4, 1, 7), d9 (1, 1, 2), d12 (1, 1, 2), 9/9. These equal
  M62's random+cc on 1 vCPU, which was measured on an older module and not re-run here.
- (rd) random, 2 vCPUs, turns, no cc: STOPPED EARLY. d8 repetition 1 found nothing in 60 runs;
  repetition 2 had run 19 schedules without a detection when it was stopped. d9 and d12 not run.

**Checks.** Every detection (33 in the five cells) re-ran with the same fingerprint, matched
its defect's signature, and passed on the control; no candidate was rejected by the control.
Cell (a) reproduced M62's swarm+cc numbers exactly (d8 5/2/46, d9 5/31/1, d12 5/2/1), so the
module changes since M62 did not change the one-vCPU results here.

## Did the 2-vCPU runs use the second CPU? Yes.

A stderr-only diagnostic (`HVX_EXPLORE_VCPUSTAT=1` in `hvx_explore`; it reads
`HVX_GET_SCHED` and changes neither the guest nor any fingerprint) re-ran 21 two-vCPU schedules:
cell (b)'s 9 detecting schedules, the three triggers, and three no-concurrency schedules from
cell (d), on the three defect images.
- Every boot printed `smp: Brought up 1 node, 2 CPUs`.
- During the workload vCPU 1 ran about as much as vCPU 0: 13–733 million guest branches for
  vCPU 1 against 14–683 million for vCPU 0. Most runs took 2,000–5,600 turns per vCPU; short
  turn tokens give many more (3.3 million with `q=33`).
- All 21 fingerprints equal the ones recorded in cell (b) and by `confirm.py`, so the diagnostic
  binary behaves the same.

So the identical run numbers in (a) and (b) are not an idle second CPU. They come from the
schedules: in the concurrency cells the detecting schedule carries `Q=<n>` concurrent clients
(often `Q=4d`), and with that many overlapping requests the defect fires under either CPU count,
as the triggers show. The first detecting seed is then the first seed whose schedule has enough
concurrency, which is the same seed in both cells.

## Where two vCPUs behaved differently

1. **d9, cell (b) repetition 3.** Seed 3001 (`K=kafka@25+2 Q=4`) panicked on 2 vCPUs as on 1,
   but the restarted order service could not dial Kafka (`ERROR front door restart`), so the
   run had an ERROR verdict and does not count. The next detection was run 8. This is a harness
   outcome, not a search difference.
2. **d12 without concurrency.** On 2 vCPUs, seed 2013 (`disk=none ... | run K=kafka@26+1
   R=everysec`, the identical string, no turn tokens) fails (`order=27 present 2 times`,
   `abdaa3c268fef56b`); on 1 vCPU it passes. Seed 1018 fails on 2 vCPUs with `q=716449 qs=...`
   and passes on 1 vCPU. So (d) found d12 a few runs earlier than (c) in two repetitions
   (18 vs 23, 13 vs 19). Three repetitions are too few to call that a real speed-up.
3. **d9 with only two concurrent requests (`Q=2`), hand-written probe** (every schedule twice,
   all pairs identical):
   - 1 vCPU, default timing: `Q=2` and `Q=2d` never crash (as M62 found).
   - 2 vCPUs: `Q=2` crashes in 6 of 9 turn settings (default, `q=10000`, `q=100000`,
     `q=10000 qs=2`, `q=1000000 qs=5`, `q=200`); `Q=2d` crashes in 0 of 7. The control passes
     all 6 crashing schedules on 2 vCPUs.
   - 1 vCPU with the existing timing levers: `Q=2` crashes in 5 of 10 (tick jitter seeds 1, 2, 6;
     `pp=65536`; `xt=2 xtr=4 xtpm=500`).
   So 2 vCPUs reach this interleaving, but so does 1 vCPU once its timing is perturbed. It is
   not something only 2 vCPUs can find.

## What it means

- With concurrent clients, every defect is found at the same runs with 1 or 2 vCPUs (one
  exception caused by a harness ERROR). The concurrency in the client, not the second CPU,
  decides detection.
- Without concurrent clients, d8 and d9 stay unfound on 2 vCPUs (0 of 6 repetitions, 360 runs),
  as on 1 vCPU. Two CPUs do not create the overlapping requests these defects need.
- d12 without concurrency: 2 vCPUs changed which schedules fail and were a little earlier in two
  of three repetitions; not enough data to claim faster.
- Cost: a two-vCPU run took about 43 s against about 15 s for one vCPU in the no-concurrency
  cells (sum of run times; box load differed between the cells, so this is approximate). Fine
  turns are much dearer (a `q=1118` run took 363 s).

## Not verified

- Only swarm was run in full; random was stopped early (rb complete, rd partial).
- 3 repetitions per cell; the d12 difference between (c) and (d) is within noise.
- The 1-vCPU timing-lever probe schedules that crashed d9 were not run on the control.
- Only 2 vCPUs; 3 or 4 not tried. The turn mix is `explore.py`'s (per schedule, not per turn).

## Files

`run.py` / `confirm.py` (`--vcpus`, `--turns`), `smp_ident.py`, `listrun.py` (`--vcpus`,
`--repeat`, `--env`, crash line), `smp_cells.sh`, `smp_after.sh`, `smp_after2.sh`,
`smp-d9-q2*.list`; `hv/hvx/hvx_explore.c` (`HVX_EXPLORE_VCPUSTAT`). Box: `/var/tmp/bench2/<cell>`,
`confirm-v2`, `diag`, `probe-v1`, `probe-v2`, `probe-v1t`, `probe-ctl`, `cells.log`.
