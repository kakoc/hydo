# Known bug #1: SQLite WAL-reset corruption (3.51.2 buggy, 3.51.3 fixed)

Can hydo find a real concurrency bug in unmodified software without being told the trigger?
Background and sources: `../CANDIDATES.md` section 1 (sqlite.org/wal.html section 11, the
Antithesis write-up). The bug: two connections in WAL mode; a checkpoint completes, a second
checkpoint starts, and while it starts another connection commits a transaction that resets
the WAL; the second checkpoint misses the reset and records part of the WAL as checkpointed;
a later checkpoint skips part of a committed transaction and the database goes corrupt.

## Files

| file | what |
|---|---|
| `walrace.c` | the workload + checker, written from the public description only: writers, checkpointers and readers on one WAL database, each its own connection, all concurrent; every committed transaction recorded; check = every committed row present with its value + `PRAGMA integrity_check` = ok |
| `hydo.yaml` | the manifest: no service, one workload, one invariant; `params: v` builds 3.51.2 (`3510200`, default) or 3.51.3 (`--set v=3510300`) |
| `build.sh` | builds `bin/walrace-<v>` statically from the official amalgamation zips (sha256-pinned) on the bare-metal test machine (Ubuntu gcc 13.3.0, static glibc); byte-identical across builds |
| `hunt.py` | the blind explorer for this target (below) |
| `hydo-cpudiag.yaml` | diagnostic copy of the manifest that prints per-CPU counters (several-vCPU sanity check only) |

Pins: `sqlite-amalgamation-3510200.zip` sha256 `6e2a845a…f23ec`, `sqlite-amalgamation-3510300.zip`
sha256 `acb1e6f5…552b4` (full values in `build.sh`).

## Why a separate driver

`explore.py`'s generators place kills, cuts and disk faults by the stack's service names and
only know the M/R knobs. This target has no service and needs no fault; its levers are the
thread interleaving (seeded tick jitter, Milestone 43) and the workload's own seed (the
manifest's `S` knob: writer/checkpointer/reader counts, transaction sizes, checkpoint modes,
autocheckpoint). `hunt.py` reuses `explore.py`'s worker pool and tick-jitter formulas and
generates three arms: `timing` (jitter only), `workload` (`S` only), `both`. Run i is a pure
function of (i, salt), and its schedule string alone reproduces it.

## Results (2026-10-18, bare-metal test machine, 1 worker; results in `/var/tmp/m48-sqlite`)

**Not found.** No run violated the invariant, on hydo or natively.

| run | what | runs | violations |
|---|---|---|---|
| hydo sanity | `disk=none dseed=0 \| run` on 3.51.2 / 3.51.3 | 2 | 0 (OK on both) |
| hydo `hunt1` | arms timing / workload / both, first workload shape (2–3 writers, 1 checkpointer, 0–1 reader, 8 steps) | 150 (13.7 min) | 0 |
| hydo `hunt4` | arms workload / both, wider shape (2–5 writers, 1–2 checkpointers, 0–2 readers, 4 steps) | 1000 (89 min) | 0 |
| native control 1 | the `hunt4` shape on the host's 4 real cores, tmpfs | 3000 seeds | 0 |
| native control 2 | + per-run subset of checkpoint modes (swarm style) | 1265 seeds | 0 |
| native control 3 | + writers that also checkpoint on a second connection | 1278 seeds | 0 |
| native **aimed** | shape pinned to the write-up/fix (PASSIVE checkpoints back to back, writers, no readers) — NOT blind | 300 + 300 on 3.51.3 | 0 + 0 |

What `hunt4` explored: 1000 distinct end states (every fingerprint different); 826,405
involuntary and 401,368 voluntary thread switches inside the workload (the guest has one vCPU,
so every interleaving point is a context switch of the process); 500 runs with tick jitter,
33,904 of their 148,020 workload ticks delayed. Median run 3.9 s guest time, ~11 runs per
minute on one worker. Determinism: a `hunt4` schedule re-run from a fresh boot gave the same
fingerprint twice (`e75e8480c7583361`).

Why it is plausibly out of reach as built (after reading the 3.51.3 diff, i.e. no longer
blind): the fix adds, in `walCheckpoint`, a check after taking read-lock slot 0 that the WAL
salt did not change — the race is a writer wrapping the WAL between the checkpointer reading
its header copy and the `nBackfill < mxFrame` test. On hydo that needs a preemption inside a
window of perhaps a few hundred instructions, while preemptions only happen at timer ticks,
one per 2^20 retired branches (the PMU clock period); tick jitter moves where a tick lands but
not how often. Even natively, with real parallelism, this workload did not hit it in ~5,800
seeds, so the workload is also not close enough to whatever the write-up's (generic, unpublished)
workload does; the public reproducer page (theconsensus.dev) could not be fetched (403 /
JavaScript wall).

Levers tried: workload seed (writers, checkpointers, readers, transaction count/size,
autocheckpoint, yields), tick jitter at 5–1000 per mille with 2,000–500,000-branch delays,
more threads, more checkpoints (up to 12 M per run), swarm-style mode subsets and mixed roles
(natively). Not tried, needs code outside this directory: a shorter tick period or extra
inserted preemption ticks in `hvx_explore` (it does not expose the PMU period); several
processes instead of threads.

Images used by the hydo runs (built by `hydo-build` from this manifest; walrace at commit
`fe97d2c`, before the mode-subset / mixed / aimed options were added): 3.51.2
`explore-ob-kb-sqlite-3510200-disk.img` sha256 `05d75b28…`, 3.51.3 `…-3510300-disk.img`
`2ff12b40…`; `hunt1` used an earlier walrace (`41c8f4b`). Rebuilding from this commit gives
new hashes (the program changed).

Commands (box, `~/hydo-sqlite/hvx/knownbugs/sqlite`):
`python3 hunt.py --image explore-ob-kb-sqlite-3510200-disk.img --fixed explore-ob-kb-sqlite-3510300-disk.img --runs 1000 --workers 1 --arms workload,both --salt 0xB4 --out /var/tmp/m48-sqlite/hunt4`;
native controls: `/var/tmp/m48-sqlite/native*.sh`, `aimed-native.sh`.

## Several vCPUs with branch-counted turns (2026-10-23, bare-metal test machine, 2 workers; results in `/var/tmp/sqlsmp`)

**Not found.** The question: now that hvx can run a guest on several vCPUs that take turns of
an exact, seeded number of guest conditional branches (Milestones 66 and 69), can a blind
search hit the WAL-reset window that one vCPU never reached? 144 runs on 2 vCPUs with short
turns (3.5 hours), 32 runs on 4 vCPUs with short turns (1.8 hours) and 912 runs on 4 vCPUs
with the default turn length (46 minutes) found no violation, so there was nothing to re-run,
shrink, or check on 3.51.3.

What changed (commit on branch `worktree-agent-aa50542a20e045d9c`):

- `hunt.py --vcpus N` passes `--vcpus N` to `hvx_explore`; `--turns` gives schedules the turn
  tokens `q=<branches>` (fixed turns) and `qs=<hex>` (each turn uniform in [1, q]) from their
  own random stream, so every other field equals the schedule without `--turns`. Without the
  new options the schedule strings are unchanged (36,000 compared against the old `hunt.py`
  over all arms and three salts). Also `--build`, `--snap-dir`, `--wall-secs`, `--batch`,
  `--max-wall`. The shrinker drops the turns first, then makes seeded turns fixed, then
  lengthens them.
- `hvx_explore`: a `SCHEDSTAT` line on stderr with each vCPU's branches and turns during the
  workload (several vCPUs only; reads only, the RESULT line is unchanged);
  `HVX_EXPLORE_BOOTCON=1` saves the boot console in `one` mode; `HVX_EXPLORE_WALL_SECS`
  overrides the 900 s per-run wall-clock cap.
- `hydo-cpudiag.yaml`: a diagnostic copy of the manifest (same walrace binary, extracted from
  the hunt image) whose workload step also prints `/proc/stat` per CPU and which CPU each
  walrace thread was last seen on. Used only for the sanity check below.

**Cost of short turns** (default workload `S=0`, the heaviest shape: 5 writers, 2
checkpointers, 2 readers; 2 vCPUs): each turn end costs about 0.8 ms of host time, so a run
costs about 13 s plus 0.8 ms per turn.

| turns | workload s | turns taken |
|---|---|---|
| default (2^20) | 13 | — |
| q=10000 | 32 | 38,465 |
| q=3000 | 67 | 72,435 |
| q=1000 | 216 | 244,255 |
| q=1000 seeded | 609 | 760,065 |
| q=300 | > 900 (wall cap) | — |
| q=100 | > 900 (wall cap) | — |

**The turn mix** (per schedule; the module takes one length for all turns of a run): 5%
default turns, 15% long (q log-uniform in 10,001..2^20, half seeded), 70% mid (q log-uniform
in 1,000..10,000, half seeded), 10% low (q log-uniform in 300..999, fixed only). That puts 80%
of the schedules at q <= 10,000, against 20% in `explore.py --turns`. Below 300 only seeded
turns reach: a seeded q draws each turn from [1, q], so a seeded q=5,000 also takes about 2% of
its turns below 100 branches. Fixed q=100 for a whole run would cost about an hour per run.

**Sanity on 2 vCPUs.**

- The guest boots with 2 CPUs: "smp: Brought up 1 node, 2 CPUs".
- The threads run on both CPUs (diagnostic image, `q=5000`): during the workload the guest's
  `/proc/stat` user time grew by 111 ticks on cpu0 and 43 on cpu1 (system time by 185 and
  241); of the 28 walrace threads sampled, 19 were seen on both CPUs. `SCHEDSTAT` for the
  same run: vCPU 0 retired 194 M branches in 41,966 turns, vCPU 1 179 M in 39,122. Over the
  whole hunt, vCPU 0 retired 19.5 G and vCPU 1 28.1 G workload branches.
- Reproducible: the default schedule gave `3173108c39ad8004` twice from a fresh boot and twice from
  the snapshot; `q=4864 qs=b5ec452e…` with tick jitter gave `958429cf955867e6` and
  `q=2398 qs=ee5712cc…` gave `6cac0c27871d3ad9`, each twice on two different engine
  processes and a third time inside the hunt (runs 1 and 2).

**The hunt on 3.51.2** (`explore-ob-kb-sqlite-3510200-pin-disk.img`, arms `workload` and
`both`, salt `0x5D7E`): 144 runs in 3 h 32 min, 0 violations, 144 different fingerprints.

| turn class | runs | median s | max s | median turns |
|---|---|---|---|---|
| default | 4 | 9 | 40 | — |
| long, fixed | 17 | 6 | 12 | 5,195 |
| long, seeded | 7 | 16 | 29 | 13,508 |
| mid, fixed | 55 | 21 | 1,654 | 23,947 |
| mid, seeded | 42 | 44 | 1,085 | 50,275 |
| low, fixed | 19 | 187 | 848 | 193,701 |

In total: 19.1 M turn switches, 80 M checkpoints (16.5 M of them left the WAL empty, the
state a WAL reset starts from), 162,068 committed transactions, 445,079 voluntary and 162,805
involuntary thread switches; 72 runs also had tick jitter (1,292 delayed ticks). For
comparison, the one-vCPU `hunt4` above had 1,000 runs and 826,405 involuntary switches, but
switches there could only happen at ticks, once per 2^20 branches; here two threads run at the
same time and hand over every few thousand branches.

**4 vCPUs.** The guest boots with "smp: Brought up 1 node, 4 CPUs"; on the diagnostic image
(`q=5000`) the guest's user time grew by 500, 519, 484 and 446 ticks on cpu0..cpu3, the four
vCPUs retired 79, 79, 82 and 79 M workload branches, and the walrace threads were sampled on
all four CPUs. One schedule (`q=6563 qs=a7233f79…`) gave `91e11b86a51bc00b` twice. Two runs:

- Same turn mix, salt `0x4C9A`: 32 runs in 1 h 47 min, 0 violations, 32 different
  fingerprints. Short turns cost more here (mean 341 s per run; fixed q of 300..999 took
  about 28 minutes per run), so a few hundred such runs did not fit.
- Default turns (2^20 branches, no turn tokens), salt `0x4D00`: 912 runs in 46 minutes
  (median 5 s per run), 0 violations, 912 different fingerprints; 484 M checkpoints, 66 M of
  them left the WAL empty. This is real 4-way parallelism at coarse turns, like the native
  controls above, which also never hit the bug.

**What it means.** Exact, short turns on several vCPUs did not make this bug reachable with
this workload in the budget we could afford. Two things limit it. First, cost: short turns are
expensive (0.8 ms per turn end), so 3.5 hours bought 144 runs, and the shortest fixed turns
that fit (about 300 branches) are no shorter than the window. Second, and probably more
important, the native controls above already ran this workload with real parallelism on 4
cores for about 5,800 seeds without a hit, so the workload itself is likely not producing the
trigger (a writer resetting the WAL exactly while a second checkpoint starts) often enough. A
rough estimate also says shorter turns help less than expected: once a turn is shorter than
the gap between two WAL resets, the chance of a reset landing inside the checkpointer's window
is about window / gap, whatever the turn length. The next steps are therefore on the workload
side (a shape that resets the WAL far more often, e.g. a writer that waits for a fully
checkpointed WAL before committing) or a module change that makes turn ends cheaper or puts
short turns only around WAL activity, not more runs of this search.

Commands (box, `~/hydo-sqlsmp/hvx/knownbugs/sqlite`):
`python3 hunt.py --image explore-ob-kb-sqlite-3510200-pin-disk.img --fixed explore-ob-kb-sqlite-3510300-pin-disk.img --runs 3000 --workers 2 --batch 16 --max-wall 12600 --arms workload,both --salt 0x5D7E --vcpus 2 --turns --build ~/hydo-sqlsmp/build --snap-dir /var/tmp/sqlsmp/snap --wall-secs 2400 --stop-after 1 --out /var/tmp/sqlsmp/hunt2`
(4 vCPUs: `--vcpus 4 --salt 0x4C9A --max-wall 5400 --out /var/tmp/sqlsmp/hunt4`; default
turns: `--vcpus 4 --salt 0x4D00 --max-wall 2700 --out /var/tmp/sqlsmp/hunt4d`, without
`--turns`).
`~/hydo-sqlsmp/build` holds links to `~/hydo/build`'s `bzImage`, `initramfs-kafka.cpio.gz` and
the two `-pin` images, plus the diagnostic image `explore-ob-kb-sqlite-cpudiag-3510200-disk.img`
(sha256 `3fe3c705…`, built by `hydo-build hydo-cpudiag.yaml` on the build machine).
