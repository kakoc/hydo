I could make this failure happen on every run, which may help with reviewing #11028.

**Repro**

I ran the test binary of 222b2e44 (dev profile, one test per process as nextest does) in a deterministic VM with one vCPU. The VM can force the guest kernel to switch threads at chosen VM exits ("preemption points", picked from a seed). With 40 such points the test fails on every run with exactly the line from the report:

```
Post-load update queue info: ShardUpdateQueueInfo { length: 0, op_num: Some(500), deferred_points: Some(0) }
... panicked at lib/collection/src/tests/wal_recovery_test.rs:581:5:
update queue should have pending operations after WAL replay
```

With 39 points from the same seed it passes with `length: 22, op_num: Some(477)`. Without forced points it passes with `length: 18, op_num: Some(481)`. Each of these ran twice from a fresh boot with the same result.

**What it shows**

1. `load_from_wal` sends the 35 tail entries (465..499) to the update channel while the update worker already runs.
2. After `LocalShard::load` returns, `local_update_queue_info()` first waits for a `spawn_blocking` task (the deferred point count) and only then reads `update_queue_length()`.
3. In the failing run the worker applies all 35 entries in that window (`op_num: Some(500)`), so the length is 0. In passing runs length + applied entries add up to 35.

So it's the race the comment above the assert describes, not lost data. With #11028 applied the test passes on the failing schedule.

**Native**

I could not make it fail natively: 0 of 2,604 runs on an M2 Pro (idle, 12 at once, with busy loops, `TOKIO_WORKER_THREADS` 1/2/64) and a Xeon E3-1270 v6 (idle, `taskset -c 0`, with load). On one CPU the length goes down to 23, never to 0.

One more thing: with 3 or 4 vCPUs that run in turns, the same test fails on the 2 second wait with `Timeout waiting for update queue to empty`, the message of #10103. I didn't check whether that happens outside the VM.

I found this with hydo (https://github.com/kakoc/hydo), a deterministic hypervisor I'm working on. The setup and all runs are in hydo/onboard/examples/qdrant-flaky-10200 there. Happy to share the image if it's useful for checking another fix.
