# TigerBeetle without hydo: issue 3960's unit test, and load that does not reach it

TigerBeetle's issue https://github.com/tigerbeetle/tigerbeetle/issues/3960 reports a replica that
stops on `assert(expires_at <= timestamp_event)` in `src/state_machine.zig`. The stop needs two
holds that expire on the same nanosecond and a pulse whose timestamp is that nanosecond. The
issue shows it with a unit test of the state machine, which needs no hydo. The section "Issue
3960" of [`../RESULTS.md`](../RESULTS.md) shows the same stop under hydo. There a release binary
reaches it from ordinary client requests, and the replica then cannot start again.

This folder holds two things:

- `unit-test.sh` runs the issue's own unit test. This is the native reproduction.
- `benchmark.sh` runs TigerBeetle's benchmark with holds. Native load like this never stopped
  the replica, because it does not put two holds on one expiry nanosecond.

## Prerequisites

- Git, `curl` and `unzip`. `unit-test.sh` downloads the Zig that TigerBeetle pins, with
  TigerBeetle's own `zig/download.sh`.
- About 1 GB of disk for the clone and the build in `work/`.
- Docker, only for `unit-test-in-docker.sh`.

## Steps

1. Run the issue's unit test on main of 2026-11-10 (`8cbd6065`), the day the issue was opened:

   ```sh
   sh unit-test.sh
   ```

2. Run the same test on the release 0.17.9, the release that this example runs under hydo:

   ```sh
   TB_COMMIT=cc1c06a924e49b11089c521b2209d34c92caaf18 sh unit-test-in-docker.sh
   ```

3. Run the benchmark with pending transfers:

   ```sh
   sh benchmark.sh
   ```

`unit-test.sh` clones TigerBeetle into `work/tigerbeetle-src` and appends
`issue-3960-test.zig`, the test copied from the issue, to `src/state_machine_tests.zig`. Then it
runs `./zig/zig build test:unit` with that test's name as the filter, and restores the file.

Step 2 uses the container, because on macOS 26.4.1 the Zig 0.14.1 of 0.17.9 fails to link its
build program. On Linux, `sh unit-test.sh` with the same `TB_COMMIT` does the same.

## The expected outcome

Step 1 fails with the assert of the issue:

```text
error: 'state_machine_tests.test.expire_pending_transfers: expiry not before expires_at' terminated with signal ABRT with stderr:
       thread 3431 panic: reached unreachable code
...
       /w/work/tigerbeetle-src/src/state_machine.zig:5281:23: 0x13e5a13 in execute (test-unit)
                       assert(expires_at <= timestamp_event);
```

Step 2 passes: `Build Summary: 7/7 steps succeeded; 2/2 tests passed`. The issue's numbers
place the pulse for main's timestamps. Under hydo, a construction for 0.17.9 stopped the
replicas of 0.17.9, 0.16.45 and 0.16.29, and not 0.16.28 (RESULTS.md).

Step 3 prints the benchmark's throughput and latencies, and the replica does not stop.

## The measured rates

| where | what | result |
|---|---|---|
| Mac, M2 Pro, macOS 26.4.1, 2026-11-14 | step 1 on macOS | the test aborts on the assert |
| same | step 1 in the Debian container | the test aborts on the assert |
| same | step 2, in the Debian container | the test passes |
| same | `sh benchmark.sh --transfer-pending --transfer-count=200000`, 11 s | the replica did not stop |
| Mac, before 2026-11-14 (RESULTS.md) | 0.17.9, 120 s of batches of 8,000 holds | the replica never stopped |
| same | 0.17.9, 101 s of `tigerbeetle benchmark --transfer-pending` | the replica never stopped |
| the bare-metal machine under hydo (RESULTS.md) | blind explorations with 2,983 holds in 651 batches | no two holds shared an expiry nanosecond |

The script of the 120 s run with batches of 8,000 holds was not kept, so this folder has only
the benchmark.
