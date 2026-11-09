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

- On one vCPU (the KVM engine, the Mac engine, or hvx without `--vcpus`), a second thread runs
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
