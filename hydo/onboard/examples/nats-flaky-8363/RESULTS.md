# A flaky Go unit test under hydo (nats-server issue 8363)

This page records an attempt to reproduce an open flaky test with hydo. The test is
`TestRegisterAndUnregister` in the package `server/ats` of nats-server. The flake is a race
between goroutines inside one test process, so it depends on thread timing only. No service,
disk or network fault is involved. The last section lists what hydo lacked for a test like this one.

## The issue

Issue 8363 ("Flaky TestRegisterAndUnregister due to asynchronous ATS goroutine shutdown") was
open on 2026-11-08, labelled `defect` and `stale`, with no comments. Its report names v2.14.3 and
this failure:

```
ats_test.go:72: Expected same number of go routines after removing all registered: 3 vs 2
```

`Unregister()` sends on the `done` channel and returns without waiting for the ticker goroutine
to exit. The goroutine left by one iteration's deferred `Unregister()` is then counted by the next
iteration's baseline (`runtime.NumGoroutine()` at the top of the test). The failure needs two
iterations in one test process, so a run with `-count=1` never fails.

Two pull requests fix it. Pull request 8375 was closed unmerged on 2026-10-13 (no activity).
Pull request 8794, "Fix ATS worker shutdown synchronization" at head `436addad`, was open. Main at
`59bf235a` (2026-11-06) still has the race.

## The test programs

`build-test.sh` builds two static linux/amd64 test binaries with the module's toolchain,
go1.26.10, and checks them against pinned sha256 values:

| binary | source | sha256 |
|---|---|---|
| `bin/ats-buggy.test` | main at `59bf235aea63881477dd485af3579be2ffc32b0c` | `521e509e…` |
| `bin/ats-fixed.test` | pull request 8794 at `436addad5f9db3983772fe3b85969e1f51d2b99d` | `eea9f033…` |

The command for each is `GOTOOLCHAIN=go1.26.10 CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go test -c
-trimpath` in `server/ats`.

## Native rates

Each row is two runs of `-test.count=100`, so 200 iterations. The first iteration of a process
cannot fail. The command is `<binary> -test.run '^TestRegisterAndUnregister$' -test.count=100`.

On the Mac (Apple M2 Pro, 12 cores, macOS 26.4.1, go1.26.10 darwin/arm64 binaries; the load is
12 busy shell loops):

| GOMAXPROCS | build | load | failed iterations |
|---|---|---|---|
| 1 | plain | none | 100 of 200 |
| 2 | plain | none | 1 of 200 |
| 4 | plain | none | 9 of 200 |
| default (12) | plain | none | 27 of 200 |
| default (12) | `-race` | none | 2 of 200 |
| 1 | plain | 12 loops | 100 of 200 |
| 2 | plain | 12 loops | 11 of 200 |
| default (12) | plain | 12 loops | 26 of 200 |
| default (12) | `-race` | 12 loops | 13 of 200 |

A first check with go1.27.3 failed 5 of 100 iterations with the default GOMAXPROCS.

On Linux (the bare-metal machine, a Xeon E3-1270 v6 with 8 hardware threads, Ubuntu, kernel 6.8,
the static linux/amd64 buggy binary, no load):

| GOMAXPROCS or CPU set | failed iterations |
|---|---|
| 1 | 100 of 200 |
| 2 | 25 of 200 |
| 4 | 5 of 200 |
| default (8) | 96 of 200 |
| `taskset -c 1` (Go then uses one P) | 100 of 200 |
| `taskset -c 1,2` | 4 of 200 |

The most a process of 100 iterations can fail is 50, because a failed iteration leaves no
goroutine behind (next paragraph). So the default on this idle Linux host fails 96 of at most
100, nearly every time it can.

With one P the test is not flaky. It fails in every second iteration: the iteration after a
passing one fails, and a failing iteration stops at line 72 before its second `Register()`, so the
next one passes. `GOMAXPROCS=1` with `-test.count=2` failed 10 of 10 times; the fixed binary
passed 10 of 10 times. One `runtime.Gosched()` before the baseline makes the buggy test pass 5 of
5 times with one P. That confirms the order: with one P, the ticker goroutine waits in the local
run queue behind the next test goroutine, which samples the goroutine count first.

The script of these native runs, with its steps, is in [`native/README.md`](native/README.md).

## The hydo project

`hydo.yaml` has no services. Its workload runs `/bin/ats.test` once, as one process with
`-test.count={{count}}` (default 2), and writes the output to `/data/t/out` and the console. The
invariant `ats.test_passes` fails when the output has a `--- FAIL` line, and its message carries
the order of PASS and FAIL lines and the test's own failure line. The parameters are `variant`
(`buggy` or `fixed`), `count` and `procs` (GOMAXPROCS; empty means Go's default, the guest's CPU
count).

The images were built with the published hydo 0.3.0 Linux release on both machines, with the
same bytes on both:

| image | `--set` | disk image sha256 |
|---|---|---|
| buggy | none | `d1ce39a404688171…` |
| fixed | `variant=fixed` | `db212e314bcaa509…` |
| buggy, GOMAXPROCS=2 | `procs=2` | `e893c60eb4d11a9d…` (build machine only) |
| buggy, GOMAXPROCS=4 | `procs=4` | `a9e25b205e4fc2c5…` (build machine only) |

## On the KVM engine: one vCPU, always fails

The KVM engine runs one vCPU, so Go runs with one P and the test fails on every schedule. These
runs were made on the build machine:

| what | schedules | failed |
|---|---|---|
| `disk=none \| run`, twice from fresh boots | 2 | 2 (`fp=5d9fb671f4fe48f1` both times) |
| `hydo explore --backend kvm --timing --seeds 1-40` | 40 | 40, all reproduced |
| hand schedules with `tjpm=1000 tjmax=100000` and `pctn` up to 1000 | 7 | 7 |
| GOMAXPROCS=2 image: `hydo explore --timing --seeds 1-100` | 100 | 100 |
| GOMAXPROCS=2 image: `pct=<seed> pctn=10\|100\|1000\|5000` | 60 | 60 |
| fixed image, `disk=none \| run` | 1 | 0 (`fp=93839c43124abd39`) |

The timing tokens changed the fingerprint of every run, but none changed the verdict. Even with
two Ps on one vCPU every run failed, while GOMAXPROCS=2 failed 1 of 200 iterations natively on
the Mac and 25 of 200 on Linux. The window between the deferred `Unregister()` and the next
`NumGoroutine()` is a few microseconds of Go runtime code with no VM exit in it. On KVM the clock,
the tick jitter, and the forced preemption points all count VM exits, and so none of them can
land in that window. The second Go thread runs only when the first one blocks, which is after
the count.

## The KVM engine with two virtual CPUs

Since Milestone 219 the KVM engine runs several vCPUs, in turns of a number of VM exits
(`docs/design/kvm-vcpus.md`). A turn can end only at an exit, and the window of the previous section has none. The
question was whether a turn boundary at another exit still lets the other vCPU run in time.
It does. `hv/kvm/smp/nats-check.sh` ran on GitHub's runners on 2026-11-13, on AMD EPYC 9V45 and
two EPYC 7763 (run https://github.com/kakoc/hydo/actions/runs/43496814855), with the SMP guest
kernel `b40b2711…`. Each setting ran three times from fresh boots of `hydo run --backend kvm`, and
the guest saw 2 CPUs (`cpus=2`). Every setting gave one fingerprint in its three runs, and the
same fingerprint on all three hosts:

| image | vCPUs | schedule | verdict on all hosts |
|---|---|---|---|
| buggy | 1 | `disk=none \| run` | VIOLATION (`d6cb9210afe66ff9`) |
| fixed | 2 | `disk=none \| run` | OK (`91fe8b1ca94b85a6`) |
| buggy | 2 | `disk=none \| run` (turns of 256 exits) | VIOLATION (`fb300564e3e41a6b`) |
| buggy | 2 | `q=3`, `8`, `12`, `16`, `24`, `32` | OK |
| buggy | 2 | `q=1`, `2`, `4`, `6`, `48`, `64`, `96`, `128`, `192`, `256`, `384`, `512`, `768`, `1024`, `2048`, `4096` | VIOLATION |
| buggy | 2 | `q=64 qs=4` | OK |
| buggy | 2 | `q=64 qs=1`, `2`, `3`, `5`, `6` | VIOLATION |

So 7 of the 28 turn settings pass and 21 fail. Turns of 8 to 32 exits let the other vCPU run
between `Unregister()` and the count. The verdict is not monotonic in the turn length: `q=3`
passes, while `q=2`, `q=4` and `q=6` fail.

An earlier run (https://github.com/kakoc/hydo/actions/runs/43446326976) counted 10 passing
settings, but its kernel had a determinism bug: a program's RDTSC on vCPU 1 read the host's TSC
(Milestone 219 in `docs/HISTORY.md`). Go's runtime reads the TSC, so those verdicts depended on
the host's time. Against the valid run, `q=2`, `q=4`, `q=6` and `q=64 qs=6` passed there and fail
now, `q=12` failed there and passes now, and the other 23 settings have the same verdict. That run
also gave other fingerprints on each host at `q=12`, `q=1024`, `q=2048` and `q=4096`.

The fixed kernel ran on Intel on 2026-11-14 (run
https://github.com/kakoc/hydo/actions/runs/43527210060, Milestone 227). On six hosts, an Intel
Xeon 6975P-C and five AMD hosts (three EPYC 9V74, a 9V45 and a 7763), every one of the 31
settings gave one fingerprint in three runs. Every host gave the verdicts of the table above.
The five AMD hosts gave the same fingerprints, and the Intel host gave its own, as KVM
fingerprints differ between CPU vendors.

### The explorer on two vCPUs

Since Milestone 227 the explorer draws KVM turn lengths. The same run ran
`hv/kvm/smp/turns-check.sh` on AMD EPYC 9V74 and 7763, with the same results on both hosts:

- `hydo explore --backend kvm --vcpus 2 --turns --seeds 1-40` drew a turn length for 21 of 40
  schedules, each from 1 to 512 exits. 8 schedules passed and 32 failed. The explorer reproduced
  its 32 failures, and `hydo repro` of the exploration gave 33 of 33 with the recorded
  fingerprint (the failures and the minimal schedule `disk=none dseed=975835de1c9756ce | run`).
- The passing schedules had `q=16`, `q=26`, and the seeded turns `q=7`, `q=15`, `q=17`, `q=26`,
  `q=36` and `q=135` with a `qs=`.
- `hydo explore --backend kvm --vcpus 2 --sweep turns` ran 12 lengths from 1 to 4,096 exits and
  narrowed them to one exit, 20 runs in all. It found two boundaries: `q=15` fails and `q=16`
  passes, and `q=39` passes and `q=40` fails. Each of the four lengths gave one fingerprint
  twice, and `hydo repro` of the sweep gave 2 of 2.

The sweep's `q=2`, `q=16` and `q=32` gave the fingerprints of the table's fresh boots. The table
did not run `q=10`, `q=15` and `q=40` to `q=44`.

## On hvx: several vCPUs, a turn-length boundary

On the bare-metal machine, hvx runs the guest on several vCPUs in turns of a fixed number of
guest branches (the host token `q=`, default 2^20). These runs used `HVX_EXPLORE_VCPUS=2` (or 4)
and `HYDO_HVX_HOSTCALL=0` (the section "What hydo lacked" says why). With the default GOMAXPROCS,
Go runs one P per vCPU.

`hydo explore --vcpus 2 --turns --timing --seeds 1-60` gave 56 failures and 4 passes. All 56
failures were reproduced, and the four passes all had turns of at most 9,096 branches. A series of
the turn length alone, one fresh boot per schedule, showed a sharp boundary:

| schedule (2 vCPUs) | verdict |
|---|---|
| `disk=none q=50 \| run` up to `q=18700` (19 values) | OK |
| `disk=none q=18800 \| run` up to `q=1000000` (10 values) | VIOLATION, `[PASS FAIL]` |
| `disk=none \| run` (default turn, 2^20) | VIOLATION, `fp=31fe486bb2cc6ffe` twice |

The boundary pair, each run twice from fresh boots:

| image | schedule (2 vCPUs) | verdict | fingerprint (both runs) |
|---|---|---|---|
| buggy | `disk=none q=18800 \| run` | VIOLATION, line 72, `3 vs 2` | `ccb15d49de7ed781` |
| buggy | `disk=none q=18700 \| run` | OK, `pass=2 of 2` | `20a819cec77ec197` |
| fixed | `disk=none q=18800 \| run` | OK | `1485b2106933bdcf` |
| fixed | `disk=none \| run` | OK | `f1ccfd7ebb0887ef` |

To run the failing schedule on the bare-metal machine:

```sh
~/bin/hvx-run sudo env HOME=$HOME PATH=$PATH HVX_EXPLORE_VCPUS=2 HYDO_HVX_HOSTCALL=0 \
  hydo run --image <buggy image> "disk=none q=18800 | run"
```

On 4 vCPUs the boundary is between `q=18800` (OK, `d894048d46727e40`) and `q=20000`
(VIOLATION, `7a4f1b7c5672a86d`); the default turn fails (`88be851d1257f1d2`). Turns of 1,000 to
18,800 branches pass.

So hydo reproduces the failure every time, and it also gives a passing neighbour on the same
binary: a turn 100 branches shorter. With turns of at most about 18,700 branches, the other vCPU
runs while the test goroutine is still between `Unregister()` and the next baseline. Its P then
takes the ticker goroutine from the queue. With longer turns the test goroutine reaches
`NumGoroutine()` within its own turn, as with one P.

`hydo diff` of the q=18700 and q=18800 runs says that they share every stop up to the workload's
start and that the first difference is the test's own output. It cannot show the goroutines,
because the difference lies inside the Go runtime. The test writes its output at the end, so
every test line has the same stop.

## Schedules run

- KVM, one vCPU: 209 runs of the buggy images (2 + 40 + 7 + 100 + 60, plus the explorer's re-runs
  and shrink runs) and 1 of the fixed image. No buggy run passed. Before the workload got four
  steps, a one-step image gave the same verdicts (VIOLATION twice, `6a65603a11a4cd68`; fixed OK).
- hvx, 2 vCPUs: 60 explorer schedules plus their re-runs, and 39 hand runs: the first default
  run, the three turn-length series (17 + 9 + 4), the boundary pair twice (4) and the fixed image (4). Two
  more runs with the host-call port on gave no result (item 1 below).
- hvx, 4 vCPUs: 11 runs.
- hvx, 1 vCPU: 1 run (VIOLATION, `cdee99d853af62f1`).

## What hydo lacked, ranked

1. **Several vCPUs hang with the default host-call port.** `hydo run` on 2 vCPUs ran for 12
   minutes at 100 % of one CPU and wrote nothing; it was stopped by hand. A second try,
   `disk=none q=18800 | run`, gave no result in 290 seconds. With `HYDO_HVX_HOSTCALL=0` both
   finished in seconds, and one vCPU with the port on finished in seconds too. AGENTS.md open
   item 36 says the port does not carry several vCPUs' records at once; in practice the run
   never ends. Until it is
   fixed, every several-vCPU run needs the variable, and nothing tells the user.
2. **No way to choose the vCPU count on `hydo run`.** The count comes only from the environment
   variable `HVX_EXPLORE_VCPUS`, which `hydo run --help` does not mention. The explorer's report
   prints a "by hand" command without the vCPU count and without `HYDO_HVX_HOSTCALL=0`, so that
   command runs a different, one-vCPU schedule.
3. **No search over turn lengths.** The finding is a boundary in `q=`, and the explorer has no mode that tries a range of turn lengths.
   The explorer draws a turn under 10,000 branches in about 10 % of schedules, so 4 of 60 passed.
   The boundary took 30 hand runs.
4. **Exit-counted time cannot reach exit-free races.** On KVM (and on the Mac) tick jitter and
   preemption points count VM exits. A race inside the Go runtime has no exit in its window, so
   209 one-vCPU schedules gave one verdict. One vCPU is no substitute for several here.
5. **The workload format assumes a system test.** The explorer refused one workload step
   (`manifest_space: workload.steps=1; the generators need at least 4 steps`), so the workload
   has four steps and runs the test in the first. A project without services still needs a
   `faults:` list, and the explorer classed the unit-test failure as `durable-defaults`. There is
   no ready-made workload for `go test` (or another test runner) that turns a failing test into a
   finding.
6. **No look inside a process.** `hydo diff` and `hydo inspect` work on stops, consoles and
   control transcripts. For a race between goroutines the useful evidence is which vCPU ran
   which thread around the turn boundary, which hydo does not record.
7. **Not tried:** the `-race` binary in the guest (it needs cgo, so a dynamic binary and glibc in
   the image), and `hydo explore --vcpus` with the host-call port on.

## Milestone 205: the gaps closed

Milestone 205 fixed items 1, 2, 3, and 5 of the preceding list. The runs below were
made on the bare-metal machine with hydo built from the M205 branch, one VM at a time.

**The host-call port with several vCPUs.** With several vCPUs, the module's in-kernel scheduler
does not end a run call at the agent's record. The engine read the port's records only when a call
ended at one, so the agent's first line never reached the host. Since M205 the engine reads
the records at every return of a run call. It also ends a call every 8,192 exits, so that the
port's 64 KiB buffer cannot fill. The port is on by default, and no variable is needed. On the images of
the section "The hydo project" (the same bytes), each schedule ran twice from fresh boots:

| image | schedule | vCPUs | verdict | fingerprint (both runs) |
|---|---|---|---|---|
| buggy | `disk=none q=18800 \| run` | 2 | VIOLATION | `3687688f9524f9c5` |
| buggy | `disk=none q=18700 \| run` | 2 | OK | `e5d353a436951f60` |
| buggy | `disk=none \| run` | 2 | VIOLATION | `57ba6456c2c6e8ac` |
| fixed | `disk=none q=18800 \| run` | 2 | OK | `dd2e764497d4794d` |
| buggy | `disk=none q=20000 \| run` | 4 | VIOLATION | `a6d4a06a44eed1b3` |
| buggy | `disk=none \| run` | 1 | VIOLATION | `cdee99d853af62f1` |

The boundary is where it was over the serial line. The fingerprints differ from the serial line's,
because the agent's lines travel another way. The one-vCPU run gives the value recorded before
M205. Each run took 7 to 8 seconds.

**The project in the new form.** `hydo.yaml` now says `workload.kind: test` with one step and no
`faults:`. The M205 `hydo build` made new images: buggy `f4c8b0e15a2dd6ae…`, fixed
`056258a6dafd120e…`. Each schedule ran twice with `hydo run --vcpus 2`, and `result.json` records
the two vCPUs:

| image | schedule | verdict | fingerprint (both runs) |
|---|---|---|---|
| buggy | `disk=none q=18800 \| run` | VIOLATION | `3f5ddc03373f0903` |
| buggy | `disk=none q=18700 \| run` | OK | `4c392504eb20a851` |
| fixed | `disk=none q=18800 \| run` | OK | `c19f715ce52e1dbe` |
| buggy, one vCPU | `disk=none \| run` | VIOLATION | `00bce3c8f97dfc2c` |

`hydo repro` of the first run directory said REPRODUCED. `hydo explore --vcpus 2 --turns --timing
--seeds 1-12 --workers 1` gave 9 failures, all in the class `test-failed`, all reproduced. The
report's line by hand was `hydo run --vcpus 2 --image <image> "disk=none dseed=975835de1c9756ce |
run"`.

**The turn sweep.** `hydo explore --vcpus 2 --sweep turns --workers 1` made 19 runs in 60 seconds
and found one boundary: `q=18407` passes (`96e792fe27fe27b0`) and `q=18496` fails
(`adb0fab23867e30f`), both reproduced. Fresh boots with `hydo run` gave the same two
fingerprints. The verdict does not change only once with the turn length. A dense sweep of the
range around it made 36 runs in 30 seconds
(`--turn-range 18000-19000 --turn-points 21 --turn-resolution 10`). It found five boundaries.
The failing bands are 18,486 to 18,543, 18,732 to 18,853, and 18,980 branches on. `hydo repro` of the dense sweep reproduced all five failing lengths.

## The fingerprint epoch 2 (Milestone 206): the current values

The values in the sections above are those of the fingerprint epoch 1. Since Milestone 204 an
x86_64 image built by hydo describes the CPU's caches in CPUID by default (`guest.cpu_cache`), and
that moves the guest's timing, so the turn-length boundary moved too. The images built at epoch 2
(buggy `43b6237e6fdb25d7…`, fixed `a4646977e5c4a2fb…`) ran on the bare-metal machine, each
schedule twice from a fresh boot with `hydo run`:

| image | schedule | vCPUs | verdict | fingerprint (both runs) |
|---|---|---|---|---|
| buggy | `disk=none q=18140 \| run` | 2 | VIOLATION, line 72, `3 vs 2` | `6fca0caef17a99f9` |
| buggy | `disk=none q=18051 \| run` | 2 | OK, `pass=2 of 2` | `9293d2b0af65bb25` |
| fixed | `disk=none q=18140 \| run` | 2 | OK | `5650cc5ce89fb568` |
| buggy | `disk=none \| run` | 1 | VIOLATION | `21944e55f674efb2` |

These four runs are in `hydo/onboard/recorded.json` (`hvx/nats-flaky-buggy`,
`hvx/nats-flaky-fixed`), so `hydo-bench rebaseline` checks them with the other examples. The
default turn sweep (`hydo explore --vcpus 2 --sweep turns`, 19 runs in 64 seconds) found the
boundary between `q=18051` and `q=18140`. The dense sweep of 18,000 to 19,000 (36 runs) found five
other boundaries: the failing bands are 18,171 to 18,387, 18,461 to 18,606, and 18,808 branches
on. It stepped over the band at 18,140: its grid points 18,098 and 18,147 pass. At epoch 2,
`q=18800` and `q=18700` both pass.

## Files

| file | what it is |
|---|---|
| `build-test.sh` | builds and checks `bin/ats-buggy.test` and `bin/ats-fixed.test` |
| `hydo.yaml` | the project: one test process per run, the invariant on its output |
| `upstream-comment-draft.md` | a draft comment for issue 8363, not posted |
