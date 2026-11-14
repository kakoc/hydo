# Flaky tests: run a test program under hydo

A flaky test passes on some runs and fails on others with the same code. Often the cause is a
race between threads of one test process. hydo runs the test program in a guest whose execution
is the same on every run, so a failing run fails again every time. You can then change one
setting, such as the vCPU count or the turn length, and see the verdict change. This page shows
how to set up such a project, which settings reach a race, and how to find the turn length where
the verdict changes.
The worked example is `hydo/onboard/examples/nats-flaky-8363` in hydo's repository (a Go test of
nats-server).

## The project: `workload.kind: test`

A test project has no services. Its workload runs the test program once, and an invariant turns
the test's own output into a verdict:

```yaml
name: my-flaky-test
root:
  files:
    - {src: bin/pkg.test, dst: /bin/pkg.test, mode: "0755"}
data:
  dirs: [t]
workload:
  kind: test
  steps: 1
  run: |
    [ "$HYDO_FROM" = 0 ] || exit 0
    cd /data/t
    /bin/pkg.test -test.run '^TestName$' -test.count=2 -test.v > out 2>&1
    echo 0 >> $HYDO_ACKED
invariants:
  - id: test.passes
    cmd: |
      grep -q -- '^--- FAIL' /data/t/out && { grep -m1 _test.go /data/t/out; exit 1; }
      exit 0
```

With `workload.kind: test`:

- `workload.steps` may be 1. Faults that the explorer draws for later steps are moved into the
  workload's own steps.
- `faults:` is optional. Without it, the explorer draws no fault kinds. It varies only what its
  options add: tick jitter (`--timing`), preemption points (`--preempt`) and vCPU turn lengths
  (`--turns`, `--sweep turns`).
- A failure that no fault explains is in the class `test-failed`. A system workload's failure is
  in the class `durable-defaults`.

## Build the test program

Build one static program that runs the test, with the toolchain the project pins. For Go:

```sh
CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go test -c -trimpath -o bin/pkg.test ./pkg
```

- Run the test at least twice in one process (`-test.count=2`) when its failure depends on what
  an earlier iteration leaves behind, such as a goroutine that has not exited yet.
- Leave `GOMAXPROCS` unset. Go then uses one processor per vCPU of the guest.
- A `-race` program needs cgo, so it needs a dynamic program and the C library in the image.

## Threads on one processor or on several

A race between threads needs the threads to run at the same time, or to be switched at the right
instruction.

- On one vCPU (the Mac engine, or hvx or KVM without `--vcpus`), a second thread runs
  only when the first one blocks or a timer interrupt arrives. KVM and the Mac count guest time in
  VM exits, so a window of a few microseconds with no exit in it is never interrupted. On KVM the
  nats-server test failed on every one of 209 schedules, whatever the timing tokens.
- On hvx with several vCPUs (`--vcpus N`, up to 8), the vCPUs take turns of exactly `q=` guest
  branches (default 2^20). The other vCPU's threads run between the turns, so the turn length
  decides where the threads interleave. The nats-server test fails at `q=18140` and passes at
  `q=18051` on two vCPUs, each twice with one fingerprint. These are the values of the
  fingerprint epoch 2; another epoch, image or engine can move the boundary.

```sh
hydo run --vcpus 2 "disk=none q=18140 | run"
hydo run --vcpus 2 "disk=none q=18051 | run"
```

- On the KVM engine with several vCPUs (`--backend kvm --vcpus N`, up to 8), the vCPUs take turns
  of `q=` VM exits (default 256), so a turn can end only at an exit. That still reaches races
  whose threads meet at a wake-up or a clock read. The nats-server test fails at the default turns
  and at `q=12`, and passes at `q=2` to `q=8` on two vCPUs, the same on two AMD hosts. The values
  are in the example's `RESULTS.md`; `hydo explore --sweep turns` does not search KVM turns yet.

```sh
hydo run --backend kvm --vcpus 2 "disk=none q=4 | run"
```

The run's `result.json` records the vCPU count, so `hydo repro <run dir>` runs it again on as many
vCPUs.

## Find where the verdict changes: `--sweep turns`

```sh
hydo explore --vcpus 2 --sweep turns --out turns
```

The sweep runs the schedule `disk=none | run` (or `--sweep-with "<schedule>"`) with 12 turn
lengths from 64 to 2,097,152 branches. Then it bisects every pair of neighbouring lengths whose
verdicts differ, until they are at most 100 branches apart. Each boundary's two lengths run a
second time. The options are:

- `--turn-range LO-HI`: the lengths searched (default `64-2097152`).
- `--turn-points N`: the size of the first grid (default 12).
- `--turn-resolution N`: how close the two lengths of a boundary get (default 100; 1 is exact).

The sweep prints each boundary with the `hydo run` command of both lengths. `report.md` has the
same, and every length it ran with its verdict and fingerprint. `exploration.json` has them in
`turn_sweep`, and `hydo repro <out dir>` runs the failing length of every boundary again.

The verdict need not change only once. The sweep bisects only between the lengths it ran, so a
narrow band of other verdicts between two grid points stays hidden. On the nats-server image the
default sweep made 19 runs and found one boundary, between `q=18051` (passes) and `q=18140`
(fails). A dense sweep of the range around it found five boundaries in 36 runs, and it stepped
over that one: `q=18140` fails, but its grid points `q=18098` and `q=18147` both pass.

```sh
hydo explore --vcpus 2 --sweep turns --turn-range 18000-19000 --turn-points 21 \
  --turn-resolution 10 --out turns-dense
```

| turn lengths | verdict |
|---|---|
| 18,000 to 18,165 | passes (but 18,140 fails) |
| 18,171 to 18,387 | fails |
| 18,394 to 18,455 | passes |
| 18,461 to 18,606 | fails |
| 18,612 to 18,802 | passes |
| 18,808 to 19,000 | fails |

## Where the forced preemption points fired (KVM and the Mac)

On one vCPU of the KVM or the Mac engine, `pct=<seed> pctn=<n> pctk=<exits>` puts `n` forced
preemption points at seeded VM exits among the first `pctk` exits of the workload. At a point
whose guest timer is armed, the guest's clock jumps to the timer's deadline, so the timer
interrupt preempts the thread that runs there. A verdict can change between `pctn=39` and
`pctn=40` (the Qdrant example `hydo/onboard/examples/qdrant-flaky-10200`). The first 39 points
of both schedules are the same, so the fortieth point decides. To see where each point fired, set
`HYDO_PREEMPT_RECORD=1`:

```sh
HYDO_PREEMPT_RECORD=1 hydo run --backend kvm "disk=none pct=ec699251106e8e0c pctn=40 pctk=3000 | run" --out r40
HYDO_PREEMPT_RECORD=1 hydo run --backend kvm "disk=none pct=ec699251106e8e0c pctn=39 pctk=3000 | run" --out r39
hydo diff r39 r40                          # the first point where the two runs' records differ
hydo inspect events r40 --kind preempt     # every point, at the stop that follows it
```

The run directory then has `one.preempt.jsonl`, one JSON line per point:

- `i`: the point's index in exit order.
- `exit`: the engine's exit count where it fired.
- `result`: what the point did. `moved` means the clock moved to the timer's deadline. `due`
  means the timer was already due. `missed` means no timer was armed, so the point did nothing.
- `vtime_ns`, `to_ns`: the guest's clock before the point and after it.
- `pc`, `mode`, `page_table`, `thread_pointer`, `sp`: the guest's registers at the point. They
  are the instruction, `user` or `kernel`, the page-table root (the process) and the user thread
  pointer (the thread). The thread pointer is the FS base on x86_64 and TPIDR_EL0 on arm64. The
  last one is the stack pointer.

The engine only reads registers, so the fingerprints are the same with and without the records.
hvx has no such record yet: its preemption points (`pp=`, `xt=`) fire inside the kernel module.

## Explore the timing

```sh
hydo explore --vcpus 2 --turns --timing --seeds 1-60
```

`--turns` draws a turn length for half of the schedules. `--timing` adds tick jitter. The report's
"by hand" lines carry `--vcpus`, so a copied line runs the same schedule. On the nats-server image
this exploration gave 56 failures and 4 passes at the fingerprint epoch 1. Every pass had turns
of at most 9,096 branches.
Each failure is in the class `test-failed`.

On hvx, `--preempt` draws a shorter tick period (`pp=65536` or `pp=16384`) or extra ticks
(`xt=`). With `--timing` too, the drawn tick jitter stays below half of that period, because hvx
refuses a jitter delay of a tick period or more. The engine refuses a schedule it cannot arm
before any VM, for example a hand-written `tjmax=500000` with `pp=65536`. `hydo run` stops with
the reason. The explorer records such a run with the class `refused` and counts it in
`refused`, never as a failure.

## A test that fails when its whole process stops

Some tests fail only when the whole test process gets no CPU for some milliseconds. A container
with a CPU limit does that: when the container has used its CPU time for the period, all its
threads wait. hydo's timing tokens change which thread runs next. They do not stop every thread
for a given time, so they do not reach such a failure.

A pause of the workload does. The timed pause `P=workload@<step>~<start ms>:<length ms>` stops
every process of the workload, the test program here, at a moment of guest time inside the step:

```sh
hydo run "disk=none | run P=workload@0~4:45"     # stopped 4 ms after the step's start, for 45 ms
hydo explore --pause-workload --seeds 1-100       # draws such pauses (hydo guide faults)
```

The example is `hydo/onboard/examples/etcd-flaky-22796` (etcd's `TestCacheCompactionResync`).
Its test never failed under the timing tokens alone. With a pause of the workload:

| engine | schedule | verdict | fingerprint |
|---|---|---|---|
| Mac | `disk=none \| run P=workload@0~4:45` | fails: `expected cached revision 5 after compaction, got 20` | `24dedf9da7af0a08` (3 runs) |
| Mac | `disk=none \| run P=workload@0~4:35` | passes: the pause is too short | `b8b9b46b4027b5af` (2 runs) |
| Mac | `disk=none \| run P=workload@0~8:45` | passes: the pause starts too late | `9402add9274c87ce` (2 runs) |
| hvx | `disk=none \| run P=workload@0~155:45` | fails, with the same line | `2ae2f4390bb012b8` (3 runs) |
| hvx | `disk=none \| run P=workload@0~155:30` | passes: the pause is too short | `3244703db66dae96` (2 runs) |
| hvx | `disk=none \| run P=workload@0~170:45` | passes: the pause starts too late | `42f3053533e11faa` (2 runs) |

The failing start differs between the engines, because each engine's guest clock runs at its own
pace. Find it with runs of `P=workload@0~<start>:60` at several starts. Or let
`hydo explore --pause-workload --pause-max <ms>` draw it: on hvx that exploration failed 3 of 200
schedules with `--pause-max 200`, on the Mac 13 of 100.

The same pause works on every engine, because the guest's own clock measures it. A CPU limit in
the guest (a cgroup `cpu.max` with busy loops beside the test) works only on hvx. On the Mac and
on KVM a busy loop makes no VM exits, so it uses no guest time and never uses up the limit.

## A test that fails when one thread stops

Some tests race two threads of one process: a check in the test's thread against a background
worker. Such a test fails only when the check's thread stops at the wrong moment while the worker
runs on. A pause of the workload stops both threads, so it does not reach the race. A pause of a
thread does:

```sh
hydo run "disk=none | run P=workload/nosuch@0~3000:10"           # names the workload's threads
hydo run "disk=none | run P=workload/tests::wal_reco@0~4433:500" # stops the test's thread only
```

The first run stops nothing. Its control transcript names the threads the workload has at that
moment: `busybox, collection.test, tests::wal_reco, tokio-rt-worker (4)`. The test's thread is
named after the test, cut to 15 bytes.

The example is `hydo/onboard/examples/qdrant-flaky-10200` (Qdrant's
`test_wal_replay_loads_pending_to_queue`). The test reads the length of a queue that a worker
empties, and fails at 0. On hvx with one vCPU, without a pause, it reads 35 on every run:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run P=workload/tests::wal_reco@0~4433:500` | fails: `length: 0, op_num: Some(500)`, the line of the upstream report | `4ae6099ea6e753a6` (3 runs) |
| `disk=none \| run P=workload/tests::wal_reco@0~4432:500` | passes: the pause starts 1 ms early, `length: 35` | `ab031f4dc2b53472` (3 runs) |
| `disk=none \| run P=workload/tests::wal_reco@0~4433:200` | passes: the pause is too short, `length: 3` | `f929e23e369aa175` (2 runs) |
| `disk=none \| run P=workload@0~4433:500` | passes: the whole process stops, `length: 35` | `fe44e7e86b9ea884` (2 runs) |
| the failing schedule on the fixed test | passes | `50b14ca4fb2e871d` (2 runs) |

The pause fails the test only when it starts while the test's thread waits for a blocking task.
Such windows are about 1 ms wide and come back every 2 to 4 ms. Find the moment from the test's own
output first: a run whose output lines carry the guest's uptime shows when the test reloads the
data. Then let the explorer draw starts around it:

```sh
hydo explore --pause-thread tests::wal_reco --pause-min 4400 --pause-max 4500 \
  --pause-lengths 500,1000 --seeds 1-100
```

On hvx it found 6 failures in 100 schedules, all reproduced, and shrank one to a 250 ms pause at
4,433 ms. With starts drawn from 0 to 5,000 ms it found none in 300 schedules. A pause of threads needs the guest kernel's cgroup
v2 freezer, so it runs on hvx only. The Mac and KVM engines refuse it: their guest kernels have no
cgroups.

## Steer the search by a number the test prints

The steered search is experimental: on the one test it was measured on, it did not help (the
numbers are at the end of this section). Some tests print how close they came to failing. The Qdrant example's test prints the length of
a queue that must not be empty, and fails at 0. A steered search keeps the schedules whose runs
printed the smallest numbers and spends half of its schedules on one change to them:

```sh
hydo explore --backend kvm --timing --preempt --seeds 1-400 --workers 1 \
  --steer-match 'length: (\d+)' --steer-stop
```

`--steer-match` takes the number from the first capture group. A manifest's `steer:` entry names
the pattern once, and `--steer <id>` takes it. A Rust test can report the number with
`hydo_assert::minimize` (the SDK's cargo feature `steer`). `--steer-goal max` steers towards
larger numbers. `--steer-mix 0` runs the same seeds without changes, which is the comparison to
make.

On the Qdrant test the steered search did not find the failure sooner. Over five seed ranges on
KVM, the plain exploration failed first after a median of 72 schedules, the steered search after
118. The changed schedules printed small lengths twice as often as random ones, but failed no
more often: the failing run's neighbour `pctn=39` prints 22, not 1. Report a number whose small
values come just before the failure. `docs/design/steered-search.md` in hydo's repository has the
measurements.
