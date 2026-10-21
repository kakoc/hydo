# Describing invariants: hydo's assertion protocol

Guest programs running under hvx describe what must (and what should at least once) be true by
emitting **assertion records** on the guest console. Harnesses and the explorer collect them from
the console and build a property table across one run or a whole exploration. This document is
the contract between guest code and host tools.

## Wire format (fixed)

Each record is one console line, written with a single `write()` so it is never interleaved:

```
HYDO-ASSERT-DECL {"t":"<type>","id":"<stable id>","msg":"<description>"}
HYDO-ASSERT {"t":"<type>","id":"<stable id>","ok":true|false,"msg":"<description>","details":{...}}
HYDO-SETUP-COMPLETE
```

- `HYDO-ASSERT-DECL` is the **catalog**: every assertion a program contains, emitted once early,
  so a property that is never evaluated can still be reported.
- `HYDO-ASSERT` is one **evaluation**. `details` is optional and free-form (JSON object).
- `HYDO-SETUP-COMPLETE` is the **lifecycle** signal: the system is provisioned and ready, faults
  may begin, and this is a good point to snapshot and branch from.
- Records may be preceded by other text on the same line (the parsers search for the tag).
- New fields may be added; existing ones are never removed or renamed.

## Types and verdicts

| `t` | Meaning | Verdict over a run or an exploration |
|---|---|---|
| `always` | the condition holds every time it is evaluated, and it is evaluated at least once | FAIL on any `ok:false`, or if never evaluated |
| `always_or_unreachable` | the condition holds every time it is evaluated | FAIL on any `ok:false`; never evaluated is fine |
| `sometimes` | the condition is true at least once | NOT EXERCISED if no `ok:true` anywhere |
| `reachable` | the code location is reached at least once | NOT EXERCISED if never emitted |
| `unreachable` | the code location is never reached | FAIL if emitted (it is emitted with `ok:false`) |
| `eventually` | after an anchor, the condition becomes true within a deadline (bounded liveness) | FAIL on any `ok:false` (an episode missed its deadline); NOT EVALUATED if no episode was resolved |

Numeric comparisons are `always` / `sometimes` evaluations whose `details` carry
`{"left":L,"op":"<=","right":R}` (Go SDK: `Cmp`).

**Bounded liveness: `eventually`.** The other types are safety properties ("never X",
"always Y"). An `eventually` property says "after an anchor point, X becomes true within a
deadline": "after the faults are healed, every pending order is resolved within 10 s", "after a
killed service restarts, it serves a request within 5 s". An unbounded "eventually" can never
be refuted by a finite run; a bounded one can, and because hvx makes guest time deterministic
(the virtual TSC is a function of the instruction clock), a deadline measured on the guest's
clock is deterministic too: the same schedule gives the same verdict and the same elapsed
time on every run. One **episode** = one anchor and one deadline. The episode is resolved by
ONE evaluation record:

- `ok:true` — the condition became true by the deadline;
- `ok:false` — the deadline passed first, or the condition became true only after it.

`details` carry the measured time: `elapsed_s` (string, seconds, shell + manifests) or
`elapsed_ms` (number, Go SDK), and the deadline `within_s` / `within_ms`; manifest
invariants also carry the anchor (`after`) and the check's last output (`out`). An
`eventually` property says that the system recovers after the faults stop, with an explicit
deadline, so "recovers" becomes a checkable claim.

**Why `sometimes` and `reachable` matter.** An `always` that never fails proves nothing if the
interesting case never happened. `sometimes` / `reachable` are coverage claims: "a retried order
was de-duplicated", "a service restart was observed". A property table where they are
NOT EXERCISED says the test did not reach the situation the invariant is about, and an
exploration should keep going (or the scenario is wrong).

## Determinism and volume

hvx keeps at most 64 KiB of console per VM, so SDKs must not flood it. The Go SDK emits each
(assertion, outcome) pair once per process (the first pass and the first failure, with the
details of that first one) and never emits a `sometimes` that evaluated false. The output is a
pure function of the program's evaluation sequence: no clocks, no host state, catalog in
declaration order, JSON with sorted keys. Counts in the property table are therefore counts of
emitted records (per process and outcome), not of every evaluation.

## Guest SDKs

**Go** (`hvx/stack/hydoassert`, import `hydostack/hydoassert`):

```go
var ackedInMySQL = hydoassert.Always("order.in_mysql", "every checked order is in MySQL with its amount")
var dedupHit     = hydoassert.Sometimes("server.kafka_dedup_hit", "the Kafka lookup found the order already there")
var restart      = hydoassert.Reachable("server.redis_redialed", "the front door re-dialed Redis")
var appliedNoAck = hydoassert.Unreachable("order.applied_but_not_acked", "an un-ACKed order reached Kafka")

hydoassert.Catalog()                                   // once, early: the DECL lines
ackedInMySQL.Check(got == want, hydoassert.D{"order": id, "got": got})
dedupHit.Check(found, nil)
restart.Hit(nil)
appliedNoAck.Hit(hydoassert.D{"order": id})
hydoassert.SetupComplete()

// bounded liveness
var drained = hydoassert.Eventually("orders.drained", "pending orders resolve within 10 s after the heal", 10*time.Second)
drained.Await(func() bool { return pending() == 0 }, time.Second, nil)   // blocks; one record
w := drained.Begin()      // or: anchor now ...
w.Done(nil)               // ... condition true now (ok if in time)  /  w.Expire(nil): gave up
```

Records go to `/dev/ttyS0` (override with `HYDO_ASSERT_OUT=<path>|stderr|off`), so they reach the
console even when the program's stdout is redirected to a log file.

**Shell** (`hvx/stack/hydoassert/hydo_assert.sh`, for `/sbin/init` scripts; uses `$BB sed`):

```sh
. /etc/hydo_assert.sh
hydo_decl always data.mounted "the data disk mounts after recovery"
hydo_always data.mounted "the data disk mounts after recovery" $BB mount -t ext4 /dev/vda2 /data
hydo_sometimes oracle.retried "some order needed a retry" $BB grep -q retry /tmp/client.log
hydo_setup_complete
hydo_eventually svc.back "redis serves again within 5 s" 5 1 sh -c '/bin/redis-cli ping | grep -q PONG'
```

`hydo_eventually <id> <msg> <within-s> <poll-s> <cmd...>` anchors at the call, polls `cmd`
every `poll-s` seconds of guest time and emits one record (`elapsed_s`, `within_s`).

The shell helper does not deduplicate; call it where an event happens once.

## Host side

- `hvx/hydo_assert_table.h`: header-only parser and aggregator (`ha_feed_console` once per run,
  `ha_print`, `ha_violations`), for harnesses and the explorer.
- `hvx/hydo_assert_report`: `hydo_assert_report <console>...` prints the property table over the
  given saved consoles (e.g. `HVX_CONSOLE_OUT` dumps) and exits 1 if any property is violated.
  For `eventually` properties it also shows the slowest in-time episode and prints a
  `liveness: <n> eventually properties, <k> missed a deadline` line.
- `explore.py` counts an `eventually` with `ok:false` as a failed assertion of the run (like a
  failed `always`), and `report.md` gets a "Liveness" table: per property, schedules where it
  held in time / missed its deadline, the slowest in-time episode and the first miss.
- Manifest invariants (`hvx/onboard/MANIFEST.md`) can be `type: eventually` with a deadline
  after the heal or the workload start; the generic init polls and emits the records.

## The four-service stack's properties

| id | type | what it says |
|---|---|---|
| `oracle.stores_readable` | always | after recovery the oracle can read MySQL, Kafka and Redis |
| `order.in_mysql` | always | every checked order is in MySQL with its amount |
| `order.in_kafka` | always | every checked order is in Kafka with its amount |
| `order.kafka_exactly_once` | always | every checked order is in Kafka exactly once |
| `order.acked_marked_published` | always_or_unreachable | an ACKed order is marked published in MySQL |
| `order.redis_never_wrong` | always | Redis never holds a wrong amount (a miss is allowed) |
| `order.applied_but_not_acked` | unreachable | an un-ACKed order reached Kafka |
| `oracle.redis_cache_miss` | sometimes | the oracle saw a Redis cache miss |
| `oracle.pending_order` | sometimes | an un-ACKed order was left pending in MySQL |
| `server.place_acked` | sometimes | a Place call was ACKed |
| `server.store_error` | sometimes | a Place call failed on a store error |
| `server.kafka_recheck` | sometimes | an uncertain earlier produce was looked up in Kafka first |
| `server.kafka_dedup_hit` | sometimes | that lookup found the order already there (a duplicate avoided) |
| `server.redis_redialed` | reachable | the front door re-dialed Redis after its connection broke |
| `client.place_failed` | sometimes | a client Place attempt failed |
| `client.retry_recovered` | sometimes | a failed order was ACKed on a later attempt |
| `client.retry_exhausted` | sometimes | an order was still un-ACKed after all retry attempts |

"Checked" orders are all orders in the plain runs and the ACKed ones in the fault runs. The
`HYDO-ORACLE` verdict line is derived from these checks (the first violation in id order).
