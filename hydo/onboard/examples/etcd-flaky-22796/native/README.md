# The etcd flaky test without hydo: a CPU limit and a sleep

This folder reproduces etcd's flaky test `TestCacheCompactionResync` without hydo, from issue
https://github.com/etcd-io/etcd/issues/22796. The test fails with `expected cached revision 5
after compaction, got 20` when the whole test process gets no CPU for about 40 ms. Then the
cache's retry, 50 ms after the compaction, loads the second snapshot before the test's `Get`.
[`../RESULTS.md`](../RESULTS.md) explains the race.

Three scripts show it:

- `cpu-limit.sh` runs the test under a CPU limit that busy neighbours use up, as a CI pod's
  limit does. This is the native reproduction.
- `sleep-probe.sh` adds a 60 ms sleep before the test's `Get`. The sleep is a probe that shows
  the order of events, not a fix.
- `run-native.sh` runs the test without a limit. It does not fail.

## Prerequisites

- Git and Go. The scripts set `GOTOOLCHAIN=go1.27.4`, the toolchain line of `cache/go.mod`, so Go
  fetches that version once.
- For `cpu-limit.sh`: Linux with systemd and a user session (`systemd-run --user`), and
  `taskset`. It needs no root.

## Steps

1. Build the test program for this machine:

   ```sh
   sh build-program.sh
   ```

2. On Linux, run the test 100 times with 10% of a CPU and two busy loops:

   ```sh
   sh cpu-limit.sh work/cache.test 10% 2 100
   ```

3. Run it 100 times with 20% of a CPU and one busy loop, then with no busy loop:

   ```sh
   sh cpu-limit.sh work/cache.test 20% 1 100
   sh cpu-limit.sh work/cache.test 20% 0 100
   ```

4. On any machine, run the sleep probe three times:

   ```sh
   sh sleep-probe.sh 3
   ```

5. On any machine, run 100 processes of 10 test runs each without a limit:

   ```sh
   sh run-native.sh work/cache.test 100 10
   ```

`cpu-limit.sh` puts its second half into a new cgroup with `systemd-run --user --scope -p
CPUQuota=<quota>`. The busy loops and the test processes run there, all on CPU 0. Each test
process runs `-test.count=1 -test.cpu=1`, as the CI job's `CPU=1`. The outputs are in
`work/limit/`.

`sleep-probe.sh` clones etcd at commit `1b969e26` into `work/etcd-src`, inserts the sleep, runs
`go test` in `cache/`, and restores the file.

## The expected outcome

Steps 2 and 4 print the failed runs with the report's line:

```text
    cache_test.go:475: expected cached revision 5 after compaction, got 20
--- FAIL: TestCacheCompactionResync (0.07s)
```

Steps 3 and 5 differ: 20% with one busy loop fails most runs, and 20% alone fails none. Without
a limit, the test passes: `failed 0 of 1000 test runs`.

## The measured rates

| where | how | test runs | failed |
|---|---|---|---|
| Linux, Xeon E3-1270 v6, 8 threads, kernel 6.8 | CPU limit 10%, two busy loops, one CPU | 100 | 100 |
| same | CPU limit 20%, one busy loop, one CPU | 100 | 90 |
| same | CPU limit 20%, one CPU | 100 | 0 |
| Linux and Mac | no CPU limit, with and without load and `taskset` (RESULTS.md has each row) | 8,000 | 0 |
| Mac, M2 Pro, macOS 26.4.1 | `sleep-probe.sh 3` | 3 | 3 |
| Mac, M2 Pro, macOS 26.4.1 | `run-native.sh work/cache.test 20 10 4`, on 2026-11-14 | 200 | 0 |

The CPU-limit rows and the 8,000 runs are RESULTS.md's. The sleep probe failed 3 of 3 runs both
in RESULTS.md and with this script on 2026-11-14.
