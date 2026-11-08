# Cost tracking: performance as deterministic work counts (experiment, 2026-11-08)

Status: an experiment with a written result (user decision 2026-11-08), not a feature. The
prototype is one host-side option, `HYDO_COST` (`hydo/crates/hydo-control/src/schedule/cost.rs`),
off by default. With it off, every recorded value comes back (section 4.1).

## 1. The question and the short answer

Time inside a hydo guest is virtual, so hydo cannot measure real latency. A run is deterministic,
so counts of work are exact instead: guest branches (hvx's PMU branch counter), VM exits, disk
writes, flushes and bytes, packets, page faults, forks, context switches, and memory per process.
The experiment asked whether those counts are exactly stable across runs, and whether real
changes move them visibly. It also asked which engines can measure what.

The short answer:

- **Stable: yes, exactly.** Every one of about 160 to 300 counters per run was identical in five
  fresh boots and five runs from S1, on all three engines. Fresh-boot runs and S1 runs agreed with
  each other too (section 4.2).
- **Sensitive: yes, to the right things.** Redis's `appendfsync` setting moves flushes from 40
  to 0 and written KiB from 328 to 0. The same 2.0 flushes and 16.4 KiB per acknowledged write
  appeared on all three engines. A Redis restart costs 28 to 77 % more work. A power cut's
  recovery boot of the stack costs 9 to 31 times the workload it follows. An extra unsynced write
  per request shows on every engine. Extra CPU work per request shows **only on hvx**, and only
  above a noise floor (section 4.3).
- **The noise floor.** A run of an image is exactly repeatable, but a different program is a
  different run. The extra work shifts the timing, and the timing shift moves everything that
  depends on time. In this experiment that moved the branch total by about 0.8 %. A regression
  smaller than that floor is real in the counts but cannot be told apart from the shift.
- **Across engines,** device counts that do not depend on time (flushes and bytes per write, disk
  reads at recovery) are equal on all three engines. Engine clocks and anything driven by timers
  agree in direction but not in size (section 4.4).
- **Verdict: worth building as a small, opt-in part of CI stage 2.** Track device counts and
  engine clocks per schedule against the base commit, and show only changes above a threshold.
  Do not present the numbers as latency. Section 6 has the recommendation and the cost.

## 2. What counts exist, per engine

| Counter | hvx (bare metal) | kvm | hvm (Mac) | Where it comes from | Disturbs the run? |
|---|---|---|---|---|---|
| Guest conditional branches retired | yes, exact (PMU) | no | no | `HVX_GET_PMU`, already read by `HVX_EXPLORE_PHASES` per phase: workload, checks, recovery | no |
| Engine clock (exits; on hvx the instruction clock) | yes | yes | yes | every stop's `at` in the control transcript; hvx `PHASES` `w_exits`; kvm `HYDO_KVM_DIAG` | no |
| VM exits by kind (I/O, MMIO, HLT, IRQs, clock reads) | partly (module statistics) | yes | yes | kvm `vmm.stats()`, written by `HYDO_KVM_DIAG` | no |
| Disk requests, reads, writes, flushes, volatile-cache writes | requests only, and only with disk faults on (`fault_req_seq`) | yes | yes | hydo-hvm's virtio `VStats`, shared by kvm and hvm; today printed only by `hydo-hvm hvm` | no |
| Network frames on the virtio NIC | tx frames (`vnet_tx_count`) | yes | yes | the same device models; services talk over loopback or veths, so these see almost nothing | no |
| Disk I/O per partition (requests, sectors, flushes) | yes | yes | yes | guest `/proc/diskstats`, read by the probe | after the verdict: no |
| Packets and bytes per interface (loopback, veths) | yes | yes | yes | guest `/proc/net/dev` | after the verdict: no |
| Forks, context switches, interrupts, page faults, pages written back | yes | yes | yes | guest `/proc/stat`, `/proc/vmstat` | after the verdict: no |
| Memory per process (resident KiB), page faults per process | yes | yes | yes | guest `/proc/<pid>/stat` | after the verdict: no |
| CPU time per process | 10 ms ticks only | ticks only | ticks only | `/proc/<pid>/stat`; the guest kernels have no `schedstat` and no `/proc/<pid>/io` | after the verdict: no |

Two kinds of counter result from this:

- **Host-side counters** (the first four rows) cost nothing in the guest and cannot change a run.
  hvx already collects branches, exits and the instruction clock per phase behind
  `HVX_EXPLORE_PHASES`. kvm and hvm already count exits by kind and disk requests, reads, writes
  and flushes in their device models. Nothing records them in `result.json` yet.
- **Guest-side counters** (the rest) come from `/proc`, read by the prototype's probe: one busybox
  script that the host controller runs through the agent's `run` command. Its output is reported
  as `HYDO-COST <label> <line>` lines in the control transcript, at a stop whose `at` is the
  engine clock there.

### The probe moves the run it measures

The first version of the probe ran at the workload's start and end. It is guest work: a shell
and several awk processes, which on hvx is about 26,000 exits and 12 million branches per probe,
more than the whole Redis workload (1.3 times its exits, 2.1 times its branches). That work moves the timing of everything after it. On kvm it
turned the recorded VIOLATION of `disk=cut | run C=12 R=everysec` into OK: the probe gave
Redis's once-a-second fsync time to run before the power cut.

The prototype therefore probes **after each conversation's verdict** by default (`HYDO_COST=1`).
The verdict and everything before it, including the workload's fingerprint `wfp`, are those of a
run without the option (section 4.1). Only the final fingerprint changes. `HYDO_COST=full` adds the
probes at the workload's start and end and in a recovery boot before its checks. These give
exact S1 baselines and workload-only deltas, but they disturb timing as described.

The after-verdict probe has two limits. Its counts include the checks (constant per image, small
for these examples). And a power-cut run has no probe at the end of its first boot: that boot
ends at the cut, so its workload is measured only by the host-side counters.

## 3. Setup

- **Engines and machines.** hvx on the bare-metal machine (Xeon E3-1270 v6, the main `hvx.ko`,
  the PMU branch clock on, `HVX_EXPLORE_PHASES=1`). kvm on the build machine (i7-13700HX, Linux
  6.6 KVM, `HYDO_KVM_DIAG=1`). hvm on this Mac (M2 Pro). The `hydo` that ran is this branch built
  in the `ci` profile, placed in a copy of a release as `bin/hydo-cost`. One VM at a time; on the
  bare-metal machine runs waited for `/var/tmp/hydo-QUIET` and for 8 GB of free memory.
- **Images.**
  - The release Redis example: `d255fb2b…`, the same image on hvx and kvm. On the Mac it is the
    arm64 build of the same manifest, built with the installed 0.2.0 Mac release; with the option
    off it gives docs/REBASELINE.md 13.4's Mac values.
  - The stack's pinned-reproducer images: `dc8f8a89…` with the old order service and `1de40b6a…`
    with the fixed one (hvx and kvm).
  - rkv (`hydo/onboard/examples/rust-kv`) in four builds: unchanged; with an extra loop of 1,000
    or 100,000 multiply-xor rounds per PUT (a little and a lot of extra CPU work); and with an
    extra unsynced append of each record to a second file per PUT (an extra write). Each build
    exists for x86_64 (hvx and kvm, the same image bytes) and arm64 (hvm).
- **Schedules.** Redis: clean (`appendfsync always`, the default), `R=everysec`, `R=no`,
  `K=redis@5+2`, `P=redis@4+3`, and a power cut `C=12` with each setting. rkv: clean, `S=never`,
  `C=12`. Stack: clean, `K=redis@11+1`, and the pinned `K=redis@11+1 C=24`. Most schedules ran
  twice or more; the stack's fault schedules ran once in each probe mode. Each `|`-schedule carried `dseed=1` so that it also runs in `hydo explore
  --schedule`, which starts it from a worker's S1. The counts below come from 199 runs: 72 on
  hvx, 71 on kvm and 56 on hvm.
- **How a number is formed.** A guest counter is the probe after the verdict minus the same
  counter at S1 of the same image. The S1 value comes from a `HYDO_COST=full` run's start probe,
  which is the same for every schedule of an image. A guest counter therefore covers the workload,
  the retry pass and the checks. "Engine clock" is the clock at the stop that ends the workload
  minus the clock at S1, both read from the transcript. It is exits on kvm and hvm, and the
  instruction clock on hvx, which counts exits and emulated RDTSCs. "Branches" is hvx's workload
  phase.

## 4. Results

### 4.1 With the option off nothing moves, and the after-verdict probe keeps the verdict

| Engine | Schedule | Option off: fp (recorded) | wfp off | wfp after-verdict probe | Verdict off / probe / `full` |
|---|---|---|---|---|---|
| hvx | Redis `disk=none \| run` | `1893d8c55d7704d9` (= recorded) | `1421b01f…` | `1421b01f…` | OK / OK / OK |
| hvx | Redis `disk=cut \| run C=12 R=everysec` | `3520af95868b4e8f` (= recorded) | `fce0cfbd…` | `fce0cfbd…` | VIOLATION / VIOLATION / VIOLATION |
| hvx | stack pinned, old | `1dc3677fb94053cf` (= pinned) | | | VIOLATION |
| hvx | stack pinned, fixed | `ba0aa0612502a1df` (= pinned) | | | OK |
| kvm | Redis `disk=none \| run` | `bfa03053d51bec8a` (= recorded) | `3032b5e1…` | `3032b5e1…` | OK / OK / OK |
| kvm | Redis `disk=cut \| run C=12 R=everysec` | `8ed16d6da60c34c6` (= recorded) | `7cf9edeb…` | `7cf9edeb…` | VIOLATION / VIOLATION / **OK** |
| kvm | stack pinned, old | `c3419179cf1a55ba` (= recorded) | | | VIOLATION |
| hvm | Redis `disk=none \| run` | `67e5d6469f1a90a5` (= 13.4) | `4c8b96be…` | `4c8b96be…` | OK / OK / OK |
| hvm | Redis `disk=cut \| run C=12 R=everysec` | `a6870a92a1bbf0ae` (= 13.4) | `b4656567…` | `b4656567…` | VIOLATION / VIOLATION / VIOLATION |

The `full` probe flipped kvm's power-cut verdict (section 2). On hvx and hvm it kept the verdict
but changed `wfp`.

### 4.2 Stability: identical, from a fresh boot and from S1

| Engine | Schedule | Fresh boots | Runs from S1 | Distinct results (fp, wfp, verdict, every counter) | Counters per run |
|---|---|---|---|---|---|
| hvx | Redis clean | 5 | 5 | 1 | 299 |
| hvx | Redis `C=12 R=everysec` | 5 | 5 | 1 | 291 |
| kvm | Redis clean | 5 | 5 | 1 | 157–171 |
| kvm | Redis `C=12 R=everysec` | 5 | 5 | 1 | 169–183 |
| hvm | Redis clean | 5 | 5 | 1 | 202 |
| hvm | Redis `C=12 R=everysec` | 5 | 5 | 1 | 194 |

Every other schedule ran twice and gave the same values both times. On all three engines the
fresh boots and the runs from S1 gave the same fingerprints, verdicts and counters. The "counters
per run" column counts the guest's values plus the engine's. kvm's fresh-boot runs also carry the
`HYDO_KVM_DIAG` exit statistics, and its S1 runs do not, so its two rows differ.

### 4.3 Sensitivity

**Redis's durability setting and faults** (20 SETs; guest counters are the workload, the checks and
the retry pass):

| Schedule | Engine | Acked | Engine clock | Branches | Data-disk writes | KiB written | Flushes | Forks | Context switches | Page faults | Loopback packets |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `always` (clean) | hvx | 20 | 20,753 | 5,676,725 | 82 | 328 | 40 | 86 | 1,120 | 4,979 | 400 |
| `always` (clean) | kvm | 20 | 8,448 | | 82 | 328 | 40 | 86 | 823 | 4,998 | 400 |
| `always` (clean) | hvm | 20 | 12,288 | | 82 | 328 | 40 | 86 | 631 | 20,447 | 400 |
| `R=everysec` | hvx | 20 | 17,745 | 5,551,079 | 6 | 24 | 2 | 87 | 824 | 5,113 | 410 |
| `R=everysec` | kvm | 20 | 7,040 | | 10 | 40 | 4 | 87 | 595 | 5,141 | 410 |
| `R=everysec` | hvm | 20 | 6,784 | | 0 | 0 | 0 | 87 | 329 | 21,001 | 410 |
| `R=no` | hvx | 20 | 15,528 | 5,507,969 | 0 | 0 | 0 | 87 | 820 | 5,112 | 410 |
| `R=no` | kvm | 20 | 6,976 | | 0 | 0 | 0 | 87 | 596 | 5,129 | 410 |
| `R=no` | hvm | 20 | 6,656 | | 0 | 0 | 0 | 87 | 324 | 21,002 | 410 |
| `K=redis@5+2` | hvx | 18 | 26,566 | 8,194,730 | 74 | 296 | 36 | 99 | 1,181 | 5,600 | 378 |
| `K=redis@5+2` | kvm | 18 | 14,976 | | 74 | 296 | 36 | 99 | 861 | 5,651 | 378 |
| `K=redis@5+2` | hvm | 18 | 18,432 | | 74 | 296 | 36 | 100 | 613 | 21,676 | 378 |
| `P=redis@4+3` | hvx | 17 | 86,262 | 8,148,156 | 74 | 296 | 36 | 89 | 1,306 | 4,848 | 366 |
| `P=redis@4+3` | kvm | 17 | 12,544 | | 74 | 296 | 36 | 89 | 854 | 4,818 | 367 |
| `P=redis@4+3` | hvm | 17 | 18,816 | | 74 | 296 | 36 | 89 | 663 | 19,154 | 366 |

- With `always`, every engine counts 2.0 flushes and 16.4 KiB written per acknowledged write
  (40 flushes and 328 KiB for 20 SETs). With the kill and the pause, 18 or 17 SETs were
  acknowledged, and the cost per write stays at 2.0 flushes and 16.4 KiB. These are facts about
  Redis and the kernel, so they are the same on every engine.
- `everysec` flushes once per second of guest time, so its count depends on how much guest time
  the engine lets pass: 2 flushes on hvx, 4 on kvm and none on hvm, where the workload lasts a few
  hundredths of a guest second.
- A restart of Redis (`K=`) costs 44 % more branches on hvx, and 28 % (hvx), 77 % (kvm) and 50 %
  (hvm) more engine clock. It also costs 13 more forks (14 on hvm). A pause adds about the
  same branches, and on hvx it adds 4 times the engine clock, because the idle guest's timer
  ticks are exits too.
- The context-switch and page-fault totals include engine-specific kernel work (hvm's arm64
  kernel takes 4 times the page faults). They compare well between runs on one engine and badly
  between engines.

**Recovery cost after a power cut at step 12** (the recovery boot, from its first instruction to
after its verdict):

| Schedule | Engine | Verdict | Recovery clock | Recovery branches | Data-disk reads | KiB read | Writes | Flushes |
|---|---|---|---|---|---|---|---|---|
| `C=12` (`always`) | hvx | OK | 51,141 | 16,765,763 | 104 | 413 | 12 | 4 |
| `C=12 R=everysec` | hvx | VIOLATION | 45,317 | 14,993,057 | 72 | 285 | 12 | 4 |
| `C=12 R=no` | hvx | VIOLATION | 43,153 | 14,632,217 | 71 | 281 | 12 | 4 |
| `C=12` (`always`) | kvm | OK | 13,184 | | 104 | 413 | 12 | 4 |
| `C=12 R=everysec` | kvm | VIOLATION | 12,224 | | 72 | 285 | 12 | 4 |
| `C=12` (`always`) | hvm | OK | 13,120 | | 104 | 413 | 12 | 4 |
| `C=12 R=everysec` | hvm | VIOLATION | 12,096 | | 71 | 281 | 12 | 4 |

The recovery reads every acknowledged write back from the append-only file. The bytes read
measure how much survived the cut: 413 KiB with `always` against 285 with `everysec` (281 on
hvm). For the stack, recovery dominates. After the pinned schedule's cut at step 24, the
recovery boot takes 3.78 million on the instruction clock and 1,696 million branches on hvx, 31 times the
workload's 54 million branches. On kvm it takes 419,200 exits, 9.5 times the workload's 43,840.
On hvx it reads 792 (old service) and 864 (fixed) data-disk requests and writes 1,593 and 1,627.

**The stack, old against the fixed order service** (30 orders):

| Schedule | Engine | Service | Verdict | Engine clock | Branches | Data writes | Flushes | Context switches | Page faults | Loopback packets |
|---|---|---|---|---|---|---|---|---|---|---|
| clean | hvx | old | OK | 140,239 | 49,979,957 | 541 | 269 | 7,684 | 7,751 | 1,397 |
| clean | hvx | fixed | OK | 138,832 | 49,978,285 | 542 | 270 | 7,778 | 7,786 | 1,397 |
| `K=redis@11+1` | hvx | old | OK | 167,569 | 62,431,868 | 551 | 274 | 7,922 | 10,219 | 1,472 |
| `K=redis@11+1` | hvx | fixed | OK | 196,187 | 69,446,931 | 552 | 277 | 8,743 | 11,156 | 1,475 |
| clean | kvm | old | OK | 45,824 | | 505 | 269 | 7,189 | 7,702 | 1,389 |
| clean | kvm | fixed | OK | 46,784 | | 512 | 274 | 7,286 | 7,694 | 1,388 |
| `K=redis@11+1` | kvm | old | OK | 53,184 | | 506 | 268 | 7,505 | 10,216 | 1,467 |
| `K=redis@11+1` | kvm | fixed | OK | 54,848 | | 507 | 269 | 7,890 | 10,155 | 1,436 |

- On the clean path the two services cost the same: 50.0 million branches each on hvx (they
  differ by 0.003 %), and on kvm the fixed service's clock is 2 % higher. The fix changes only
  what happens after a failed cache write.
- The Redis kill costs the old service 25 % more branches and the fixed one 39 % more. On kvm the
  increase is 16 % and 17 % of the engine clock. On both engines, under the fault, the fixed
  service does more work than the old one, because it retries the cache write that the old service
  gave up on.
- The stack's guest-time totals are coarse: per-process CPU is in 10 ms ticks (mysqld 22 to 30
  ticks on hvx, 2 on kvm). They say little.

**A deliberate code regression in rkv** (20 PUTs, each with an `fdatasync`):

| Build | Engine | Engine clock | Branches | Data writes | KiB written | Flushes | Context switches |
|---|---|---|---|---|---|---|---|
| unchanged | hvx | 13,886 | 3,499,001 | 82 | 328 | 40 | 895 |
| + 1,000 rounds per PUT | hvx | 13,877 | 3,490,651 (−0.24 %) | 82 | 328 | 40 | 861 |
| + 100,000 rounds per PUT | hvx | 18,056 | 5,480,067 (+57 %) | 82 | 328 | 40 | 885 |
| + one extra write per PUT | hvx | 14,154 | 3,540,006 (+1.2 %) | 85 | 340 | 40 | 913 |
| unchanged | kvm | 7,488 | | 82 | 328 | 40 | 698 |
| + 1,000 rounds per PUT | kvm | 7,424 | | 82 | 328 | 40 | 691 |
| + 100,000 rounds per PUT | kvm | 7,424 | | 82 | 328 | 40 | 691 |
| + one extra write per PUT | kvm | 7,488 | | 85 | 340 | 40 | 725 |
| unchanged | hvm | 11,328 | | 82 | 328 | 40 | 624 |
| + 1,000 rounds per PUT | hvm | 11,328 | | 82 | 328 | 40 | 624 |
| + 100,000 rounds per PUT | hvm | 11,328 | | 82 | 328 | 40 | 624 |
| + one extra write per PUT | hvm | 11,328 | | 85 | 340 | 40 | 624 |

- **CPU work.** hvx counts 100,000 rounds per PUT as +1.98 million branches, which is the 20 ×
  100,000 loop iterations, almost exactly. kvm and hvm cannot see CPU work at all: code without
  exits takes no guest time and causes no counted event on those engines. Their two CPU builds
  give exactly the same counts as each other.
- **The noise floor.** 1,000 rounds per PUT should add 20,000 branches. Instead the total fell
  by 8,350, because the changed binary moves timing, and the timer-driven work around the
  workload moves with it: the expected +0.57 % came out as −0.24 %, a shift of about 0.8 %. The
  −0.24 % is exact and repeatable, but it is not the regression's size. A change has to be clearly above that floor to be read as
  a cost.
- **An extra write** shows on every engine as 3 more data-disk writes and 12 KiB more, the same
  numbers everywhere. The audit file is not synced, so the kernel writes it back in a few batches
  instead of 20 writes. Turning off `fdatasync` (`S=never`) drops the flushes from 40 to 0 on hvx,
  and to 1 on kvm.

### 4.4 Across engines

| Change | hvx | kvm | hvm | Agree? |
|---|---|---|---|---|
| Flushes per acked SET, `always` | 2.0 | 2.0 | 2.0 | equal |
| KiB written per acked SET, `always` | 16.4 | 16.4 | 16.4 | equal |
| Flushes, `everysec` against `always` | −95 % | −90 % | −100 % | direction only (timer-driven) |
| Engine clock, `R=no` against `always` | −25 % | −17 % | −46 % | direction only |
| Redis kill: engine clock | +28 % | +77 % | +50 % | direction only |
| Recovery reads after `C=12`: `always` / `everysec` | 104 / 72 | 104 / 72 | 104 / 71 | equal (±1) |
| rkv extra write: writes / KiB | +3 / +12 | +3 / +12 | +3 / +12 | equal |
| rkv +100,000 rounds per PUT | +57 % branches | invisible | invisible | hvx only |
| Stack kill, fixed against old service | +11 % branches | +3 % clock | (not run) | direction only |

Counts of device work that do not depend on time agree exactly across engines. So do counts
that depend only on the sequence of operations (writes, flushes and bytes per operation, reads at
recovery). Engine clocks agree in direction only: each engine charges differently for a given
operation (an exit on kvm is a different unit from one on hvx). Timer-driven behavior (`everysec`,
idle ticks) agrees in direction only, because guest time per unit of work differs by engine.
Only hvx can measure CPU work.

## 5. What it cannot show

- **Real latency or throughput.** Guest time is a function of the engine's clock, not of the
  hardware. A pause adds 6 guest seconds on hvm and 8 on kvm. A slow fsync costs one flush
  whether the disk would take 50 µs or 50 ms.
- **CPU cost on kvm and hvm.** Code without exits takes no guest time there. Only hvx's branch
  counter sees it, and only as a branch count: it says nothing about instructions, cache misses,
  vectorized against scalar code, or memory bandwidth.
- **Caches and the memory hierarchy.** The guest's page cache is counted (page faults, pages
  written back), but no CPU cache, TLB or branch predictor is.
- **Small changes.** A change shifts the timing of the run it is in. In this experiment, changes
  below about 1 % of the branch total could not be told apart from the timing shift they cause.
  The floor is per workload and must be measured per project.
- **The workload of a power-cut run's first boot from inside the guest.** That boot ends at the
  cut, before any probe after a verdict. It is covered only by host-side counters.
- **Per-process CPU time** at a useful resolution: the guest kernels count it in 10 ms ticks and
  keep no `schedstat` or `/proc/<pid>/io`. Enabling `CONFIG_SCHED_INFO` and
  `CONFIG_TASK_IO_ACCOUNTING` in the guest kernels would give nanoseconds per process and system
  calls per process. That changes the kernels, so every fingerprint would move once.

## 6. Verdict and recommendation for CI stage 2

**Worth building, small and opt-in.** For a pull request, CI stage 2 (docs/design/ci.md) could
say: "this change makes each acknowledged order cost 3 more flushes", or "recovery after a power
cut reads 40 % more", or "the order service's workload costs 39 % more branches under a Redis
kill". Each statement would be exact, repeatable and backed by a replayable run. No timing-based
benchmark in CI can say this without noise. It needs no new engine work for the counters that
matter most.

**What to track (the counters).**

1. Host-side, every run, free: the engine clock per phase (workload, checks, recovery boot),
   hvx's branches per phase, and kvm and hvm's disk requests, writes and flushes. These go into
   `result.json` as a `cost` object, an addition to results format 1.
2. Guest-side, after the verdict, for the schedules a project marks as cost schedules: data
   written and flushes per disk, reads at recovery, packets and bytes per link, forks, context
   switches, page faults, and resident memory per service. Report per acknowledged operation
   where the workload reports acknowledgements.
3. Not tracked: guest seconds, per-process CPU ticks, and anything driven by timers. These move
   with the engine and with unrelated timing changes.

**What a report shows.** For each cost schedule, base commit against head, on the same engine
and the same kind of host CPU, only the counters that changed by more than a threshold. The
threshold is a default of 1 % plus that project's measured noise floor. For example:

(An example of the output, not measured:)

```
hydo cost: 2 of 14 counters changed on "disk=none | run" (hvx)
  flushes per acked write    2.00 -> 3.00   (+50 %)
  data KiB per acked write  16.4  -> 24.6   (+50 %)
hydo cost: recovery after "C=24" (hvx): branches 1,696 M -> 2,310 M (+36 %)
```

Each line links the two runs, which hydo can replay. The report must name the engine and say
that CPU changes are invisible on kvm and hvm.

**Rules the report needs.** Compare only runs of the same engine, host CPU kind and fingerprint
epoch. Compare counts, never guest time. Never probe inside the workload: a probe there moves the
timing and can change the verdict (section 2). Treat a change of one counter alone below the
noise floor as "no change".

**Cost of building.**

- Host-side counters into `result.json`, for hvx (read the PHASES counters always), kvm and hvm
  (the device statistics and exits per phase): about two days, including the results-format
  addition and schema test.
- The after-verdict probe as a product feature: a manifest or `hydo/ci.yaml` list of cost
  schedules, parsed `HYDO-COST` lines into `result.json`, and per-operation normalization: about
  two to three days. The probe in this branch already works on all three engines.
- The comparison and the report in `hydo ci run`: about two days. It reuses stage 1's base run
  by replay.
- Optional: per-process nanoseconds and system calls through the two guest kernel options (one
  re-baseline of every fingerprint, a fingerprint-epoch bump): about one day plus the re-baseline.

**Start with the host-side counters.** They cost nothing per run, cannot disturb a run, and
already show durability cost, recovery cost and, on hvx, CPU work. Add the guest probe for the
flush and byte counts per operation, which the host cannot attribute to a disk partition or a
service.

## 7. The prototype and how to repeat the experiment

- `HYDO_COST` (hydo-config `run::Control::cost`, registry entry `HYDO_COST`; read by
  `AgentEngine::with_control` into `Plan::cost`): unset or `0` changes nothing. `1` or any other
  value reports the probe after each conversation's verdict. `full` also reports it at the
  workload's start and end and in a recovery boot before its checks.
- `hydo/crates/hydo-control/src/schedule/cost.rs`: the probe script and `probe()`. The call sites
  are in `schedule.rs` (`finish`, `explore_run`, `main_recovery`).
- Host-side counters used: `HVX_EXPLORE_PHASES=1` (hvx, `PHASES` lines in `engine.log`),
  `HYDO_KVM_DIAG=1` (kvm, `<tag>.diag`), and the stops' `at` in `control.jsonl`.
- A run with the probe, for example on kvm:
  `HYDO_COST=1 hydo run --backend kvm --image <redis image> "disk=none dseed=1 | run"`. Then
  `grep HYDO-COST <run dir>/control.jsonl`.
