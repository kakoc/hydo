# Where the time goes when hydo explores (measured 2026-10-30)

This note measures the cost of one explorer run and of one campaign on the bare-metal machine
(the bare-metal machine: Xeon E3-1270 v6, 4 cores with hyper-threading = 8 hardware threads, 32 GB, two
rotating disks in RAID 1, the main `hvx.ko` built from `hvx/hvx.c` as on `main`), so that the Rust
rewrite of the explorer and the engine runner aims at the real costs. Nothing in the product
changed: the engine's measurement code is off unless an environment variable turns it on, and every
fingerprint it produced was checked against the unchanged engine (section 10).

## 1. Summary (ranked)

**What dominates one run.**

1. **Writing and re-reading snapshot data through a rotating disk.** The engine keeps its save
   buffers (S0, S1, and the data disk carried across a power cut) in files under
   `/var/tmp/hvx-snap`, written with `fdatasync` and then dropped from the page cache with
   `POSIX_FADV_DONTNEED`. On this machine that file is on a rotating disk. A power-cut run of the
   stack pays 5.4 s to write the 512 MiB data partition and 6.1 s to read it (and S0) back: 11.6 s
   of a 35 s run (33 %). The same step is a 0.1 s `memcpy` when the buffer stays in memory
   (`HVX_SNAP_MALLOC=1`, section 4). For Redis the same files cost 1.9 s of a 5.7 s power-cut run. The worker's boot pays 60–80 s of its 87–99 s for the same
   reason (section 3).
2. **The guest itself.** Inside `HVX_RUN` (workload + check + recovery boot) a stack run spends
   4.3 s without a power cut and 14.8 s with one (about 40 % of the run). A Redis run spends only
   0.15 s in the guest (3–6 % of its run).
3. **Fingerprints.** Every run hashes all of guest RAM and the data partition two times (end of
   workload, end of check or recovery) with a byte-wise FNV-1a: 1.07–1.41 s per GiB, so 2.4 s per
   Redis run (60–78 % of a run without a power cut) and 4.3 s per stack run (37 % of a stack run
   without a power cut). FNV-1a skips all-zero pages exactly (a zero page multiplies the state by
   P^4096); with that trick the RAM hash of a Redis guest drops from 1.07 s to 0.145 s with the
   same value, so the fingerprints stay byte-for-byte the same (section 5).
4. **Creating a fresh VM for every run and copying everything back.** `vm_fresh` + restore +
   teardown cost 0.5 s per Redis run and 1.7 s per stack run (twice for a power-cut run). Most of
   it is the module zeroing and freeing 1 GiB of RAM pages and a page array as large as the whole
   disk image (2.3 GiB for the stack), and a `memcpy` of the whole image into it.
5. **Python and the console.** The explorer's Python side is hidden: the next `RUN` line always
   arrived while the engine was still tearing the previous VM down (0.2 s), so the engines never
   waited for Python. Console handling costs under 1 ms per run.

**What dominates a campaign.**

1. **First runs.** Over 116 earlier campaigns on the machine (22,717 runs), 76.5 % of all runs
   were first runs, 13.5 % confirmation re-runs, 9.1 % shrink candidates and 0.9 % runs of the
   minimal schedule; the first sweep took 68 % of the wall time. In swarm campaigns, which keep
   finding failures, re-runs and shrinking are more than half of the runs (section 8).
2. **Worker boot.** Each worker boots once: 42–70 s for Redis and 87–99 s for the stack today, of
   which 60–80 % is writing S0 and S1 to the rotating disk; 5 s and 19 s with the save buffers in
   memory. In the measured 40-schedule campaigns that was 16–19 % of the wall time.
3. **Batch barriers, not Python.** `explore.py` used 0.1 s of CPU for a whole campaign. What costs
   time is that each batch (the first sweep, the re-runs, every shrink round) waits for its
   slowest run: 3 % of worker time in the Redis campaign, more when a batch is smaller than the
   number of workers.

**Which redesign ideas pay, and how much.** Baseline: the stack campaign's mean run, 19.7 s
(26 runs without and 15 with a power cut: 8.2 s in the guest, 4.3 s fingerprints, 4.1 s save files
on the rotating disk, 3.0 s VM creation, restore copies and teardown), and the Redis campaign's
2.5 s (no cut) and 5.7 s (cut). Confidence in brackets.

1. **Keep every snapshot and the carried data disk in memory, never in a file.** Saves 4.1 s of
   the stack's 19.7 s mean run (21 %; 11.6 s of each power-cut run), 1.9 s of each Redis
   power-cut run, and cuts the worker boot from 87–99 s to 19 s (stack) and from 42–70 s to 5 s
   (Redis). Measured directly with the existing `HVX_SNAP_MALLOC=1` switch, same fingerprints:
   a stack power-cut run went from 34.9 s to 14.1 s. Costs 2.1–3 GiB of memory per worker, which
   sets the worker count (below). Needs no module change. [high]
2. **Hash zero pages in one step (exact FNV-1a).** Saves about 3.1 s per stack run (16 %) and
   about 2.1 s per Redis run (most of a Redis run), with every recorded fingerprint unchanged.
   Measured: Redis RAM hash 1.07 s → 0.145 s, stack 1.05 s → 0.52 s, same values. [high]
3. **Fork runs from one in-memory snapshot with copy-on-write, or keep one VM per worker and reset
   only dirtied pages.** A run changes 0.4 % (Redis) to 11–29 % (stack) of guest RAM and under
   1.5 % of the data partition, while today every run zeroes, copies and frees 1 GiB of RAM and the
   whole disk image. Saves about 1.6 s per stack VM and 0.4 s per Redis VM, about 2.2 s per stack
   run (11 %; a power-cut run builds two VMs) and 0.5 s per Redis run. Needs module work. A smaller
   first step with the same order of saving for the stack: share the read-only root partition
   between VMs instead of copying 1.75 GiB into each (0.9 s of the 1.2 s VM creation). [medium-high]
4. **Workers sized to the machine.** A worker keeps one hardware thread busy and holds, per
   worker, the VM's kernel memory (1.1 GiB for Redis, 3.3 GiB for the stack, because the whole
   disk image is copied in) plus its save buffers (2.1 GiB and 3 GiB, plus 0.5 GiB while a stack
   power-cut run carries its data disk). On this 8-thread, 32 GB machine Redis in memory scaled to
   3.7× at 4 workers and 5.2× at 6; the stack with file buffers peaked at 2.8× with 4 workers and
   fell to 2.2× at 6, when the save files no longer fitted in the page cache. The worker count
   should be computed from the image: about min(hardware threads, free memory ÷ per-worker
   memory), which is 6–7 for Redis and 4 for the stack on this machine (the fixed default of 3
   fits neither). Copy-on-write forking (idea 3) shrinks the per-worker memory to the dirtied
   pages, after which the thread count alone decides. [high for the measurements, medium for the
   rule]
5. **Strategies that react to results as runs finish.** `explore.py` itself is not the cost
   (0.1 s CPU per campaign); the batch barriers are (3 % of worker time in the Redis campaign, more
   in small shrink rounds), plus shrink candidates that run after the round's first success and are
   thrown away. Worth doing in the Rust explorer because it is simple there, not for a large
   saving. [medium]
6. **Cheap mid-run branching.** Only shrink candidates and timing variants can use it (about 9 %
   of runs in a typical campaign, up to 36 % in swarm campaigns); confirmation re-runs must run
   from S1 to prove determinism. The guest is 40 % of a stack run and 3–6 % of a Redis run, so
   skipping a shared prefix saves at most a few percent of a typical campaign's time, more only in
   targeted shrinking or timing exploration of long workloads. [medium]

Together, 1 + 2 + 3 bring the stack's mean run from 19.7 s to about 10 s (the guest's 8.2 s plus
about 1.2 s of fingerprints and a few tenths of a second of copying), and a Redis run from 2.5–5.7 s
to about 0.6 s (0.15 s guest, 0.3 s fingerprints). After that the guest is the cost: see section 4
for how much of the guest time is the deterministic tick landing.

## 2. How it was measured

- **The engine's own clock.** `hvx/hvx_explore.c` got measurement code that runs only with
  `HVX_EXPLORE_PHASES=1`. It prints one `PHASES <tag> ...` line per schedule on stderr (never on
  the `RESULT` line) with the host wall time of every phase, and `PHBOOT` lines for the worker's
  boot. Around the workload, the check and the recovery boot it reads the module's own counters:
  the retired-exit clock (`icount` from `HVX_GET_SNAPSHOT`), PMU branches and ticks
  (`HVX_GET_PMU`; one tick is 2^20 guest branches and 2 ms of guest time), the virtual TSC, and
  from `HVX_GET_FTSTAT` the time spent inside the VMX section and the number of VM entries, split
  into single-step landings (MTF steps that land a tick on its exact branch) and free-running
  entries. `HVX_EXPLORE_DIRTY=1` also compares guest RAM and the data partition, 4 KiB page by
  page, with the snapshot the run started from (S1, or S0 for a recovery boot); this reads the
  save buffers and the RAM mapping only. `HVX_EXPLORE_ZSKIP=1` hashes guest RAM a second way
  (section 5) and checks that the value is the same. The time these checks take is booked
  separately (`dirtyscan`) and left out of every per-phase number below.
- **Separate probe.** `perf/vmcost.c` creates a VM exactly as `vm_fresh` does and times each step
  (VM creation, disk page array, image copy, kernel load, snapshot ioctls, FNV over RAM, the save
  files written and read cold and warm, teardown) in a VM that never runs.
- **Drivers.** `perf/serve.sh` feeds a fixed list of schedules to one engine in serve mode;
  `perf/hvx-run-phases` is a drop-in `HYDO_HVX_RUN` that only adds the environment variables, so
  `explore.py` ran unchanged; `perf/pyrusage.py` runs `explore.py` unchanged and prints its own
  CPU time; `perf/scale.sh` runs one scaling point (and holds the quiet window for more than two
  workers); `perf/phases.py` and `perf/campaign_mix.py` summarise the logs; `perf/drive.py` times
  the Mac engine from outside.
- **Images.** The stack: `explore-binlog-ret-xr-nq-disk.img` (2.3 GiB image, 512 MiB data
  partition, 1 GiB guest RAM). Redis: `explore-ob-redis-nq-disk.img` with
  `hvx/onboard/examples/redis/hydo.yaml` (129 MiB image, 64 MiB data partition, 1 GiB guest RAM).
- **Schedules.** Fixed lists in `perf/sched/` (clean, one kill, one power cut, kill + power cut,
  a network delay), and full campaigns: the stack with `--strategy process --more-faults`, seeds
  1–40, 2 workers; Redis with `--strategy swarm`, seeds 1–40, 2 workers (it finds the documented
  relaxed-`appendfsync` loss, so it re-runs and shrinks).
- Results are under `/var/tmp/perf/` on the machine.

## 3. One-time cost per worker: boot to S0 and S1

Seconds of host wall time. "Alone" is one engine on an idle machine; "2 workers" is one of the two
engines of a campaign, booting at the same time as the other. "In memory" is the same engine with
`HVX_SNAP_MALLOC=1` (the save buffers in `malloc`ed memory instead of files; an existing switch).

| step | Redis alone | Redis, 2 workers | stack alone | stack, 2 workers | stack in memory |
|---|---|---|---|---|---|
| create VM, copy image, load kernel | 1.2 | 1.2 | 1.5 (24.6 the first time: the 2.3 GiB image read from disk) | 1.5 | 1.3 |
| guest boots to S0 | 0.79 | 0.76 | 0.73 | 0.8 | 0.73 |
| write S0 (RAM + data partition) | 25.7 | 21.0 | 25.4 | 44.0 | 1.0 |
| fingerprint for the S0 log line | 1.2 | 1.2 | 2.6 | 2.2 | 2.1 |
| guest provisions to S1 | 0.07 | 0.07 | 11.7 | 12.7 | 11.3 |
| fingerprint for the S1 log line | 1.2 | 1.2 | 2.2 | 2.2 | 2.1 |
| write S1 | 39.5 | 16.3–24.6 | 19.3 | 34–37 | 0.7 |
| **READY** | **69.8** | **42–46** | **86.8** | **99.2** | **19.4** |

- The guest's own work is under 1 s for Redis and 12.4 s for the stack (S0: 871,000 exits,
  230 ticks; S1: 7.2 million exits, 2,996 ticks, 11.7 s inside the VMX section).
- 60–80 % of the boot is writing 2 × (1 GiB RAM + the data partition) to the rotating disk with
  `fdatasync`. With the buffers in memory the stack boots in 19.4 s, and Redis would boot in about
  4 s (1.2 + 0.8 + 2.4 of log-line fingerprints).
- The two fingerprints only feed the `S0 at ... state` / `S1 at ... state` log lines: 2.4 s
  (Redis) and 4.3 s (stack) per worker boot.

## 4. Phase breakdown per run

Mean seconds of host wall time per run, from the two campaigns (2 workers each, save buffers in
files as today). "Restore" is the warm case except where the mean includes each worker's first,
cold restore (10.8 s for the stack, 18 s for Redis: S1 read back from the rotating disk).

| phase | stack, no cut (26 runs) | stack, power cut (15) | Redis, no cut (31) | Redis, power cut (47) |
|---|---|---|---|---|
| new VM (`vm_fresh`: create, zeroed disk array, image copy, kernel) | 1.21 | 1.18 | 0.16 | 0.15 |
| restore S1: RAM (1 GiB) | 0.67 (warm 0.26) | 0.98 (warm 0.27) | 0.27 | 0.66 (warm 0.26) |
| restore S1: data partition | 0.28 (warm 0.06) | 0.36 (warm 0.06) | 0.008 | 0.03 |
| restore: snapshot struct, console, write cache | < 0.001 | < 0.001 | < 0.001 | < 0.001 |
| workload (inside `HVX_RUN`) | 4.16 | 8.71 | 0.09 | 0.08 |
| fingerprint after the workload | 2.15 | 2.15 | 1.20 | 1.20 |
| check in the same VM (inside `HVX_RUN`) | 0.16 | — | 0.04 | — |
| fingerprint after the check | 2.14 | — | 0.66 * | — |
| save the flushed data partition (file + `fdatasync`) | — | 5.45 | — | 0.87 |
| tear down the workload VM | — | 0.21 | — | 0.08 |
| new VM for the recovery boot | — | 1.17 | — | 0.15 |
| restore S0 + the flushed data partition | — | 6.14 | — | 1.08 |
| recovery boot and check (inside `HVX_RUN`) | — | 6.09 | — | 0.08 |
| fingerprint after the recovery | — | 2.14 | — | 1.20 |
| tear down | 0.21 | 0.21 | 0.08 | 0.08 |
| console reading (`HVX_GET_CONSOLE`, 64 KiB per call) | < 0.001 | 0.001 | < 0.001 | < 0.001 |
| **total** | **11.0** | **34.9** | **2.5** | **5.7** |
| of it in the guest (`HVX_RUN`) | 4.3 (39 %) | 14.8 (42 %) | 0.13 (5 %) | 0.16 (3 %) |
| of it fingerprints | 4.3 (39 %) | 4.3 (12 %) | 1.9 (74 %) | 2.4 (42 %) |
| of it rotating-disk save files | — | 11.6 (33 %) | — | 1.9 (34 %) |
| of it VM creation, restore copies, teardown | 2.4 (21 %) | 4.1 (12 %) | 0.5 (20 %) | 1.1 (20 %) |

\* Some Redis swarm runs end with a verdict during the workload (for example a lying disk), and
then the engine reuses the first fingerprint.

The `secs=` value that the engine reports on its `RESULT` line, and that `explore.py` averages as
`mean_run_s`, starts after the new VM and the restore, so it leaves out 0.5 s (Redis) to 1.8 s
(stack) of every run.

**The same stack schedules, one engine alone** (`perf/sched/stack.txt`, warm, file-backed; total
without the measurement's own time):

| schedule | new VM + restore | workload | fingerprints | check | save, new VM, restore for the recovery | recovery | teardowns | total |
|---|---|---|---|---|---|---|---|---|
| clean `run` (first run: cold restore) | 1.3 + 12.3 | 0.46 | 4.3 | 0.21 | — | — | 0.21 | 18.7 (about 6.7 warm) |
| `K=mysql@10+2` | 1.5 | 2.16 | 4.3 | 0.20 | — | — | 0.21 | 8.3 |
| `C=20` (power cut) | 1.5 | 0.26 | 4.2 | — | 14.4 + 1.2 + 21.7 (S0 still cold) | 6.1 | 0.4 | 50.2 |
| `K=mysql@24+2 C=20 P=…` | 1.5 | 0.35 | 4.2 | — | 15.1 + 1.2 + 10.6 | 6.0 | 0.4 | 39.4 |
| `N=mysql@21-28:delay,177ms` | 1.4 | 0.81 | 4.3 | 0.19 | — | — | 0.21 | 7.0 |
| `C=20`, buffers in memory | 1.5 | 0.26 | 4.2 | — | 0.24 + 1.1 + 0.42 | 5.9 | 0.4 | 14.1 |

The 6-second recovery boot after a power cut is the same in every stack schedule (1,575–1,603
ticks, 3.8 million exits): it boots MySQL, Kafka and Redis on the flushed disk and runs the oracle.

**Inside the guest phases** (`HVX_GET_FTSTAT`, the stack's 37 schedules of the 1-worker scaling
point and Redis's 49 of the 2-worker point; one tick is 2 ms of guest time):

| | exits per run | guest time per run | host time in the VMX section | host s per guest s | µs per exit | share of VMX time spent single-stepping onto exact tick points |
|---|---|---|---|---|---|---|
| stack workload | 3.9 million | 1.44 s (720 ticks) | 4.73 s | 3.3 | 1.22 | 11 % |
| stack recovery boot | 3.8 million | 3.18 s (1,590 ticks) | 5.80 s | 1.8 | 1.53 | 18 % |
| stack check | 110,000 | 0.05 s | 0.15 s | 3.1 | 1.40 | 11 % |
| Redis workload | 137,000 | 0.03 s | 0.14 s | 4.4 | 1.02 | 9 % |

The guest phases cost about 1.0–1.5 µs of host time per VM exit (exit handling, entry, and the
guest code up to the next exit), and the stack's workload makes 2.7 million exits per guest
second. One stack schedule with a timed kill made 35 million exits in 3.3 s of guest time and took
33 s. The deterministic tick landing (single steps up to the exact branch) is only 9–18 % of it.
So once the fixed costs are gone, the guest's exit rate is what to attack; which exit reasons
dominate (`PAUSE` exits are on for the stack, `HVX_VM_PAUSE_EXIT`) is not visible through the
existing ioctls (section 11).

**Python and the protocol.** `explore.py`'s own CPU time over the whole Redis campaign (78 engine
runs, re-runs and shrinking, report) was 0.13 s (`perf/pyrusage.py`). The engines never waited for
it between two runs of a batch: the next `RUN` line was already there when the engine finished
tearing down the last VM (median idle gap 0.000 s). The waits that did happen were batch
barriers: `Pool.run_all` hands out a batch (the first sweep, the re-runs, one shrink round) and
waits for all of it, so one worker idles at the end of every batch. In the Redis campaign that was
7.4 s and 5.8 s of 243 s per worker (3 %); in the stack campaign, whose single batch was the first
sweep, it was 0.

## 5. Fingerprint cost and the exact zero-page shortcut

`fingerprint()` (in `hvx/hvx_stack_verify.c`) hashes the console, the snapshot struct, all guest
RAM and the data partition with FNV-1a one byte at a time. The multiply chain allows no
parallelism, so it runs at 1.07–1.41 s per GiB on this CPU (about 4–5 cycles per byte; 1.38 s
in `perf/vmcost`, 1.04–1.10 s inside the engine). One fingerprint costs 1.20 s for Redis (1 GiB
RAM + 64 MiB) and 2.15 s for the stack (1 GiB + 512 MiB). A run takes two (after the workload,
and after the check or the recovery), and every worker boot takes two more for its log lines.

FNV-1a can skip a zero byte exactly: `h = (h ^ 0) * P = h * P`, so an all-zero 4 KiB page is
`h * P^4096 (mod 2^64)`, one multiplication. `fnv_zskip()` in `hvx/hvx_explore.c` does that, and
`HVX_EXPLORE_ZSKIP=1` compared it with the byte-wise hash on the real end-of-workload RAM of every
run: always the same value, and

| guest RAM at the end of the workload | byte-wise FNV | zero-page skipping | non-zero pages |
|---|---|---|---|
| Redis (78 runs of the swarm campaign) | 1.07–1.10 s | 0.143–0.146 s | about 9 % |
| stack (5 schedules) | 1.04–1.05 s | 0.52 s | about 45 % |

The data partition is 0.2–1 % (Redis) and 10 % (stack) non-zero, so its hash would drop from
0.07 s and 0.53 s to almost nothing. A Rust engine that keeps today's fingerprint definition, as
the language policy requires, would spend about 0.3 s instead of 2.4 s per Redis run and about
1.2 s instead of 4.3 s per stack run on fingerprints, and every recorded baseline stays valid.

Going further needs a decision: a hash that is fast per page and combined in order (for example
xxh3 per page) would cost under 0.1 s per GiB, or with dirty-page tracking only the changed pages,
but it changes every recorded fingerprint. Not computing fingerprints for passing first runs at all
would save the rest, but the fingerprint of a first run is what a later re-run is checked against,
so this only works if failing runs are re-run from scratch twice instead.

## 6. Memory, snapshot sizes and dirty pages

**Per VM.** `HVX_CREATE_VM` allocates and zeroes every guest RAM page up front (1 GiB: 0.08–0.14 s),
and `HVX_SET_DISK` a zeroed page array as large as the whole image (Redis 129 MiB: 0.01 s; stack
2.3 GiB: 0.18 s), into which `vm_fresh` then copies the whole image (0.05 s; 0.87 s). For the
stack 1.75 GiB of that copy is the read-only root partition, which no run ever changes. Teardown
(unmap + close) frees it all again: 0.08 s (Redis), 0.21 s (stack). Kernel-side memory per VM is
therefore 1.1 GiB (Redis) and 3.3 GiB (stack); the engine's resident set shows 1.3 GB and 5.8 GB
(the second number also counts the 2.3 GiB image file mapping, which the page cache shares
between workers).

**Per worker, snapshots.** S0 and S1 each hold all of RAM (1 GiB), the data partition (64 MiB or
512 MiB), the snapshot struct (2,432 bytes), the console up to that point (18–24 KB) and the
disk's volatile write cache. A worker therefore keeps 2.1 GiB (Redis) or 3 GiB (stack) of save
buffers, today as files that are written once with `fdatasync` and dropped from the page cache
(`POSIX_FADV_DONTNEED`), so the first restore of each reads them back from the rotating disk
(10–22 s per GiB, `perf/vmcost`) and later restores find them in the page cache only because
nothing else needed the memory. A power-cut run writes its flushed data partition the same way
and reads it back cold every time (that is the 5.4 + 6.1 s of section 4).

Most of the snapshot is zero: at S1, 23,007 of 262,144 RAM pages are non-zero for Redis (90 MiB,
9 %) and 119,148 for the stack (465 MiB, 45 %); the data partition is 37 of 16,384 pages (Redis)
and 12,999 of 131,072 pages (stack, 10 %) non-zero.

**Dirty pages.** Pages that differ from S1 at the end of a run (compared byte for byte, so a page
written back with the same content is not counted; the count is a lower bound of the pages
written):

| | RAM pages changed (of 262,144) | data-partition pages changed |
|---|---|---|
| Redis, end of workload (78 runs) | mean 1,100–1,220 (4.5 MiB, 0.4 %), max 1,543 | mean 27, max 63 (of 16,384) |
| Redis, end of check | mean 1,200 | mean 35 |
| Redis, recovery boot against S0 | mean 2,100, max 2,453 | — |
| stack, end of workload (41 runs) | mean 29,100 (114 MiB, 11 %), min 10,000, max 74,900 (29 %) | mean 1,900 (7.4 MiB), max 4,051 (of 131,072) |
| stack, end of check | mean 33,800 (13 %), max 75,200 | mean 2,300 |
| stack, recovery boot against S0 | mean 94,800 (370 MiB, 36 %) | — |

So a run writes 0.4 % (Redis) to 11–13 % (stack, up to 29 %) of guest RAM and 0.2–1.4 % of the
data partition, while the engine copies 100 % of both, plus the whole disk image, for every run.
Copy-on-write forking from one in-memory S1 (or resetting only the pages a run dirtied in a VM that
is kept) would copy about 5 MiB per Redis run and 120–300 MiB per stack run instead of
1.1 GiB + 129 MiB and 1.5 GiB + 2.3 GiB. At 1–2 µs per copy-on-write fault that is a few
milliseconds for Redis and 0.05–0.15 s for the stack, against 0.5 s and 1.7 s today (2.9 s for a
stack power-cut run, which builds two VMs). It needs module support (write-protected EPT entries
or a dirty log); none of the existing ioctls can tell which pages a run wrote, which is why the
counts above come from comparing against the saved copy.

## 7. Throughput scaling with the number of workers

`explore.py --strategy process --no-shrink` on a fixed seed range with 1, 2, 4 and 6 workers
(`perf/scale.sh`; 4 and 6 inside the quiet window). "Runs/min" counts runs that finished after
every worker had started its first run, so the worker boot is not in it; "run" is the mean run
time without the measurement's own time; CPU is from `vmstat` (8 hardware threads = 100 %).

**Redis, save buffers in memory** (`HVX_SNAP_MALLOC=1`, seeds 1–100; this isolates CPU and memory):

| workers | runs/min | speed-up | mean run | new VM | restore RAM | workload | fingerprints | CPU user / system / idle / I/O wait |
|---|---|---|---|---|---|---|---|---|
| 1 | 18.0 | 1.00 | 3.34 s | 0.15 | 0.38 | 0.13 | 2.35 | 10 / 3 / 87 / 0 % |
| 2 | 34.9 | 1.94 | 3.41 s | 0.15 | 0.39 | 0.14 | 2.40 | 19 / 6 / 75 / 0 % |
| 4 | 67.2 | 3.73 | 3.53 s | 0.17 | 0.41 | 0.14 | 2.47 | 37 / 12 / 50 / 1 % |
| 6 | 94.0 | 5.23 | 3.78 s | 0.20 | 0.48 | 0.17 | 2.54 | 53 / 20 / 27 / 0 % |

Each worker keeps one hardware thread busy, so throughput grows with the worker count up to the 4
physical cores and then by about 40 % for the next two workers, which share cores with
hyper-threading. The phases that slow down most are the memory-bound ones (new VM +35 %, RAM
restore +25 % from 1 to 6 workers); the fingerprints slow down by 8 %. There is no lock: the
module has no global run lock (each VM's run section disables preemption on its own CPU), and
`hvx-run`'s lock is shared. With 8 workers the bare-metal machine would run out of hardware threads.

**Stack, save buffers in files as today** (`--more-faults`, seeds 1–36; per-phase means in
seconds):

| workers | runs/min | speed-up | worker boot | mean run | new VM | restore RAM | restore data | save flushed disk | restore for recovery | workload | CPU user / system / idle / I/O wait |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 3.67 | 1.00 | 44 s | 16.4 | 1.13 | 0.45 | 0.15 | 1.51 | 1.52 | 4.74 | 4 / 7 / 87 / 3 % |
| 2 | 6.20 | 1.69 | 97 s | 19.8 | 1.17 | 0.87 | 0.29 | 2.64 | 2.80 | 4.96 | 10 / 11 / 61 / 18 % |
| 4 | 10.42 | 2.84 | 135 s | 22.9 | 1.29 | 1.52 | 0.53 | 2.78 | 4.21 | 5.14 | 7 / 15 / 48 / 30 % |
| 6 | 7.91 | 2.16 | 221 s | 44.2 | 2.10 | 8.64 | 4.15 | 7.60 | 8.93 | 5.11 | 5 / 12 / 27 / 56 % |

{{SCALE_SM}}

Here the rotating disk is the limit. The guest phases do not slow down (workload 4.7–5.1 s at
every worker count), but every phase that touches the save files does: the flushed-disk save and
the recovery restore double from 1 to 2 workers, and at 6 workers the restore of S1 itself reads
from the disk (8.6 s instead of 0.45 s), because 6 × 3.3 GiB of VM memory plus 6 × 3 GiB of save
files no longer fit in the page cache of a 32 GB machine. Throughput peaks at 4 workers (2.8×) and
falls at 6 (2.2×, I/O wait 56 %). The boot of 6 workers takes 3.7 minutes, almost all of it
writing 18 GiB of save files. Each sample has 6–36 runs per worker; the drop at 6 is far larger
than that noise.

## 8. Where a campaign spends its runs

**The two campaigns measured here** (`perf/campaign_mix.py`):

| campaign | first runs | confirmation re-runs | shrink candidates | runs of the minimal schedule (2 each) | worker boot | first sweep (incl. boot) | total wall |
|---|---|---|---|---|---|---|---|
| stack, `process --more-faults`, seeds 1–40, 2 workers | 40 (+1 FLUSH-count probe) | 0 | 0 | 0 | 99 s | 414 s | 535 s |
| Redis, `swarm`, seeds 1–40, 2 workers | 40 | 17 | 10 | 8 | 46 s | 171 s | 286 s |

The stack campaign found nothing (as M67 and M71 report for this image), so every run was a first
run, and the rest of the wall time (121 s) is the worker boot's share that falls outside the
sweep timer plus report writing. In the Redis campaign 17 of 40 schedules failed (the documented
relaxed-`appendfsync` loss): the first runs are 53 % of the 75 runs, and 40 % of the wall time
comes after the first sweep.

**All 116 earlier campaigns on the machine** (every `report.json` under `/var/tmp`, 22,717 runs):

| strategy | campaigns | runs | first runs | confirmation re-runs | shrink candidates | minimal-schedule runs | first sweep's share of wall time |
|---|---|---|---|---|---|---|---|
| `process` | 32 | 2,695 | 80.7 % | 9.7 % | 8.3 % | 1.3 % | 73 % |
| `random` | 12 | 840 | 73.3 % | 15.4 % | 10.1 % | 1.2 % | 71 % |
| `swarm` | 27 | 4,289 | 42.8 % | 19.2 % | 35.7 % | 2.3 % | 44 % |
| sweeps (`--sweep ...`) | 32 | 12,653 | 90.0 % | 10.0 % | 0 % | 0 % | 86 % |
| older runs without a recorded strategy | 13 | 2,240 | 61.0 % | 26.4 % | 9.7 % | 2.9 % | 63 % |
| **all** | 116 | 22,717 | 76.5 % | 13.5 % | 9.1 % | 0.9 % | 68 % |

So in a typical campaign three runs in four are first runs, and re-runs plus shrinking take a
quarter of the runs and about a third of the wall time; in a campaign that keeps finding things
(swarm) more than half of the runs are re-runs and shrink candidates. The shrinker runs its
candidates in batches (`Pool.run_all`) and takes the first one in list order that still fails, so
every round waits for its slowest candidate, and candidates after the first success are wasted
work. Most shrink candidates share the schedule's beginning with the failing run, which is what
mid-run branching could reuse (section 1, ideas).

## 9. The Mac backend (hvmx), for comparison

On this Mac (M2 Pro), `hvmx serve` (built 2026-10-24) with the Redis OCI example
(`explore-ob-redis-oci-arm64`, 1 GiB guest RAM), timed from outside with `perf/drive.py` (no
per-phase instrumentation; the engine's `secs=` starts after the restore, like on Linux):

| | wall | engine `secs=` | difference (VM re-creation + RAM copy back) |
|---|---|---|---|
| boot to READY (S0 and S1) | 2.25 s | 2.1 s | — |
| clean `run` | 1.43 s | 1.1 s | 0.33 s |
| `K=redis@4+3` | 1.44 s | 1.1 s | 0.34 s |
| `C=12` (power cut) | 1.73 / 2.38 s | 1.4 / 1.9 s | 0.33 / 0.48 s |
| `K=… C=20 P=… T=…` | 1.87 s | 1.5 s | 0.37 s |

The Mac engine boots 20–30 times faster than the Linux engine because its snapshots are an
unlinked file mapped into memory with a plain `memcpy` (no `fdatasync`, no cache drop, and an SSD
underneath), and it hashes 8 bytes per step (a different fingerprint definition from Linux). A
Redis run costs 1.4–2.4 s there against 2.5–5.7 s on the Linux machine. Its maximum resident set
was 3.8 GB (1 GiB RAM, two snapshots, the disk).

## 10. Checks that the measurement changed nothing

- Redis, four schedules: the instrumented engine with `HVX_EXPLORE_PHASES=1`, and again with
  `HVX_EXPLORE_DIRTY=1`, gave the same `fp` and `wfp` as an engine built from `main`'s
  `hvx_explore.c` (`c03f7ae36c138ea1`, `38f9db20e2556d02`, `4d18552600c6eb32`,
  `18c1f834adf8a8f6`).
- Stack: the instrumented engine reached the S0 and S1 states recorded by an earlier campaign
  (`b6e19860e4a053fd`, `5e96583ac3069557`), and two of its schedules gave the fingerprints the
  unchanged engine recorded for the same schedules in `/var/tmp/nq/swarm-nq` (`b4c00cb20794a368`,
  `05054cd978eaae30`). With `HVX_SNAP_MALLOC=1 HVX_EXPLORE_ZSKIP=1` all five stack schedules gave
  the same fingerprints as with file-backed buffers.
- The zero-page-skipping hash equalled the byte-wise hash on every RAM state it was tried on
  (83 states, Redis and stack).
- With the variables unset, the engine runs none of the new code except reading the clock around
  phases that are already there.

## 11. What was not measured, and why

- **Memory-bandwidth counters.** `perf stat` with uncore events was not used; the scaling result
  (section 7) and the per-phase times show which phases slow down with more workers.
- **Where inside `HVX_RUN` the guest time goes, per exit reason.** `HVX_GET_FTSTAT` gives the time
  in the VMX section, the share spent landing ticks by single-stepping and the hardware part of
  it, but not a split by exit reason; that would need a module change.
- **Exact written-page counts.** No ioctl reports which pages a run wrote; the counts in section 6
  compare contents, which is a lower bound (a page rewritten with its old content is missed). An
  exact count needs a dirty log in the module.
- **The Mac's phases.** Only from outside; `serve.c` was not instrumented.
- **Re-runs and shrinking on the stack.** The stack campaign found no failure, so its re-run and
  shrink cost is taken from earlier campaigns on the machine (section 8), not from a new run.
- **A solid-state disk.** The machine has only rotating disks; on an SSD the save files would cost
  less, and the in-memory runs (`HVX_SNAP_MALLOC=1`) give the lower bound.
- **Branching (`BRANCH` requests), several vCPUs, and Processor Trace runs** were not measured.
- The build machine was not used.
