# Qdrant's flaky test without hydo: it does not fail natively

This folder holds the native runs of the section "Native rates" in
[`../RESULTS.md`](../RESULTS.md). The test is `tests::wal_recovery_test::test_wal_replay_loads_pending_to_queue` in Qdrant's crate
`collection`, from issue https://github.com/qdrant/qdrant/issues/10200. It fails when the update
worker applies all 35 queued operations before the test reads the queue length.

The point of these runs is that the test did not fail natively. In 2,604 runs on a Mac and on a
Linux machine, with one CPU, several worker-thread counts and CPU load, no run failed. The
reproduction with hydo is in RESULTS.md.

## Prerequisites

- A test program of the crate `collection` at the issue's commit `222b2e44`:
  - On a Mac or another host, `sh build-program.sh` builds it into `work/collection.test`. It needs
    Git, rustup with Rust 1.99.0, and protoc; Qdrant's CI pins protoc 34.1.
  - On Linux x86_64, `../build-test.sh` builds the pinned program `../bin/collection-buggy.test`.
- On Linux, `taskset` for the one-CPU runs.

## Steps

1. Build the program, on a Mac for example:

   ```sh
   PROTOC=/path/to/protoc sh build-program.sh
   ```

2. Run the test 224 times, one process at a time:

   ```sh
   sh run-native.sh work/collection.test 224
   ```

3. Run it 400 times, 12 processes at once, beside 18 busy loops:

   ```sh
   BUSY=18 sh run-native.sh work/collection.test 400 12
   ```

4. On Linux, run it 240 times on one CPU, with the test data on tmpfs:

   ```sh
   CPUS=0 TMPDIR=/dev/shm sh run-native.sh ../bin/collection-buggy.test 240
   ```

`run-native.sh` runs each process as `<program> --exact <test> --test-threads 1 --nocapture`, as
cargo-nextest does in Qdrant's CI. `TOKIO_WORKER_THREADS` passes through to the test. The output
of each run is in `work/runs/`.

## The expected outcome

Each run prints `failed 0 of <runs>` and the queue lengths that the test read. A length greater
than 0 means the test passed. On an idle machine every length is 35: the worker took no operation in
the window. CPU load and one CPU lower some lengths, but none reaches 0.

## The measured rates

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

The table is RESULTS.md's. The Linux runs had the test data on tmpfs (`TMPDIR=/dev/shm`).
