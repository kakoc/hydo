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

hvx keeps at most 64 KiB of console per VM, so SDKs must not flood it. The Go, Rust and JavaScript SDKs emit each
(assertion, outcome) pair once per process (the first pass and the first failure, with the
details of that first one) and never emit a `sometimes` that evaluated false. The output is a
pure function of the program's evaluation sequence: no clocks, no host state, catalog in
declaration order, JSON with sorted keys. Counts in the property table are therefore counts of
emitted records (per process and outcome), not of every evaluation.

## Guest SDKs

**Go** (`sdk/go`, import `github.com/kakoc/hydo/sdk/go/hydoassert`; `hydo/stack/hydoassert` is a
frozen copy that the order service imports as `hydostack/hydoassert`):

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

**Shell** (`hydo/stack/hydoassert/hydo_assert.sh`, for `/sbin/init` scripts; uses `$BB sed`):

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

**Python** (`sdk/python`, distribution `hydo-assert`, import `hydo_assert`; standard library
only, Python 3.8+, thread- and asyncio-safe):

```python
import hydo_assert as ha

ACKED = ha.always("kv.acked_durable", "every acknowledged PUT reads back with its value")
REPLAYED = ha.sometimes("kv.recovered", "the server replayed a non-empty log at start")
TORN = ha.reachable("kv.torn_tail", "the log ended in a partial line")
BAD = ha.unreachable("kv.bad_command", "the server got a command it does not know")
SIZE = ha.always("kv.log_size", "the log holds at most 10000 entries")
BACK = ha.eventually("kv.back", "the server answers within 5 s", 5.0)
LOST = ha.buggify("kv.put.lost_reply", "the PUT is logged but its reply is lost")

ha.catalog()                                     # once, early: the DECL lines
ACKED.check(got == want, {"key": k, "got": got})
TORN.hit()
SIZE.less_than_or_equal_to(n, 10000)            # = SIZE.cmp(n, "<=", 10000)
if LOST.fire({"key": k}): ...
BACK.await_cond(ping_ok, poll=0.2)              # asyncio: await BACK.await_async(aping, 0.2)
ha.setup_complete()
```

The records, the deduplication (each (assertion, outcome) once per process; a false
`sometimes` never written; a record with a NaN dropped), the BUGGIFY decisions and the
`HYDO-BUGGIFY` line are the Go SDK's, byte for byte (the shared cases below; Python 3.8 and
3.15). Details are any mapping,
encoded as Go's `encoding/json` encodes the same Go value. `HYDO_ASSERT_OUT` works as in Go, plus
`logging` (each line goes to the Python logger `hydo_assert`). The catalog lists assertions in
the order their declarations ran, so declare them at module level. `sdk/python/README.md` has
the whole API, and `hydo/onboard/examples/python-kv` is a Python service that uses it under hydo.

**Rust** (`sdk/rust`, crate `hydo-assert`; std only; README in the crate):

```rust
use hydo_assert::{declare, details, hydo_always, hydo_buggify, hydo_reachable};

declare! {                                  // named statics, in the catalog before main
    static ACKED: always("kv.acked_durable", "every acknowledged write reads back");
    static BACK: eventually("kv.back", "the server answers within 5 s", Duration::from_secs(5));
    static LOST_REPLY: buggify("kv.put.lost_reply", "the write lands but the reply is lost");
}
hydo_assert::catalog();                     // once, early: the DECL lines
ACKED.check(got == want, details!{"key": k, "got": got});
hydo_always!(n <= cap, "kv.log_size" => "the log stays under its cap", {"n": n});   // file:line captured
hydo_reachable!("kv.torn_tail" => "the log ended in a partial line");
if LOST_REPLY.fire(()) { return; }          // or inline: hydo_buggify!("id" => "message")
BACK.await_cond(|| ping().is_ok(), Duration::from_millis(100), ());   // await_async for async code
hydo_assert::setup_complete();
```

The run-time API (`hydo_assert::always(id, msg)`, `.check`, `.hit`, `.cmp`, `eventually`,
`buggify`, `catalog`, `setup_complete`) is the Go SDK's, and for the same sequence of calls it
writes the same bytes (it is the reference: its tests write the shared cases below). Same output switch
(`HYDO_ASSERT_OUT`), same deduplication, same BUGGIFY configuration (`$HYDO_BUGGIFY`, else
`/tmp/hydo-buggify`) and decisions. Rust additions: call-site macros (`hydo_always!`,
`hydo_sometimes!`, `hydo_reachable!`, `hydo_unreachable!`, `hydo_always_or_unreachable!`, the
comparison macros, `hydo_buggify!`, `hydo_eventually!`) whose records carry one more field,
`"loc":"<file>:<line>"`, last; `declare!`; both register before `main` with a std-only
constructor, so a property in code that never runs is still in the catalog. Catalog order:
`declare!` entries and macro sites sorted by file, line and column (a `declare!` block in the
order written), then run-time declarations in declaration order. Feature `noop` compiles
everything to nothing (BUGGIFY always false); feature `serde_json` accepts `serde_json::Value`
details. `hydo_assert::emit_line(tag, &value)` writes any other one-line record kind (the
extension point for a history recorder).

For an onboarded project, `buggify: true` in `hydo.yaml` makes the guest init write the
schedule's `B=` token to `/tmp/hydo-buggify` (`hydo/onboard/MANIFEST.md`), as `stack-explore.sh` (deleted in M146)
does for the stack. `hydo/onboard/examples/rust-kv` is a complete example.

**JavaScript** (`sdk/js`, package `hydo-assert`, Node.js; no dependencies; CommonJS and ESM;
TypeScript types; README in the package):

```js
const ha = require('hydo-assert');               // or: import * as ha from 'hydo-assert'
const ACKED = ha.always('kv.acked_durable', 'every acknowledged write reads back');   // module top level
const LOG_SIZE = ha.always('kv.log_size', 'the log stays under its cap');
const BACK = ha.eventually('kv.back', 'the server answers within 5 s', 5000);
const LOST_REPLY = ha.buggify('kv.put.lost_reply', 'the write lands but the reply is lost');
ha.catalog();                                     // once, early: the DECL lines
ACKED.check(got === want, { key: k, got });
ha.reachable('kv.torn_tail', 'the log ended in a partial line').hit({ bytes });
LOG_SIZE.lessThanOrEqualTo(size, cap);            // = LOG_SIZE.cmp(size, '<=', cap)
if (LOST_REPLY.fire({ key })) { socket.destroy(); return; }
await BACK.awaitCond(async () => (await ping()) === 'pong', 100);   // awaitCondSync blocks
ha.setupComplete();
```

The API is the Go SDK's in camelCase (`alwaysOrUnreachable`, `check`, `hit`, `cmp`,
`eventually(id, msg, withinMs)` with `begin`/`done`/`expire`, `buggify`/`fire`, `catalog`,
`setupComplete`), plus the named comparison helpers (`greaterThan`,
`greaterThanOrEqualTo`, `lessThan`, `lessThanOrEqualTo`) and `emitLine(tag, value)` for other
one-line record kinds. For the same sequence of calls it writes the same bytes as the Go SDK
(the shared cases below, through both entries). Records are written synchronously
(`fs.writeSync`, one write per record) to `/dev/ttyS0`; same output switch (`HYDO_ASSERT_OUT`,
`off` makes it a no-op), same deduplication, same BUGGIFY configuration and decisions. Details
are plain objects or `Map`s; numbers, bigints (Go's 64-bit integers), `Uint8Array` (base64,
Go's `[]byte`) and strings are encoded as Go encodes the matching Go values, keys in UTF-8 byte
order; a NaN, an infinity or a cycle drops the record, as `json.Marshal` fails on them. Go 1.27
writes invalid UTF-8 as a raw U+FFFD; the SDK writes Go 1.26's `\ufffd` escape.
`hydo/onboard/examples/node-kv` is a complete example (Mac backend).

**The shared cases** (`sdk/cases`; the format and the rules for reading it are in
`sdk/rust/README.md`, "The shared cases"). The Rust SDK is the reference: its test
`cargo test --test cases` (in `sdk/rust`) runs each case's calls and writes, for every run,
the exact bytes the sink receives and what the calls return, under the run's output target
(`HYDO_ASSERT_OUT` / `HYDO_HIST_OUT`) and BUGGIFY configuration. The other SDKs read every case,
make the same calls and compare: Go `go test ./...` in `sdk/go` and in `hydo/stack/hydoassert` (Go 1.26.5,
the guest images' Go; with Go 1.27 or later the invalid-UTF-8 cases are skipped), Python
`python3 -m unittest discover -s sdk/python/tests`, JavaScript `node --test` in `sdk/js`. A
case needing what a language lacks (float32, invalid UTF-8 in a JavaScript history record,
`emit_line` in Go) is skipped and reported. A format change starts in Rust: change it, run
`HYDO_BLESS=1 cargo test --test cases`, commit `sdk/cases`, and the other SDKs' tests fail on
the lines they must change. To add a case, add it to `sdk/rust/tests/cases/defs.rs` and bless.
The cases equal what Go 1.26.5 writes (docs/HISTORY.md, Milestone 110).

**Reading the stores** (`hydo-read` / `hydo-check`, `hydo/crates/hydo-read`; full reference in
`hydo/onboard/MANIFEST.md`, "Reading the stores"). A cross-service property often needs what
is in a MySQL/PostgreSQL table, on a Kafka topic, under Redis or etcd keys. Any SDK or script
can run the static helper instead of linking a client for every store:
`hydo-read kafka 127.0.0.1:9092 orders` prints the records as sorted JSON Lines (exit 2 = could
not read), and `hydo-check exactly-once --left 'acked=file /tmp/acked' --right 'kafka 127.0.0.1:9092 orders'
--id order.in_kafka_once --console /dev/ttyS0` evaluates a set-style property and writes the
`HYDO-ASSERT-DECL` + `HYDO-ASSERT` records (details: `check`, `first`, `explanation`) to the
console itself; its exit status (0 holds, 1 violated, 2 could not evaluate) and last output line
are the result. In a manifest invariant, leave out `--id`: the init already turns the exit
status and the last line into the record.

## Recorded histories (host-side checkers)

Assertions are evaluated inside the program. For linearizability and transactional isolation the
evidence is the whole history of operations, so it is checked on the host after the run instead:
the workload prints `HYDO-HIST` records (one per invocation and completion), and the checkers of
the hydo-history crate (`hydo/crates/hydo-history`; linearizable register / CAS per key, Elle-style list-append) turn the
history into ordinary `HYDO-ASSERT` records of type `always` plus the HYDO-ORACLE verdict. Format,
models and recorders (shell, Go): `hydo/HISTORIES.md`; manifests use `checks:`.

## Host side

- `hydo/assert/hydo_assert_table.h`: header-only parser and aggregator (`ha_feed_console` once per run,
  `ha_print`, `ha_violations`), for harnesses and the explorer.
- `hv/hvx/hydo_assert_report`: `hydo_assert_report <console>...` prints the property table over the
  given saved consoles (e.g. `HVX_CONSOLE_OUT` dumps) and exits 1 if any property is violated.
  For `eventually` properties it also shows the slowest in-time episode and prints a
  `liveness: <n> eventually properties, <k> missed a deadline` line.
- The explorer (`hydo explore`) counts an `eventually` with `ok:false` as a failed assertion of the run (like a
  failed `always`), and `report.md` gets a "Liveness" table: per property, schedules where it
  held in time / missed its deadline, the slowest in-time episode and the first miss.
- Manifest invariants (`hydo/onboard/MANIFEST.md`) can be `type: eventually` with a deadline
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
