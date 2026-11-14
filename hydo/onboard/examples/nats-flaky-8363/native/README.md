# A flaky test of the NATS server without hydo: one processor

This folder reproduces nats-server's flaky test `TestRegisterAndUnregister` (package
`server/ats`) without hydo, from issue https://github.com/nats-io/nats-server/issues/8363. The
test fails with `Expected same number of go routines after removing all registered: 3 vs 2`.
`Unregister()` returns before the ticker goroutine exits, and the next iteration counts that
goroutine. [`../RESULTS.md`](../RESULTS.md) explains the race.

`GOMAXPROCS=1` gives the Go runtime one processor, a P, to run goroutines on. With one P the
test is not flaky: every second iteration of a process fails. A failure needs two iterations in
one process, so a run with `-test.count=1` never fails.

## Prerequisites

- Git and Go. `build-program.sh` sets `GOTOOLCHAIN=go1.26.10`, the module's toolchain line, so Go
  fetches that version once.

## Steps

1. Build the test program for this machine, at main of 2026-11-06 (`59bf235a`):

   ```sh
   sh build-program.sh
   ```

2. Run two processes of 100 iterations each with one P:

   ```sh
   sh run-native.sh work/ats.test 1
   ```

3. Run the same with two Ps, and with Go's default:

   ```sh
   sh run-native.sh work/ats.test 2
   sh run-native.sh work/ats.test default
   ```

Each process runs `GOMAXPROCS=<n> <program> -test.run '^TestRegisterAndUnregister$'
-test.count=100`. The outputs are in `work/runs/`.

On Linux x86_64, `../build-test.sh` builds the pinned static program `../bin/ats-buggy.test`,
which `run-native.sh` runs the same way.

## The expected outcome

Step 2 prints 50 failed iterations for each process:

```text
process 1: 50 of 100 iterations failed
process 2: 50 of 100 iterations failed
GOMAXPROCS=1: failed 100 of 200 iterations
      2 ats_test.go:72: Expected same number of go routines after removing all registered: 3 vs 2
```

Step 3 fails fewer iterations, and the number changes from run to run.

## The measured rates

| where | GOMAXPROCS | failed iterations |
|---|---|---|
| Mac, M2 Pro, 12 cores, macOS 26.4.1 | 1 | 100 of 200 (RESULTS.md, and `run-native.sh` on 2026-11-14) |
| same | 2 | 1 of 200 (RESULTS.md); 4 of 200 (`run-native.sh` on 2026-11-14) |
| same | default (12) | 27 of 200 |
| Linux, Xeon E3-1270 v6, 8 threads, kernel 6.8 | 1 | 100 of 200 |
| same | 2 | 25 of 200 |
| same | default (8) | 96 of 200 |

Add `-test.v` and the rate with one P falls: on 2026-11-14 the same program failed 71 of 200
iterations with it. The script leaves it out, as RESULTS.md's command does.
