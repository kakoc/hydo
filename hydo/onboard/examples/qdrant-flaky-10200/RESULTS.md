# A flaky Rust test under hydo (Qdrant issue 10200)

This page records an attempt to reproduce an open flaky test with hydo. The result is posted
upstream: https://github.com/qdrant/qdrant/issues/10200#issuecomment-6803314544 (2026-11-11). The test is
`tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue` in Qdrant's crate
`collection`. The flake is a race between the test's own thread and a background worker of the
same process. It depends on thread timing only. The last section lists what hydo lacked for a test
like this one.

## The issue

Issue 10200 ("Flaky test `tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue`") was
open on 2026-11-09, labelled `accepted`, with no comments. Qdrant's CI bot opened it on
2026-08-12 and last updated it on 2026-10-21. The last report is from `macos-latest` at commit
`222b2e44`:

```
Applied seq path: ".../test_collection6GrvAs/applied_seq.json"
Current applied_seq: 449
Reduced applied_seq from 449 to 400, total_ops: 500
Post-load update queue info: ShardUpdateQueueInfo { length: 0, op_num: Some(500), deferred_points: Some(0) }

thread 'tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue' (98281) panicked at lib/collection/src/tests/wal_recovery_test.rs:581:5:
update queue should have pending operations after WAL replay
```

Pull request 11028 was open and not merged. Its title is `[AI] Fix flaky
test_wal_replay_loads_pending_to_queue by not re-reading live queue length`. Its one commit `b5ce700e` records the size of the queued
tail inside `load_from_wal` and checks that number instead of the live queue length. Its author
reports that the old test passed 8 of 8 times on a quiet machine. The last change of the test file on `dev`
was on 2026-10-09, and `dev` on 2026-11-08 still has the race.

## The test programs

`build-test.sh` builds two test programs and checks them against pinned sha256 values. Each one is
`cargo test -p collection --lib --no-run` in the dev profile, the profile Qdrant's CI uses. The
build uses these tools:

- Rust 1.99.0, the stable release on 2026-10-21;
- protoc 34.1, the version that Qdrant's CI pins;
- Ubuntu 24.04's gcc 13.3.0, for the C code of `aws-lc-sys` and `zstd-sys`. The build
paths are remapped and the debug information is stripped (542 MB to 286 MB). Two builds in two
directories of the bare-metal machine gave the same bytes.

| file | source | sha256 |
|---|---|---|
| `bin/collection-buggy.test` | `222b2e44ab1c3f17e8246396ead6558e45041df7`, the commit of the report | `ff20b709…` |
| `bin/collection-fixed.test` | the same commit with pull request 11028's commit applied | `15143bd7…` |

The programs are dynamic (glibc), so `bin/lib/` holds `ld-linux-x86-64.so.2`, `libc.so.6`,
`libm.so.6` and `libgcc_s.so.1` from Ubuntu 24.04's packages `libc6 2.39-0ubuntu8.9` and
`libgcc-s1 14.2.0-4ubuntu2~24.04.1`. The script downloads both packages from
`snapshot.ubuntu.com` and checks them by sha256.

## Native rates

cargo-nextest, which Qdrant's CI uses, runs each test in a process of its own. Every native run
below is one process with
`<program> --exact tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue --test-threads 1 --nocapture`.
The test prints the queue length that it reads, so each run also gives the number of queued
operations that were still in the queue (at most 35).

| where | how | runs | failed | queue lengths seen |
|---|---|---|---|---|
| Mac, M2 Pro, 12 cores, macOS 26.4.1 | one at a time | 224 | 0 | 35 |
| same | 12 at once | 400 | 0 | 35 (36 three times) |
| same | 12 at once beside 18 busy loops | 400 | 0 | 35, down to 26 once |
| same | `TOKIO_WORKER_THREADS` 1, 2, 64, 4 at once | 600 | 0 | 35, down to 34 |
| Linux, Xeon E3-1270 v6, 8 threads, kernel 6.8 | one at a time, idle | 40 | 0 | 35 |
| same | `taskset -c 0` (one CPU) | 240 | 0 | 23 to 35 |
| same | `taskset -c 0`, 4 at once | 200 | 0 | 31 to 36 |
| same | 8 at once | 200 | 0 | 30 to 36 |
| same | `taskset -c 0-2`, 6 busy loops, 3 at once | 300 | 0 | 31 to 36 |

On Linux the test data was on tmpfs (`TMPDIR=/dev/shm`, as in Qdrant's Linux CI). The Mac runs
used the aarch64 build of the same commit (`cargo test -p collection --lib --no-run`, Rust 1.99.0).
No run failed in 2,604 runs. No setting of one knob made the test fail: the worker thread count
(`TOKIO_WORKER_THREADS`), one CPU, and CPU load only lowered the length that the test read.

## Why the test fails

The test writes 500 operations, stops the shard without a flush, lowers the persisted
`applied_seq` from 449 to 400 and loads the shard again. `LocalShard::load_from_wal`
(`lib/collection/src/shards/local_shard/mod.rs`) replays the operations up to
`applied_seq + 65` itself. It sends the other 35 operations (465 to 499) into the update queue, a
bounded tokio channel. The update worker (`lib/collection/src/update_workers/update_worker.rs`)
already runs and takes operations from that channel.

After `LocalShard::load` returns, the test calls `local_update_queue_info()`. That function first
counts the deferred points of every segment in a `spawn_blocking` task and waits for it. Only then
does it read the queue length, which is the channel's occupancy. The test asserts that the length
is more than zero. The test's own comment says that the check fails when the worker applies every
queued operation in that window.

In the failing report the queue length is 0 and `op_num` (the last operation that the worker
applied) is 500. So the worker had applied all 35 queued operations before the test read the
length. In a passing run the two numbers add up to 35: for example `length: 18, op_num:
Some(481)`. This is a race in the test, not a loss of data. Pull request 11028 removes it.

For the race to fail, the test's thread must stay off the CPU while the worker takes 35
operations from the channel. Each operation needs three `spawn_blocking` round trips: reading the
WAL record, applying it, and saving `applied_seq`. On an idle machine with several cores, the
worker did not take a single operation in the window (length 35 in every run). In the reported CI
run the test's thread waited long enough for all 35.

## Under hydo

`hydo.yaml` has no services. Its workload runs `/bin/collection.test` once with the native command of
the preceding section, with the test data on the data disk (`TMPDIR=/data/t/tmp`). The invariant
`qdrant.test_passes` fails when the test output has no `test result: ok. 1 passed` line. Its
message carries the test's own failure line and the queue line. The parameters are `variant`
(`buggy` or `fixed`) and `workers` (`TOKIO_WORKER_THREADS`, empty for tokio's default). The
manifest's header comment names the hvx turn sweep, which was the first plan. The manifest's text
is inside the image, so the comment stays as the recorded images were built; the commands that
reproduce the failure are on this page.

The images were built with this tree's `hydo` over the published 0.3.0 Linux release, with the
same bytes on the bare-metal machine and on the build machine:

| image | `--set` | disk image sha256 |
|---|---|---|
| buggy | none | `9aa2213aea1a5295…` |
| fixed | `variant=fixed` | `4fbac285d9940f02…` |

### On the KVM engine: one vCPU, forced preemption points

The KVM engine runs one vCPU. Its clock counts the VM exits that reach the engine. Without timing
tokens the worker takes 17 of the 35 operations before the test reads the length
(`length: 18, op_num: Some(481)`), so the window is real on one CPU. The test passes.

`hydo explore --backend kvm --timing --preempt --seeds 1-400 --workers 1` adds tick jitter
(`tj=`) and forced preemption points (`pct=`, `pctn=`: that many seeded VM exits among the first
`pctk` exits of the workload, where the guest is made to switch threads). It ran 400 schedules in
996 seconds and found 4 failures (seeds 37, 226, 384 and 393). All 4 reproduced with the
recorded fingerprint, and each failure printed the report's line exactly:
`length: 0, op_num: Some(500)`. The other 396 runs read lengths from 2 to 36; 116 of them read 18.

The explorer shrank seed 37's failure to its first schedule. Varying `pctn=` by hand gave a
passing neighbour. Each schedule below ran twice from a fresh boot with `hydo run --backend kvm`
on the build machine:

| image | schedule | verdict | fingerprint (both runs) |
|---|---|---|---|
| buggy | `disk=none dseed=c7ab56057c8da907 pct=ec699251106e8e0c pctn=40 pctk=3000 \| run` | VIOLATION, `length: 0, op_num: Some(500)` | `6b2a4115d537b1c3` |
| buggy | `disk=none dseed=c7ab56057c8da907 pct=ec699251106e8e0c pctn=39 pctk=3000 \| run` | OK, `length: 22, op_num: Some(477)` | `7bd5b072bda6734c` |
| buggy | `disk=none \| run` | OK, `length: 18, op_num: Some(481)` | `8635373e77d7b828` |
| fixed | the `pctn=40` schedule | OK | `3901d8a88cc8cec7` |

`pctn=41` fails too (`6b2a4115d537b1c3`, the fingerprint of `pctn=40`), and `pctn=20` passes
(`length: 24`). `hydo repro` of the failing run directory said REPRODUCED. The fixed program passes
on the failing schedule. Its test drains the queue on purpose before it reads the length, so it
reads `length: 0, op_num: Some(500)` too, and it then checks the size that `load_from_wal`
recorded.

To run the failing schedule:

```sh
hydo build hydo.yaml
hydo run --backend kvm "disk=none dseed=c7ab56057c8da907 pct=ec699251106e8e0c pctn=40 pctk=3000 | run"
```

A first set of programs, built before the target directory's path was remapped, gave the same
result: 1 failure in 100 explored schedules and 1 in the next 300. The same seed 37 schedule
failed and `pctn=39` passed, with other fingerprints.

### On hvx: one to four vCPUs

On the bare-metal machine hvx did not reach the reported failure. The two hand runs used the
current images; the three explorations used the images of the first programs.

| what | schedules | result |
|---|---|---|
| `disk=none \| run`, 1 vCPU | 2 | OK, `length: 35, op_num: Some(400)`, `240f9805a28e2544` both times |
| `disk=none \| run`, 2 vCPUs | 2 | OK, `length: 35`, `b0b35ed931e310d5` both times |
| `hydo explore --vcpus 2 --sweep turns` | 12 turn lengths from 64 to 2,097,152 | all OK, length 35 (34 once) |
| `hydo explore --vcpus 2 --turns --timing --seeds 1-60` | 60 | all OK, length 34 to 36 |
| `hydo explore --timing --preempt --seeds 1-100`, 1 vCPU | 100 | 85 OK, 15 ERROR (gap 3 below) |

On hvx the worker took no operation in the window, as on the idle native machines. Why the KVM
guest switches threads in the window and the hvx guest does not was not investigated.

On 3 and 4 vCPUs hvx found another failure of the same test, each twice from a fresh boot:

| image | vCPUs | schedule | verdict | fingerprint (both runs) |
|---|---|---|---|---|
| buggy | 4 | `disk=none \| run` | VIOLATION, `Timeout waiting for update queue to empty` | `64a9294783a8edba` |
| buggy | 3 | `disk=none \| run` | VIOLATION, the same line (one run) | `5bca921f29aad958` |
| buggy | 2 | `disk=none \| run` | OK | `b0b35ed931e310d5` |
| fixed | 4 | `disk=none \| run` | OK | `0a79d1f07cf61ca3` |

The test waits at most 2 seconds of guest time for the worker to drain the queue. Under hvx's
turns (the vCPUs run one at a time, `q=` guest branches each), a thread woken on another vCPU waits
for that vCPU's turn. The 35 operations then took more than 2 seconds of guest time. Turn lengths
of 100,000 and 300,000 branches failed the same way. The same assertion fails upstream in the
sibling test `test_wal_replay_tolerates_corrupt_tail_entry` (issue 10103, on Windows). Whether this
is the same slowness or an artifact of the turn scheduler was not checked. The fixed program
passes, because its test waits for the worker with `plunge_async` before the 2-second loop.

## Crash and disk faults

The test simulates the crash itself: it stops the shard without a flush and loads it again inside
one process, from files on the disk. A power cut (`C=`, `cf=`) or a kill of the test process ends
the test before its assertion, so the verdict says nothing about the race. Disk faults
(`disk=lying`, `disk=ioerr`) change what `load_from_wal` reads, which is a different test.
Neither was used.

## Looking inside the failing run

`hydo diff` of the `pctn=39` and `pctn=40` runs says that they share every stop up to the
workload's start (stop 172). The guest prints the test's output when the test ends, at stop 1,670
or 1,675. The first difference that `hydo diff` names is the random name of the test's temporary
directory (`test_collection52I7sm` against `test_collectionT6fAXI`). The queue line comes later,
with the verdict.

`hydo inspect` cannot show the window. Under the timing tokens `pct=` every stop is a live branch
(no snapshot). To read a file or a process at a stop, the guest agent runs a command, and that
command takes the guest's one vCPU from the test. The failing run printed `length: 0` and
ended at stop 1,712. `hydo inspect read` of `/data/t/out` at stops 1,585, 1,590, 1,595, 1,600,
1,610 and 1,620 showed `length: 10`, `20`, `11`, `10`, `1` and `0`. A session started with
`--freeze` at stop 1,582 showed `length: 30`. At stop 1,580 the line was not printed yet. The freeze stops the processes with the agent's own
code, which also runs on that vCPU.

### Where the 40th point fired (Milestone 208)

With `HYDO_PREEMPT_RECORD=1` the KVM engine writes `one.preempt.jsonl` into the run directory,
one record per forced preemption point. The engine only reads registers. On the build machine
both schedules gave their recorded fingerprints with the records on, twice each with the same
records: `pctn=40` VIOLATION `6b2a4115d537b1c3`, `pctn=39` OK `7bd5b072bda6734c`. A run of
`pctn=40` without the records gave `6b2a4115d537b1c3` too.

The seeded draws of the first 39 points do not depend on `pctn`, so the `pctn=40` schedule has
the 39 points of `pctn=39` and one more. `hydo diff` of the two runs names that point:

```text
The first forced preemption point where the runs' records differ:
  Only in B:
    + run   stop 219  preempt  point 38 at exit 13,863 moved the clock from 3,093,347,500 to
            3,097,120,000 ns (the timer's deadline): kernel code at pc 0xffffffff81232bc0,
            thread pointer 0x7f375a2737f0 (page table 0x2d03000, sp 0xffffffff8180bf00)
  What differs: the runs' records share their first 38 points
  What differs: B has a point at exit 13863 that A does not have
  What differs: A's next point: point 38 at exit 13,882 moved the clock from 3,093,395,000 to
                3,097,120,000 ns (the timer's deadline): kernel code at pc 0xffffffff81232bc0 ...
```

The point fired 2,919 exits after the workload started, at stop 219, while the guest kernel
wrote to the serial port (`io_serial_out` in the guest kernel's symbols) on its interrupt stack
(`irq_stack_backing_store`). It moved the guest's clock to the timer's deadline 19 exits earlier
than the `pctn=39` run does at its next point. From there on the two runs differ in timing. The
test's output differs about 1,450 stops later, and the queue length that the test reads is 0
instead of 22. All 40 points fall in the first 3,000 exits of the workload (`pctk=3000`), while
the test process starts. At both runs' points there the kernel runs on its interrupt stack, so
the extra point changes when the timer interrupts come, not which thread is stopped.

## Schedules run

- Native: 2,604 runs on the Mac and the bare-metal machine; none failed.
- KVM, the build machine: 400 explored schedules and their re-runs and shrink runs on the current
  images, and 22 hand runs. On the first programs: 400 explored schedules and 15 hand runs.
- hvx, the bare-metal machine: on the current images 9 hand runs (1 to 4 vCPUs); on the first
  programs 172 explored schedules and 6 hand runs.
- Milestone 208, KVM on the build machine: the 8 runs of `hydo-bench rebaseline --check
  --thorough`, and 5 hand runs with and without the preemption records.

## What hydo lacked, ranked

1. **No passive look at a stop.** `hydo inspect exec`, `read` and `--freeze` run code in the guest,
   which competes with the racing threads on the same vCPU. Under `pct=` the values read at a stop
   were not the values of the recorded run. A race inside one process needs a look from the host.
   The host would read the guest's processes, threads and files from guest memory and disk at a
   stop, and run no guest code. Without it, the evidence is only what the test prints.
2. **No way to see where a preemption point landed.** The verdict changes between `pctn=39` and
   `pctn=40`, but hydo does not say at which VM exit each point fired, or which thread it stopped.
   The shrinker tried `pctn=0` and `pctn=20` and kept `pctn=40`. A list of the points that fired,
   with the guest thread and instruction at each, would point at the window directly.
   Added in Milestone 208, as described in the section "Where the 40th point fired" below.
3. **`--timing --preempt` on hvx draws schedules that the module refuses.** Each schedule with both
   `tj=` (tick jitter, `tjmax=500000`) and `pp=` (a shorter tick period) ends
   `ERROR harness tick jitter (needs the PMU clock)`, although the PMU clock is on: the module
   answers `HVX_SET_TJITTER` with `EINVAL` (`worker-0.err`). That was 15 of 100 schedules.
   The report counts them as 15 failures of the class `test-failed`, all reproduced.
   Fixed in Milestone 208. The explorer keeps the drawn jitter below half of `pp=`'s period. The
   engine refuses such a combination before any VM, and the explorer counts a refused run as
   `refused`, never as a failure. The same exploration on the current buggy image then gave 100
   passes; its 15 schedules with both tokens carry `tjmax=32768 pp=65536` or
   `tjmax=8192 pp=16384`.
4. **hvx's several-vCPU mode does not reach this race.** Turns of 64 to 2,097,152 branches never
   let the worker take an operation in the window. The KVM engine's one vCPU did. There the guest
   switches threads at the exit-counted timer and at the forced points. Some races need one thread
   held off the CPU for milliseconds, as on a loaded CI runner. hvx has no direct knob for that,
   such as a token that holds the running thread off the CPU for N branches at a point. Milestone
   216's pause of the workload, `P=workload@<at>~<off>:<dur>`, stops the whole test process, the
   worker too. So it does not reach this race. `hydo explore --pause-workload --pause-max 3000
   --seeds 1-60` on hvx gave 60 passes, each with a queue length of 34 to 36.
5. **One find in 100 schedules.** The KVM exploration found the failure in 4 of 400 schedules. The
   distribution of the test's queue length (2 to 36, mostly 18) showed how close each run came,
   but the explorer cannot steer by such a number. A value printed by the test could serve as a
   distance for a guided search.
   Milestone 209 built that search: `hydo explore --steer-match 'length: (\d+)'` keeps the
   schedules with the smallest lengths and changes them a little. It did not help here. On KVM,
   over five seed ranges, the first failure came after a median of 118 schedules, against 72
   for the same seeds without the changes. On hvx no run read less than 34. The changed
   schedules read small lengths twice as often, but did not fail more often
   (`docs/design/steered-search.md`).

## Files

| file | what it is |
|---|---|
| `build-test.sh` | builds and checks the two test programs and the C library files they load |
| `hydo.yaml` | the project: one test process per run, the invariant on its output |
| `upstream-comment-draft.md` | a draft comment for issue 10200, not posted |
