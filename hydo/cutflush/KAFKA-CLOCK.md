# Kafka loses acked orders after a large forward clock jump

Investigation of the open finding in CLAUDE.md Milestone 63 (2026-10-22). Branch
`worktree-agent-a48ef760a3ebe6df6`. No module change. Results on the bare-metal test machine under
`/var/tmp/kclock`.

## The finding

On `explore-binlog-pin-disk.img` (the four-service stack with the MySQL binary log on), with
every store durable and an honest disk, this schedule fails, and it repeats:

```
disk=none dseed=e80bd3fbb759c1c1 | run K=redis@3+3 N=kafka@21-25:delay,20ms,reorder,11% P=kafka@16+1 T=21:15768000 Q=2d
HYDO-ORACLE: VIOLATION kafka order=0 expected=100000 got=MISSING (mysql has it: true)   fp 770096791c66765f
```

The explorer's original run (Milestone 63 broad check, seed 116) was
`... N=kafka@21-29:corrupt,4%,delay,20ms,reorder,11% P=kafka@16+1 T=21:31536000 Q=2d`
(`b34fe30e2af30ca5`). Both were re-run here with my copy of `hvx_explore` and gave the same
fingerprints (each twice).

## The cause (confirmed)

Kafka's **time-based retention** deletes the orders. Two parts of Kafka's log layer act
together; both judge time by the record timestamps against the broker's wall clock.

1. **Segment roll.** The producer (franz-go) stamps every record with the wall clock
   (CreateTime). After the guest's realtime clock is stepped forward before order 21, order
   21 carries a timestamp more than `log.roll.ms` (default 7 days) after the first record
   of the open segment, so Kafka closes that segment and starts a new one at offset 21
   (`LocalLog ... Rolled new log segment at offset 21`).
2. **Retention check.** The log manager's retention task (`kafka-log-retention`) first runs
   30 s after the log manager starts (a fixed delay in Kafka 3.5, `InitialTaskDelayMs =
   30000`) and then every `log.retention.check.interval.ms` (default 300 s). It deletes
   every closed segment whose newest record timestamp is more than `log.retention.ms`
   (default 7 days) older than the current wall clock. After the jump, segment 0 (orders 0
   to 20, all stamped 2026-01-01) is that old, so it is deleted and the log start offset
   moves to 21 (`UnifiedLog ... Deleting segment LogSegment(baseOffset=0, ...) due to
   retention time 604800000ms breach based on the largest record timestamp in the segment`,
   then `Incremented log start offset to 21 due to segment deletion`).

The oracle reads the topic from its first offset, so orders 0..20 are missing, although
the client was ACKed and MySQL has them.

### Why the threshold looked like "90 days passes, 120 days fails"

It is not about the size of the jump beyond 7 days. Two things decide the outcome:

- **Below 7 days nothing can happen.** Neither the roll nor the retention breach is
  possible. Measured on the diagnostic image with a wait (below): a jump of 604,000 s
  (6.99 days) passes; a jump of 605,000 s (7.002 days) fails.
- **Above 7 days it is a race** between the first retention check and the oracle's read
  of the topic. In this guest Kafka's log manager starts during provisioning, so the first
  check runs at about 60.8 s of guest uptime; the second would come 300 s later, long after
  the run. The workload ends at about 56 s. The oracle starts when the host sends `go`,
  and the host sends it only when a run call returns after the guest printed
  `hydo-explore-workload-done`; meanwhile the guest idles. The oracle's start time
  therefore varies from run to run by a few seconds (observed between 55.1 s and 60.5 s of
  uptime). If the oracle reads the topic before the check, it passes; if after, it fails.
  Any change to the jump value (or to the image) moves that point a little, so the pass/fail
  pattern above 7 days is effectively arbitrary but deterministic for each exact schedule.
  On the original image 8, 15, 30, 60 and 90 days happen to pass and 120, 150, 182 and 365
  days happen to fail; on the diagnostic image, 8 days fails and 90, 120 and 182 days pass
  (table below). The earlier diagnostic image that passed at 120 days was the same race.

The earlier broker log showed nothing because the stack's log4j level is WARN and Kafka
logs rolls and retention deletions at INFO.

## Evidence

Images (built on the build machine with the pinned tools, `HYDO_GCC_MUSL`, `HYDO_MUSL_ROOT`,
`HYDO_REDIS_OUT` and `HYDO_SEEDRNG` unset; all have the same `hydostack` as
`explore-binlog-pin`, `d76305f0…`, and the same data partition, `db90505a…`):

| image | build | disk sha256 |
|---|---|---|
| `explore-kclock-diag` | `STACK_KAFKA_DIAG=1` (Kafka log layer at INFO, LogManager at DEBUG, wall-clock timestamps; the relevant broker lines are printed to the console after the verdict) | `9e2a9450…` |
| `explore-kclock-dwait` | `STACK_KAFKA_DIAG=1 STACK_ORACLE_WAIT=10` (also waits 10 s of guest time before the oracle, so the first retention check always comes first) | `75942b57…` |
| `explore-kclock-fix` | default build with the fix (`log.retention.ms=-1`) | `2d748bd1…` |
| `explore-kclock-fwait` | the fix + `STACK_KAFKA_DIAG=1 STACK_ORACLE_WAIT=10` | `1a48ba75…` |

All schedules are the failing schedule with only `T=21:<seconds>` changed
(`noT` = without the clock jump). The `dwait` and `fix` columns were run twice (two
separate serve processes) with identical fingerprints; in the `diag` column the failing
8-day run was repeated (identical), the passing ones ran once; `fwait` ran once. The
`binlog-pin` values match the Milestone 63 sweep in `/var/tmp/m59-binlog/clock*` exactly,
and the 182-day and 90-day runs were repeated from a fresh boot (`hvx_explore one`) with the
same fingerprints.

| jump | binlog-pin (original) | diag | dwait (no fix, wait) | fix | fwait (fix, wait) |
|---|---|---|---|---|---|
| none | OK `9dd35a39` | OK `0ebf613f` | OK `428d7848` | OK `eea3eb8a` | OK `9e4eb57a` |
| 6 d (518,400 s) | OK¹ | – | OK `0e637f08` | OK `06df5e7e` | OK `3f8819b6` |
| 6.99 d (604,000 s) | – | – | OK `a8c3130b` | OK `8647b9b0` | OK `7f2470c2` |
| 7.002 d (605,000 s) | – | – | **FAIL** `24ad8d5b` | OK `9ae738be` | OK `0d476542` |
| 8 d (691,200 s) | OK `dccfd8c5` | **FAIL** `87063dde` | **FAIL** `1199cae4` | OK `89f83e40` | OK `b9e452d8` |
| 90 d | OK `e1942c9a` | OK `54a9c357` | **FAIL** `a913aac1` | OK `31b5f5ba` | OK `054c6b82` |
| 120 d | **FAIL** `f14e9123` | OK `259dee90` | **FAIL** `c0710bc9` | OK `dcf3b3e3` | OK `252013e5` |
| 182 d | **FAIL** `77009679` | OK `094a5491` | **FAIL** `00e70daf` | OK `e6ceaf96` | OK `305bea20` |
| 365 d (31,536,000 s) | – | – | **FAIL** `0d50ca17` | OK `ff9e7fd6` | OK `315c9f69` |

¹ from the Milestone 63 sweep. The original seed-116 schedule (365 days, with `corrupt`):
`explore-binlog-pin` FAIL `b34fe30e2af30ca5` twice, `explore-kclock-fix` OK
`15039d602e8c2d0a` twice.

Every FAIL is `HYDO-ORACLE: VIOLATION kafka order=0 expected=100000 got=MISSING (mysql has
it: true)`.

Broker log, `explore-kclock-diag`, 8-day jump (the guest clock reads 2026-01-01 before the
jump and 2026-01-09 after it; console `/var/tmp/kclock/diag1/T691200.log`):

```
2026-01-01 00:00:23,005 INFO kafka.log.LogManager Starting log cleanup with a period of 300000 ms.
2026-01-01 00:00:23,710 INFO kafka.log.LogManager Created log for partition orders-0 in /data/kafka-logs/orders-0 with properties {}
2026-01-09 00:00:47,034 INFO kafka.log.LocalLog [LocalLog partition=orders-0, dir=/data/kafka-logs] Rolled new log segment at offset 21 in 0 ms.
2026-01-09 00:00:52,063 DEBUG kafka.log.LogManager Beginning log cleanup...
2026-01-09 00:00:52,063 INFO kafka.log.UnifiedLog [UnifiedLog partition=orders-0, dir=/data/kafka-logs] Deleting segment LogSegment(baseOffset=0, size=1586, lastModifiedTime=1767225647887, largestRecordTimestamp=Some(1767225647887)) due to retention time 604800000ms breach based on the largest record timestamp in the segment
2026-01-09 00:00:52,066 INFO kafka.log.UnifiedLog [UnifiedLog partition=orders-0, dir=/data/kafka-logs] Incremented log start offset to 21 due to segment deletion
2026-01-09 00:00:52,071 DEBUG kafka.log.LogManager Log cleanup completed. 1 files deleted in 0 seconds
```
and the partition directory afterwards: `00000000000000000000.log.deleted` (1,586 bytes,
orders 0..20) next to the new `00000000000000000021.log`. The oracle finished at uptime
60.91 s, just after the check. In the passing runs on the same image (90, 120, 182 days)
the oracle finished at 56.3 s, the roll is logged, and no cleanup had run yet.

The 7-day boundary on `explore-kclock-dwait` (oracle at uptime about 59.7 s + 10 s):
at 604,000 s there is no roll and the check deletes nothing (`0 files deleted`); at
605,000 s the roll happens and the check deletes segment 0
(`largestRecordTimestamp=Some(1767225646795)`, check at wall time 1767830652.2, so the age is
605,005 s > 604,800 s).

With the fix (`explore-kclock-fwait`, 182 days): the roll still happens and the check
still runs, but deletes nothing:
```
2026-07-02 12:00:47,036 INFO kafka.log.LocalLog [...] Rolled new log segment at offset 21 in 1 ms.
2026-07-02 12:00:52,194 DEBUG kafka.log.LogManager Beginning log cleanup...
2026-07-02 12:00:52,194 DEBUG kafka.log.LogManager Log cleanup completed. 0 files deleted in 0 seconds
```

## The fix

`build-stack.sh`, Kafka `server.properties`: **`log.retention.ms=-1`** (no time-based
retention). The orders topic is a record of every order, and the oracle checks that every
ACKed order is in it, so nothing may expire; the old configuration (Kafka's default 7 days)
did not match that contract. The topic is the only data topic, so the broker-wide setting
is the smallest change; a topic-level `retention.ms=-1` would need a change to the order
service's topic creation (and a new binary). The segment roll still happens and is harmless.

Tested (table above): on `explore-kclock-fix`, every jump passes, including the original
failing schedule and the original seed-116 schedule, each twice with identical
fingerprints; on `explore-kclock-fwait`, where the retention check is forced to run before
the oracle, every jump passes and the broker log shows the check running with nothing to
delete, while the same image without the fix (`explore-kclock-dwait`) fails every jump over
7 days.

**Broad check** on `explore-kclock-fix`: the Milestone 63 configuration (`--strategy swarm
--concurrency --new-faults --timing`, 2 workers, `--no-shrink`), seeds 1–60, the same 60
schedule strings as the Milestone 63 run. 37 failures, all 37 reproduced, all in the
expected classes (18 lying disk, 13 I/O errors, 6 relaxed settings); the 22 runs with every
store durable and an honest disk all passed (as in Milestone 63, whose one durable failure
was seed 116). Compared with the Milestone 63 run on the same seeds, the class counts are
the same and two verdicts changed, both in fault classes: seed 28 (I/O errors; before MySQL
could not recover, now OK with 0 of 30 ACKed) and seed 53 (lying disk; the reverse). The
new image has different bytes, so the timing of every run shifts. Results
`/var/tmp/kclock/broad-fix`.

### Does the fix hide a real problem?

It hides the hazard for this stack by removing the feature that causes it, which is the
right configuration for a topic used as a permanent record. It does not change how Kafka
behaves with a finite retention: any deployment that keeps a finite `retention.ms` still
deletes data after a forward clock step larger than the retention. The explorer can still
show that: on the diagnostic image without the fix, every jump over 7 days fails once the
oracle runs after the first retention check.

Two smaller observations that the fix does not address:
- the explorer's verdict for a clock jump of more than 7 days depended on a race between
  the host's `go` and Kafka's first retention check; this is a property of the short
  workload, not of hvx (every run is deterministic for its exact schedule);
- `hydo/onboard/examples/stack/hydo.yaml` and `build-kafka.sh`/`build-linux.sh` §2l keep the
  default retention. They were left alone because changing them moves other baselines.

### Would a real Kafka deployment hit this?

Yes, under the same condition: when a broker's wall clock jumps forward by more than the
topic's retention (7 days by default), Kafka judges every segment's age against the new
clock, and at the next retention check (within 5 minutes by default) deletes every closed
segment whose newest record is older than the retention. Data that consumers have not read
yet is gone, and acked writes cannot be recovered from the broker. This is Kafka working as
designed (retention is defined by record timestamps and the wall clock), not a Kafka bug.
Real causes of such a step: a machine that boots with its clock far behind (dead RTC
battery, a VM restored from an old snapshot) and is then corrected by NTP, or an operator
setting the date. Forward steps smaller than the retention are harmless. In this stack the
producer and the broker share the clock, so the producer's timestamps jump too; in a real
cluster the same thing happens when the broker's clock jumps, because the retention check
uses the broker's clock. A system that keeps a topic as its record must set
`retention.ms=-1` (or a retention far longer than any credible clock error) and should
alert on clock steps.

## How the images were built and run

Build (build machine, tree `~/hydo-kclock`, `build/kbuild.sh <name> [env...]` builds one image,
removes the intermediates and copies the disk to the box):
```
build/kbuild.sh explore-kclock-diag  STACK_KAFKA_DIAG=1
build/kbuild.sh explore-kclock-dwait STACK_KAFKA_DIAG=1 STACK_ORACLE_WAIT=10   # before the fix was made the default
build/kbuild.sh explore-kclock-fix
build/kbuild.sh explore-kclock-fwait STACK_KAFKA_DIAG=1 STACK_ORACLE_WAIT=10
```
`STACK_KAFKA_DIAG`, `STACK_ORACLE_WAIT` and `STACK_KAFKA_EXTRA` (extra broker lines) are
new diagnostic switches in `build-stack.sh`; unset, they leave the image unchanged.

Runs (bare-metal test machine): `/var/tmp/kclock/sweep.py <image> <outdir> <list>` (a copy of the
Milestone 63 serve-mode driver pointed at `~/hydo-kclock/hvx/hvx_explore`). Results:
`orig1`, `diag1`, `diag-2`, `dwait-1`, `dwait-2`, `fix-1`, `fix-2`, `fwait-1`,
`s116-binlog-pin`, `s116-kclock-fix`, `broad-fix`.
