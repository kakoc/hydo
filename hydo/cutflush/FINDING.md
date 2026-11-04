# Finding: "acked but not marked published" at a power cut on the k-th FLUSH

> **Re-baselined 2026-10-22 (docs/REBASELINE.md).** The images below were rebuilt with the pinned toolchain (`*-pin-disk.img`); only `/bin/hvx_seedrng` changed, which shifts guest timing a few FLUSHes. The bug is still there. On `explore-nf-pin-disk.img` the minimal reproducer is now `disk=cut dseed=0 cf=209 | run` (fingerprint `8666c33298537074`; cf=209, 210 fail on order 22, cf=218 on order 23); the cf=204–264 points below pass on the -pin image. On `explore-bench-bg-fixed-pin-disk.img`, `cf=242` still fails but names order 23 (`368731d6ef0e33d6`); cf=253, 262, 271, 303 fail. The values below remain valid for the original (stand-in) images.

Checked 2026-10-21 on the bare-metal machine (main `hvx.ko` with `HVX_SET_FLUSHCUT`, one
worker, results under `/var/tmp/m57-cfcheck`). This note answers the open item of
Milestone 56: on the fixed front door with durable settings and an honest disk, 12 of 282
cut points (`disk=cut dseed=0 cf=<k> | run`, k in 204, 205, 213, 221, 229, 237, 238, 246,
254, 255, 263, 264) end with `HYDO-ORACLE: VIOLATION mysql order=N acked but not marked
published`.

## Verdict

The violation is a real bug in the order service's protocol, not in MySQL 8.4 or ext4.
The cut model is what exposes it, and the model is not the same as an instant power loss
of the whole machine.

- **What goes wrong.** The order service ACKs an order after it READS `published = 1`
  from MySQL. That value can be visible but not yet durable. MySQL runs with `skip-log-bin`,
  and with the binary log off InnoDB makes a commit visible to other sessions (it releases
  the row locks and marks the transaction committed in memory) before the redo-log fsync
  that makes the commit durable has finished. A retry of the order runs on a new connection,
  sees `published = 1`, skips the publish step, and ACKs. The fsync then never completes,
  and after the power cut the row has `published = 0`.
- **MySQL and ext4 behave as designed.** MySQL guarantees durability to the session whose
  COMMIT returned. Here that COMMIT never returned (the first attempt got "deadline
  exceeded"). ext4 issued the FLUSH requests that the fsync needs; the device never
  completed them.
- **Why the cut model matters.** With `cf=k` only the data partition loses power. The CPU,
  the network and every process keep running. The violating ACK is sent about 160 to 200 s
  (guest time) after the cut. With an instant loss of power to the whole machine, the order
  service could not send anything after the cut, so this exact trace cannot happen. The
  trace is possible for a different, real failure: **the disk stops completing requests
  (a hung disk), the services keep running for longer than the client deadline (20 s),
  and then the machine crashes or the disk loses its write cache.** The same window also
  opens without a long hang when a second request for the same order reads the row during
  the normal fsync time of the first request; the M50 per-order lock in the current front
  door does not close it, because the lock is released when the first request returns on
  its deadline, while its MySQL commit is still waiting.
- **Still present in the current front door** (with the M50 fixes): 13 of 118 cut points
  (k = 200 to 317) fail in the same way.

So the answer to "(a) real bug, (b) expected consequence of the protocol, (c) artefact of
the cut model" is: it is (b) — a defect in the front door's protocol, which trusts a read
of possibly non-durable state — and the trigger needs the (c) property of the model
(the guest keeps running with a dead disk). It is not (a).

## The exact sequence (explore-nf-disk.img, cf=204, order 22)

1. `place 22`, attempt 1: `INSERT ... ON DUPLICATE KEY UPDATE` commits (durable), the
   Kafka produce returns (Kafka's own fsync completed), then
   `UPDATE stack.orders SET published = 1 WHERE id = 22` runs. InnoDB commits it in
   memory, so `published = 1` becomes visible, and then waits for the redo fsync.
2. FLUSH number 204 is the FLUSH of that redo fsync (with `innodb_flush_method=fsync` the
   fsync of the redo file can send one or two FLUSHes, which is why some orders fail at two
   consecutive k values: 204/205, 237/238, 254/255, 263/264). The device loses power at
   that FLUSH: it is not executed, and no later request on the data partition completes.
   `mysqld`, `jbd2/vda2-8` and the Kafka threads block in D state (hung-task reports on
   the console).
3. The UPDATE never returns. After 20 s the client sees
   `place 22 failed (unknown): outcome-unknown: mysql publish flag: context deadline exceeded`.
4. Orders 23 to 29 fail (their INSERT commits hang the same way).
5. The client's retry phase: `retry 22`, attempt 1, on a new MySQL connection. The INSERT
   is a no-op (row exists, nothing changes, so no redo and no fsync). The SELECT returns
   `published = 1` (the in-memory commit of step 1). The service skips Kafka and the
   flag update, the Redis write fails after its 2 s cap and is skipped
   (`server.cache_write_skipped`), and the service ACKs
   (`client.retry_recovered {"attempt":1,"order":22}`). No Kafka re-check ran
   (`server.kafka_recheck` never appears), which is only possible if the SELECT returned 1.
6. Recovery boot from the flushed bytes: row 22 exists with `published = 0`, Kafka has the
   record, and the client was ACKed: `VIOLATION mysql order=22 acked but not marked
   published`.

The other points are the same sequence for orders 23 to 29. The neighbouring cuts behave
as expected: cf=203 lands in order 22's Kafka produce (the attempt ends "outcome unknown",
no ACK); cf=206 lands on Redis's fsync after the flag is durable (order 22 is ACKed
correctly).

**Why only the last 8 orders.** For an earlier cut the retry of the affected order cannot
reach MySQL: every request that times out leaves a MySQL server thread stuck in a commit
that never finishes, and by the retry phase MySQL refuses new connections
(`Error 1040: Too many connections`, seen in the guest's client log for cf=195, order 21).
This is a side effect of the guest running long after the cut, not a separate bug.

**Why cuts between orders (`C=`) never fail.** `C=` cuts only between client requests,
after every ACKed order's commits have returned, and freezes all servers. No commit is in
flight, so the window cannot open.

The guest-side text above (client log lines) was read from guest RAM, dumped after the
workload by a diagnostic copy of `hvx_explore` (`HVX_EXPLORE_RAM_OUT`, built outside the
repo in `/var/tmp/m57-cfcheck/src`). The FLUSH-to-fsync mapping in step 2 is inferred from
which calls returned and which did not; individual virtio requests were not traced.

## Evidence

All runs on the main module through `~/bin/hvx-run`, one worker at a time.

| check | result |
|---|---|
| the 12 points on `explore-nf-disk.img`, 2 more runs each (`nf-rerun`) | 24/24 same verdict and same fingerprint as the M56 sweep (e.g. cf=204 `e592e6c6fb6e5f3a`) |
| cf=204 and cf=195 from a fresh boot (`one`, with RAM dump; `diag-cf204`, `diag-cf195`) | same fingerprints as in serve mode; client logs as quoted above |
| FLUSH count on the current front door (`explore-bench-bg-fixed-disk.img`, `cfn`) | 317 per 30-order workload (281 on the old image; the current service has one more UPDATE per order) |
| current front door, k = 200 to 317 (`fixed-cf`) | 13 VIOLATION (`acked but not marked published`, orders 22 to 29), 105 OK |
| the 13 failing points, 2 more runs each (`fixed-rerun`) | 26/26 same verdict and fingerprint |
| cf=242 on the current front door from a fresh boot (`diag-fixed-cf242`) | `place 22 failed (unknown): ... mysql publish flag: context deadline exceeded`, `retry 22` ACKed on attempt 1, same fingerprint `5c04f27153945d7b` |

Failing points on the current front door: 242, 243 (order 22), 252 (23), 261, 262 (24),
271 (25), 281 (26), 290 (27), 300, 301, 302, 303 (28), 314 (29).

## Minimal schedules

- Old front door (0112b48): image `explore-nf-disk.img`, schedule
  `disk=cut dseed=0 cf=204 | run`, fingerprint `e592e6c6fb6e5f3a`.
- Current front door (M50 fixes): image `explore-bench-bg-fixed-disk.img`, schedule
  `disk=cut dseed=0 cf=242 | run`, fingerprint `5c04f27153945d7b`.

Reproduce on the bare-metal machine:

```
~/bin/hvx-run sudo env HVX_SNAP_DIR=/var/tmp/hvx-snap ~/hydo-cutflush/hvx/hvx_explore one \
  ~/hydo/build/bzImage ~/hydo/build/initramfs-kafka.cpio.gz \
  ~/hydo/build/explore-bench-bg-fixed-disk.img /var/tmp/m57-cfcheck/repro \
  "disk=cut dseed=0 cf=242 | run"
```

## Where it lives and possible fixes

`hydo/stack/main.go`, `place`: the branch where the SELECT returns `published == 1` goes
straight to Redis and the ACK (in the old front door, `hydo/bench/base/main-0112b48.go`, the
`published == 0` test). Neither version makes sure that the `published = 1` it read is
durable. Possible fixes (not tested):

1. Before ACKing on a `published = 1` that this request did not write itself, commit a
   write transaction that really changes a row (for example a counter column on the order).
   With `innodb_flush_log_at_trx_commit=1` its commit flushes the redo log up to its own
   position, which covers the earlier commit. A statement that changes nothing (the
   current `ON DUPLICATE KEY UPDATE id = id`) writes no redo and does not help.
2. Run MySQL with the binary log on and `sync_binlog=1` (MySQL's defaults; the stack sets
   `skip-log-bin` in `build-stack.sh`). Then a commit becomes visible only after the binary
   log fsync, and recovery keeps any transaction that is in the binary log. This is the
   expected behaviour of the MySQL group commit; it was not run here.

If neither is done, an ACKed order can lose its published flag after a hung disk followed
by a crash, and the oracle's invariant "an ACKed order is marked published" does not hold.
In the current front door the durable row keeps `published = 2`, so a later retry looks in
Kafka first and does not send a duplicate. In the old front door the durable row has
`published = 0` and the restarted service has forgotten that a send was started, so a
retry would send the order to Kafka a second time.

## Fix (Milestone 63): the binary log on, `sync_binlog=1`

The stack's MySQL now runs with the binary log and `sync_binlog=1` (`build-stack.sh`). A commit
then becomes visible only after both logs are on disk, and crash recovery commits everything
in the binary log, so a retry can no longer read a `published = 1` that is not durable. With
cuts at every FLUSH from 200 to the end of the workload, the current front door passed 386 of
386 cuts (before: 13 of 118 failed) and the old front door passed 292 of 292 (before: 12 of 282
failed). The workload now makes 585 and 491 FLUSHes (was 317 and 281), because every commit
also syncs the binary log. Results: `/var/tmp/m59-binlog` on the bare-metal machine; details in CLAUDE.md
Milestone 63.
