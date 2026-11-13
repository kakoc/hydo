# A flaky Go test under hydo (etcd issue 22796)

This page records an attempt to reproduce an open flaky test with hydo. The test is
`TestCacheCompactionResync` in etcd's module `go.etcd.io/etcd/cache/v3`. The flake is a race between
the test's goroutine and the cache's retry timer. It fails when the whole test process gets no CPU
for about 40 ms at one moment. hydo's timing tokens alone never made it fail. With a CPU limit
inside the guest it fails on every run of one schedule, and its neighbour passes. The same CPU limit
also makes it fail outside hydo. Milestone 216's pause of the workload needs no model in the guest.
It stops the test process for 45 ms of guest time and fails the test on every run on the Mac engine
and on hvx. The last section lists what hydo lacked.

## The issue

Issue 22796, titled `[Flaky Test] cache: TestCacheCompactionResync`, was opened on 2026-11-11 with
the labels `area/testing` and `type/flake`. A contributor assigned it to themselves the same
day. Pull request 22815 put a `t.Skip` on the test, and it was closed without a merge on
2026-11-12. No pull request fixes the test. The failing CI job is `pull-etcd-unit-test-386`, at
the head of pull request 22788:

```text
=== FAIL: go.etcd.io/etcd/cache/v3 TestCacheCompactionResync (0.05s)
    cache_test.go:451: Phase 1: initial getWatch bootstrap
    cache_test.go:462: Phase 2: simulate compaction
getWatch failed, will retry after 50ms: etcdserver: mvcc: required revision has been compacted
    cache_test.go:474: expected cached revision 5 after compaction, got 20
```

The job runs `GOOS=linux GOARCH=386 CPU=1 GO_TEST_FLAGS='-p=4' make test-unit`, so every test
program runs with `-cpu=1` (`GOMAXPROCS=1`), four packages at a time. Its pod has a CPU limit of
4 CPUs (`kubernetes/test-infra`, `config/jobs/etcd/etcd-presubmits.yaml`). The test changed last
in January 2026 (commit `f7029fdb7`, `cache: deflake TestCacheCompactionResync`, for issue 21150).

## The test programs

`build-test.sh` builds four static test programs (`go test -c -trimpath`, `CGO_ENABLED=0`) with
the module's toolchain go1.27.4, and checks them against pinned sha256 values. Two builds, one
by hand and one by the script, gave the same bytes.

| file | source | sha256 |
|---|---|---|
| `bin/cache-buggy-amd64.test` | etcd `main` at `1b969e26a2c65733f1ff696e3537373d56b3ae67` (2026-11-12) | `09ae0a67…` |
| `bin/cache-buggy-arm64.test` | the same | `7cc6a546…` |
| `bin/cache-fixed-amd64.test` | the same commit with `fix-test.patch` applied | `be92ee62…` |
| `bin/cache-fixed-arm64.test` | the same | `1202ab71…` |

`fix-test.patch` is this example's own change, not an upstream one. It changes only
`cache/cache_test.go`. The test's KV stub holds every `Get` after the first one until the test
reaches Phase 3, so the cache's retry cannot load the second snapshot early. The whole package
passes with it (`go test -count=3 -cpu=1 .`).

## Why the test fails

The test loads a first snapshot at revision 5 and then sends a compaction error to the cache's
watch. The cache's `getWatchLoop` (`cache/cache.go`) waits 50 ms (`InitialBackoff`) and calls
`getWatch` again. That call's `c.get` restores the store from the stub's second snapshot, at
revision 20, before it creates the new watch. Meanwhile the test polls `Ready()` every 10 ms
and then calls `cache.Get`, which must return revision 5. Nothing orders the test's `Get`
before the retry.

A probe proved this. A `time.Sleep(60 * time.Millisecond)` before the test's `Get` made the
test fail on 3 of 3 runs with the report's line, `expected cached revision 5 after compaction,
got 20`. With `fix-test.patch` and the same sleep, it passed 3 of 3 times.

For the failure, the test's goroutine must not run for about 40 ms between the end of its 10 ms
poll and the retry. With `GOMAXPROCS=1`, other goroutines of the program do not hold it off
that long. The whole process must stop. A container's CPU limit does that: when the container
has used its CPU time for the 100 ms period, every one of its threads waits for the next
period. In the CI job, four test programs and the compiler share the limit of 4 CPUs.

`getWatchLoop` takes its backoff from `defaultConfig()`, not from the cache's own `c.cfg`. So
the option `WithInitialBackoff` has no effect on this loop, and a test cannot lengthen the
backoff with it. `MaxBackoff` is not read anywhere.

## Native rates

Every native run is one process with
`<program> -test.run '^TestCacheCompactionResync$' -test.count=<n> -test.cpu=1 -test.v`.

| where | how | test runs | failed |
|---|---|---|---|
| Mac, M2 Pro, 12 cores, macOS 26.4.1 (darwin/arm64 program) | 100 processes of 10, one at a time | 1,000 | 0 |
| same | 120 processes of 10, 12 at once | 1,200 | 0 |
| same | the same beside 18 busy loops | 1,200 | 0 |
| Linux, Xeon E3-1270 v6, 8 threads, kernel 6.8 | 100 processes of 10, one at a time | 1,000 | 0 |
| same | `taskset -c 0` (one CPU) | 1,000 | 0 |
| same | `taskset -c 0`, 4 at once | 1,000 | 0 |
| same | 8 at once beside 8 busy loops | 1,600 | 0 |
| same | `taskset -c 0`, 2 at once beside 6 busy loops on that CPU | 1,000 | 0 |
| same | CPU limit 20% (`systemd-run --user --scope -p CPUQuota=20%`), one CPU | 100 | 0 |
| same | CPU limit 20%, one busy loop in the same scope, one CPU | 100 | 90 |
| same | CPU limit 10%, two busy loops in the same scope, one CPU | 100 | 100 |

Without a CPU limit, no run failed in 8,000 runs. CPU load and one CPU did not hold the whole
process off long enough. With a CPU limit that a busy neighbour uses up, the test failed in 190
of 200 runs, every time with the report's line. The CPU limit is the condition, and it needs
no hydo.

## Under hydo

`hydo.yaml` has no services. Its workload runs `/bin/cache.test` once with the native command,
with the test's working directory on the data disk. The invariant `etcd.test_passes` fails when
the output has a `--- FAIL` line, and its message carries the test's own failure line. The
parameters are:

- `variant`: `buggy` or `fixed`.
- `arch`: `amd64` or `arm64`, the same as the image.
- `count` and `cpu`: `-test.count` and `-test.cpu` (default 1, as in the CI job).
- `limit`: the cgroup v2 `cpu.max` of a cgroup for the workload, `"<quota> <period>"` in
  microseconds. Empty means no limit.
- `busy`: the number of busy shell loops in the same cgroup.

The images were built with the published hydo 0.3.1 (the Linux release on the bare-metal
machine, the Mac release on this Mac).

| image | `--set` | disk image sha256 |
|---|---|---|
| plain | none | `8bbbfeeb482df1b9…` |
| plain fixed | `variant=fixed` | `9eea094c0d01475f…` |
| limited | `"limit=10000 100000" busy=2` | `f9eeee59455346fc…` |
| limited fixed | the same and `variant=fixed` | `3e01fac8ebabac61…` |
| arm64 | `arch=arm64` (`--arch arm64`) | initramfs `b0664c70c7e99735…` |
| arm64 fixed | `arch=arm64 variant=fixed` | initramfs `c7053aab15b1f584…` |

### Without a CPU limit: no failure

| engine | what | schedules | failed |
|---|---|---|---|
| hvx, 1 vCPU | `hydo explore --timing --preempt --seeds 1-200` | 200 | 0 |
| hvx, 2 vCPUs | `hydo explore --vcpus 2 --sweep turns` (64 to 2,097,152 branches) | 12 | 0 |
| hvx, 2 vCPUs | `hydo explore --vcpus 2 --turns --timing --seeds 1-100` | 100 | 0 |
| Mac, 1 vCPU | `hydo explore --timing --preempt --seeds 1-300` | 300 | 0 |
| Mac, 1 vCPU | `pct=1`, `pctn=` 100 to 10,000 among the first 3,000 or 10,000 exits (the engine fires at most 64 points) | 5 | 0 |

The plain image gives `5105ecd86ce54319` (1 vCPU) and `bbb387175ae301cf` (2 vCPUs) on hvx, and
`985f97632370e53b` on the Mac, each twice. On 3 and 4 vCPUs it passes too (`c350e50d94e6e5e2`,
`52e9aa8fc6786009`). The fixed programs pass `disk=none | run` (`4c4eac3a06a2181a` on hvx,
`0e32a33ecd1d0a7b` on the Mac). A first round of explorations on images of an earlier
manifest text agreed: 200 schedules on the Mac, 100 with `--timing --preempt` and 112 with turns
on hvx, all passing.

hydo's timing tokens interleave threads at other points, but none of them stops the whole
process for 40 ms of guest time. On the Mac and on KVM the guest's clock counts VM exits. A
forced preemption point (`pct=`) moves the clock only to the next armed timer, and the test's
own 10 ms timer comes first. Tick jitter (`tj=`) delays a tick by less than one tick period.

### With a CPU limit: one schedule fails on every run

The limited image runs the test and two busy loops in a cgroup with `cpu.max` `10000 100000`:
10 ms of CPU time in every 100 ms. The busy loops stand for the other test programs and the
compiler that share the CI pod's limit. On hvx the guest's clock counts guest branches, so the
busy loops use up the quota as on a real CPU. The workload prints the cgroup's `cpu.stat`.

`hydo explore --backend hvx --timing --preempt --seeds 1-100` on the limited image found
38 failures, all with the report's line, all reproduced. Each failing schedule has one of the
two hvx preemption tokens:

- `pp=65536` or `pp=16384`, a shorter tick period: 26 of the 32 schedules with it failed.
- `xt=`, inserted ticks: 12 of the 17 schedules with it failed.
- Neither: none of the 51 schedules failed.

On hvx, `pp=<n>` makes the guest's timer tick every `n` guest branches during the workload, and
each tick still advances the guest's clock by 2 ms. Guest time then passes faster per
instruction, as on a slower CPU. `xt=` adds seeded extra ticks between the regular ones. The explorer shrank
seed 2's failure to its first schedule, `disk=none dseed=975835de1c9756ce pp=65536 | run`. The
disk seed `dseed=` changes the fingerprint but not the verdict, so the schedules below leave it
out. Each ran at least twice with `hydo run --backend hvx` on the bare-metal machine:

| image | schedule | verdict | fingerprint (every run) |
|---|---|---|---|
| limited | `disk=none pp=65536 \| run` | VIOLATION, `expected cached revision 5 after compaction, got 20` | `a69b353e5e4c2528` (3 runs) |
| limited | `disk=none \| run` | OK | `c5ff83b44504bb74` |
| limited fixed | `disk=none pp=65536 \| run` | OK | `0f32443537bd2c09` |
| plain | `disk=none dseed=975835de1c9756ce pp=65536 \| run` | OK | `7717a076412fed31` |
| limited | `disk=none dseed=975835de1c9756ce pp=65536 \| run` | VIOLATION, the same line | `8a3ccfaf9ff169ab` |
| limited | `disk=none dseed=975835de1c9756ce \| run` | OK | `5258f7bea352e847` |
| limited fixed | `disk=none dseed=975835de1c9756ce pp=65536 \| run` | OK | `944734d95abe456a` |

The test prints the cgroup's statistics. With `dseed=975835de1c9756ce`, the failing run had 36
throttled periods of 100 ms, the passing run 28, and the fixed run 37. The fixed program also
passed all 100 schedules of the same exploration on the limited fixed image.

The tick period has a boundary. Single runs at `pp=16384`, `32768`, `81920`, `98304`, `114688`,
`122880` and `126976` failed. A bisection between `114688` and `131072` ended at this pair, each
run twice:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none pp=129024 \| run` | VIOLATION | `0fe47c2cf2ee41f7` |
| `disk=none pp=130048 \| run` | OK | `0c928e2eca742e38` |
| `disk=none pp=131072 \| run` | OK | `7c5fa84f48d77cd5` |

To run the failing schedule:

```sh
sh build-test.sh
hydo build hydo.yaml --set "limit=10000 100000" --set busy=2
hydo run --backend hvx "disk=none pp=65536 | run"
```

### With a pause of the workload: no CPU-limit model

A pause of the workload is the timed pause of its own process tree, `P=workload@<at>~<off>:<dur>`
(Milestone 216, `hydo guide faults`, section "Pause the workload"). Before step `at`, hydo starts a
timer in the guest. After `off` ms of guest time it stops every process of the test with SIGSTOP,
and `dur` ms later it continues them. The CI pod's CPU limit does the same to the test program. The
pause runs on the plain images, without `limit` and `busy`. `hydo.yaml` is unchanged, so the images
keep their bytes, and its header comment does not name the pause.

The runs used this tree's `hydo` and the plain images of the preceding table, rebuilt to the same
bytes. Runs at many starts and lengths found one failing region on each engine:

- On the Mac, a 60 ms pause failed for starts of 0 to 7 ms and passed from 8 to 90 ms. At a
  start of 4 ms, lengths of 35 ms or less passed and 40 ms or more failed.
- On hvx, a 60 ms pause passed for starts of 0 to 149 ms and failed for 151 to 163 ms, except
  at 159 ms. It passed from 165 to 300 ms. At a start of 155 ms, a length of 30 ms passed, and
  35 ms failed.

The window is at another moment on each engine: hvx's clock counts guest branches, the Mac's
counts VM exits. Each schedule below ran two or three times with one fingerprint:

| engine | image | schedule | verdict | fingerprint |
|---|---|---|---|---|
| Mac | arm64 | `disk=none \| run P=workload@0~4:45` | VIOLATION, `expected cached revision 5 after compaction, got 20` | `24dedf9da7af0a08` (3 runs) |
| Mac | arm64 | `disk=none \| run P=workload@0~4:35` | OK | `b8b9b46b4027b5af` |
| Mac | arm64 | `disk=none \| run P=workload@0~8:45` | OK | `9402add9274c87ce` |
| Mac | arm64 fixed | `disk=none \| run P=workload@0~4:45` | OK | `8171ca80d265840d` |
| hvx | plain | `disk=none \| run P=workload@0~155:45` | VIOLATION, the same line | `2ae2f4390bb012b8` (3 runs) |
| hvx | plain | `disk=none \| run P=workload@0~155:30` | OK | `3244703db66dae96` |
| hvx | plain | `disk=none \| run P=workload@0~170:45` | OK | `42f3053533e11faa` |
| hvx | plain fixed | `disk=none \| run P=workload@0~155:45` | OK | `7aa29c3293e32a26` |

The runs without a pause gave the values of the earlier sections: `985f97632370e53b` and
`0e32a33ecd1d0a7b` on the Mac, `5105ecd86ce54319` and `4c4eac3a06a2181a` on hvx.

`hydo explore --pause-workload` draws such pauses:

| engine | exploration | failures | the shrinker's schedule |
|---|---|---|---|
| Mac | `--pause-workload --seeds 1-100 --workers 1` | 13 of 100, all reproduced | `disk=none dseed=1d0b14e4db018fed \| run P=workload@0~0:50` (`a980b7222dcce5ee`) |
| hvx | `--pause-workload --pause-max 200 --seeds 1-200 --workers 2` | 3 of 200, all reproduced | `disk=none dseed=2c0e0fedbe2218a8 \| run P=workload@0~154:62` (`fcc2301b270e5edf`) |

Every failure printed the report's line. The explorations of the earlier sections, without
`--pause-workload`, found none in 300 schedules on the Mac and 200 on hvx.

To run the failing schedule:

```sh
sh build-test.sh
hydo build hydo.yaml --arch arm64 --set arch=arm64      # the Mac
hydo run "disk=none | run P=workload@0~4:45"
hydo build hydo.yaml                                    # Linux, hvx
hydo run --backend hvx "disk=none | run P=workload@0~155:45"
```

## Crash and disk faults

The test uses a stub for etcd's KV and watch API, so the cache reads nothing from the disk and
talks to no server. Kills, power cuts and disk faults end the test before its assertion, or they
change nothing that it reads. Neither was used.

## What hydo lacked, ranked

1. **No token that stops a process or a thread for some guest time.** The failure needs the test
   process held off the CPU for about 40 ms at one moment, as a container's CPU limit does. `pct=`,
   `tj=`, `pp=`, `xt=` and vCPU turns change which thread runs next, not how long a runnable thread
   waits. Added in Milestone 216: the pause `P=workload@<at>~<off>:<dur>` and `hydo explore
   --pause-workload` (the section "With a pause of the workload: no CPU-limit model"). It stops the
   whole process, not one thread, so item 4 of `../qdrant-flaky-10200/RESULTS.md` stays open.
2. **The CPU-limit model needs hvx.** On the Mac and on KVM a busy loop makes no VM exits, so it
   uses no guest time and cannot use up a CPU limit. This example's limit runs only on hvx. The
   pause needs no model and works on the Mac and on hvx.
3. **hydo's value here is small.** The same CPU limit with a busy neighbour fails 90 to 100% of
   runs natively. hydo adds a schedule that fails every time and a neighbour that passes, but the
   cause was found with a native probe and a native rate table.

## Files

| file | what it is |
|---|---|
| `build-test.sh` | builds and checks the four test programs |
| `fix-test.patch` | this example's test-only fix (not an upstream change) |
| `hydo.yaml` | the project: one test process per run, an optional CPU limit, the invariant |
