# TiKV: results (Milestone 287, tentative number)

TiKV is a distributed, transactional key-value database written in Rust. Its regions are Raft
groups, and the Placement Driver (PD) places them, moves their leaders, and hands out the
timestamps of every transaction (the TSO). This example runs three PD members and three TiKV
stores in one hvx guest, and it checks what TiKV promises its clients under hydo's faults. The
cluster is the newest release, v8.5.8 (2026-08-27). The code cited below is at the tag v8.5.8 of
tikv/tikv and tikv/pd.

## The short answers

- **One new candidate, a clock effect: after the wall clock steps back, the locks of a crashed
  transaction block every writer of their keys for the length of the step.** PD's timestamps keep
  their physical part while the wall clock is behind it, as TiDB's documentation of the TSO says.
  TiKV judges whether a lock's time to live has passed by that physical part
  (`check_txn_status.rs`). So the lock of a client that died inside its commit does not expire
  until the wall clock passes the time before the step. In the example, `disk=none | run
  T=20:-3600` leaves the accounts unwritable for the rest of the run, and so does a step of 5
  minutes. Without the step the same locks are resolved 2.18 s after the heal. The blind
  exploration reached it first (seed 4), where readers of the accounts timed out too (section
  "Finding 1"). No issue of tikv/tikv, tikv/pd, tikv/client-go or pingcap/tidb describes it.
  Nothing was posted.
- **One known bug, reached blind: a PD member that restarted shortly before a power cut never
  starts again.** PD's LevelDB of region metadata, an old goleveldb, points `CURRENT` at a new
  MANIFEST that it never synced. After the cut that member stops at every start with `leveldb:
  manifest corrupted`, and with two of three members gone PD loses its quorum. This is
  tikv/pd#11596, opened on 2026-11-16 and fixed on PD's master, not in v8.5.8 (section "Finding
  2").
- **What held.** Snapshot isolation of the list-append transactions in real-time order (also
  with async commit and one-phase commit), linearizable raw registers and compare-and-swap, the
  bank's total, and every acknowledged raw and transactional write. They held under kills and
  pauses of stores and PD members, a kill of all three PD members, partitions and one-way loss,
  whole-guest power cuts, the strict power loss of one store, corrupted files of one store, clock
  steps of up to 30 days either way, and PD's leader moves, splits and merges during the
  workload. Four explorations ran 60 blind schedules (section "The explorations").
- **Not bugs.** A damaged file of one store (RocksDB's `OPTIONS` file or write-ahead log, or
  TiKV's `last_tikv.toml`) stops that store at its next start, and the cluster runs on with two
  stores. One failure was a mistake of the example's
  readiness checks (section "The explorations").
- **What hydo lacked.** Three copies of one big image did not fit the engine's disk, TiKV's gRPC
  client did not reach PD by name, the services of a shared image tree shared one `/tmp`, a
  killed group's members are restarted one at a time with each readiness check in turn, and an
  explorer worker of a 7,680 MiB guest needs about 19 GB of the host. Section "What hydo lacked".

## What TiKV promises

TiKV's README (tikv/tikv, v8.5.8) states the promises under test:

- "TiKV not only provides classical key-value APIs, but also transactional APIs with ACID
  compliance."
- "With the implementation of the Raft consensus algorithm ... TiKV guarantees data
  consistency."
- "TiKV also provides snapshot isolation (SI), snapshot isolation with lock (SQL: `SELECT ...
  FOR UPDATE`), and externally consistent reads and writes in distributed transactions."
- "Similar to Google's Spanner, TiKV supports externally-consistent distributed transactions."

The configuration documentation (`api-version`) says that only the API version 2 "supports
co-existence of TiDB, Transactional KV, and RawKV applications in a single cluster", and that it
needs `storage.enable-ttl = true`. The example uses it, so that one cluster serves both APIs.

Percolator, the model of TiKV's transactions, resolves the locks of a client that dies inside a
commit by their time to live: once it has passed, the next reader or writer of the key rolls the
transaction back or commits it. TiDB's documentation of the TSO says that the physical part of a
timestamp "remains unchanged while the logical timestamp steadily advances" when the clock runs
backwards, so that timestamps never go back. The deployment guide asks for NTP on every node,
"to guarantee linear consistency of transactions".

The example states the promises as seven checks:

| check | promise | file |
|---|---|---|
| `la` | list-append transactions are snapshot-isolated in real-time order: hydo's list-append checker with `consistency=strong-snapshot-isolation`, which also finds lost, aborted and duplicated appends | `hydo/checks/list_append_la` |
| `reg` | raw reads, writes and atomic compare-and-swap of one key are linearizable: hydo's register checker with `model=cas` | `hydo/checks/linearizable_reg` |
| `bank` | transfers between eight accounts keep the total at 800, none below zero, read in one snapshot at the end | `hydo/checks/final_bank` |
| `bank_live` | every snapshot read while the transfers commit holds 800 in all (every 3 s) | `hydo/checks/always_bank_live` |
| `durable` | every acknowledged raw or transactional write of a new key reads back with its value, also after a power cut | `hydo/checks/final_durable` |
| `readable` | within 90 s after the heal, the lists and the accounts can be read in one snapshot | `hydo/checks/eventually_readable` |
| `writable` | within 90 s after the heal, one transaction can write every account (with its own value): the locks of crashed transactions are resolved after their time to live | `hydo/checks/eventually_writable` |

## The version

| part | version | image (linux/amd64) |
|---|---|---|
| TiKV | v8.5.8, commit `a1fdd6a5` | `pingcap/tikv@sha256:cdb681d570c490424d69ecc1ecda6865999f2b86ae3b2d2cb070989bf4d2701d` |
| PD | v8.5.8 | `pingcap/pd@sha256:507b401ece53aba15db9628a90a1e350c3cd1b9b39c086c708e2e88541b0e7fd` |
| client-go | `v2.0.8-0.20260803075849-c3b50791b9fb`, the version of TiDB v8.5.8 | (the client, `client/go.mod`) |

The multi-platform digests are `pingcap/tikv@sha256:ab84580b…` and `pingcap/pd@sha256:424e8968…`
(`hydo/hydo-compose.lock.json`).

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | three PD members and three stores under Docker, for runs without hydo |
| `conf/tikv.toml` | each store's configuration: API version 2, small caches, small regions |
| `conf/pd.toml` | each PD member's configuration: no dashboard, merges soon after a split |
| `client/` | `tkc`, the test client (Go, TiKV's client-go v2, static) |
| `build.sh` | builds `bin/tkc-amd64` and `bin/tkc-arm64` |
| `hydo/hydo.yaml` | the six services, the groups `pds` and `stores`, the workload's sizes |
| `hydo/workload/`, `hydo/checks/` | the client's commands and the seven checks |
| `hydo/regressions/` | the saved schedules of the findings |

`conf/tikv.toml` changes only sizes and thread counts. Every setting of durability and
consistency keeps its default: Raft Engine for the Raft logs, the default leader lease, and the
default commit paths. Regions split at 1 MiB, and PD merges regions below 1 MiB 10 s after a
split, so that splits, merges and leader moves happen within a run of a minute.

`hydo compose` imported the Compose file. Four changes made the first clean run pass, each of
them a gap of hydo or of the import (section "What hydo lacked"):

1. `root.share_images: true`. The TiKV image holds `tikv-server` (518 MB, with debug
   information) and `tikv-ctl` (328 MB). Three copies of the unpacked image made a disk of
   8,836 MiB, and an engine maps 4,096 MiB. With one shared tree per image the stores share one
   copy.
2. TiKV and PD name each other by address, not by service name (`--pd`, `--advertise-addr`,
   PD's `--advertise-client-urls`). With names, TiKV's gRPC client did not reach PD: every call
   ended in `DEADLINE_EXCEEDED` for three minutes, while PD answered HTTP by name.
3. Each store listens on its own address (`--addr=10.89.0.14:20160`, not `0.0.0.0:20160`). TiKV
   locks a file named after its listen address in `/tmp`, and the stores of one shared image tree
   found the same file: `0.0.0.0_20160 already in use, maybe another instance is binding with
   this address`.
4. 7,680 MiB of guest RAM. Each store held about 2 GB at its start, also with a 64 MB block
   cache and `memory-usage-limit = "1GB"`. At 5,120 MiB the guest's out-of-memory killer ended
   one store 42 s after the boot.

## The workload

The client `tkc` makes one operation per command. The composer runs up to 4 at once, 80 rounds
of 500 ms. Every operation dials PD anew, so a kill or a partition meets fresh clients.

| command | what it does | what it records |
|---|---|---|
| `parallel_driver_la` | one transaction of 1 to 4 reads and appends on `la/k0..k7`; an append reads the list and writes it back one longer. One transaction in three uses async commit and one-phase commit, the commit paths that TiDB turns on by default | history `la` |
| `parallel_driver_reg` | a raw read, write or compare-and-swap (atomic mode) of `reg/k0..k4` | history `reg` |
| `parallel_driver_bank` | a transfer of 1 to 30 between two of `acct/0..7` in one transaction, refused below zero | the outcome |
| `parallel_driver_put` | a new key of 2 to 8 KiB, raw (`r/<id>`) or transactional (`t/<id>`), so that regions grow and split | `$HYDO_ACKED` |
| `anytime_move` | asks PD's operator API to move a region's leader, split a region, or merge two neighbours | nothing |
| `anytime_crash` | a client that crashes inside its commit: it writes the locks of a transfer of 1 between two accounts (Percolator's prewrite, with client-go's default time to live of 3 s) and exits before the commit | an unknown outcome |

`anytime_crash` uses client-go's exported test helpers (`transaction.TxnProbe`, `NewCommitter`,
`PrewriteAllMutations`), because hydo kills services, not the client at a point of its code. It
came into the workload after the first exploration (images `base6` and `base7` below), to make
the crash that seed 4 of that exploration needed happen in every run.

An operation's outcome follows client-go's errors. A commit without an error is acknowledged.
A transaction that failed before its commit, or whose commit returned a write conflict, wrote
nothing. Every other error leaves the outcome unknown (`info` in the history), because a commit
can be applied after the client gave up. A raw write with an error is unknown, and a
compare-and-swap that answers "not swapped" failed.

## Without faults, and the regressions

Every run used hvx on the bare-metal machine, one VM at a time through `~/bin/hvx-run`, with
this branch's `hydo`, `hydo-build` and `hydo-hvx` (built at `0038380d`) over the 0.3.4 Linux
release, at the fingerprint epoch 4. The client was built with Go 1.27.3. The images, in order:

| image | disk sha256 | what changed | clean run `disk=none \| run` |
|---|---|---|---|
| `base` | `762acf87…` | the import with readiness checks and storage | `ERROR service start (tikv1)`, twice `fp=236153adf56062f8`: TiKV did not reach PD by name |
| `base2` | `efb9369d…` | addresses instead of names | `ERROR service start (tikv1)`: `0.0.0.0_20160 already in use` |
| `base3` | `1e085856…` | each store on its own address | `ERROR provision failed`: the out-of-memory killer ended a store at 5,120 MiB |
| `base4` | `fda4c406…` | 7,680 MiB, a 64 MB block cache | `OK`, twice `fp=3ce4e931b4ce6ce9` (42.7 s); the first exploration |
| `base5` | `d45087b6…` | PD ready at once, `eventually_readable`, `always_bank_live` exits 0 when it cannot read | `OK` |
| `base6` | `d770c11a…` | `anytime_crash` | `OK`, twice |
| `base7` | `2cfcea06…` | `eventually_writable` | `OK`, twice `fp=b6f157e6448f0008`; `writable` held 2.18 s after the heal |
| `base8` | `c5d10060…` | the committed `hydo.yaml` (its header comment) | `OK`, `fp=9c236dcfc429f5cf`; the third exploration |

The committed `hydo/hydo.yaml` differs from base7's only in its header comment. `hydo build`
copies that file into the image, so the committed folder gives an image with other bytes and
other fingerprints. The image of the committed folder, base8 (`c5d10060…`), gave `OK` for
`disk=none | run` (`fp=9c236dcfc429f5cf`, `writable` held 2.18 s after the heal).

Two regressions save the findings, each with `# expect: violation`:

- `hydo/regressions/crashed-txn-locks-after-clock-step.schedule`: finding 1, `disk=none | run
  T=20:-3600`.
- `hydo/regressions/known-pd-11596-region-storage-after-power-cut.schedule`: finding 2,
  `disk=cut | run K=pds.all@40+1 C=46`.

The committed folder built again with both files (base9) gave the same image bytes as base8,
because the regressions stay beside the image since the fingerprint epoch 4. Two clean runs gave
`fp=9c236dcfc429f5cf`, and `hydo repro --regressions` reported `2 of 2 regressions as expected`
(`fp=718f926794984e13` and `fp=41525298e7723d4d`).

## What was found

### Finding 1 (a clock effect, new as far as the search found): after the wall clock steps back, the locks of a crashed transaction block every writer of their keys for the length of the step

**How it showed.** Seed 4 of the first exploration, on image base4, ended in `ERROR invariant
bank_live could not be evaluated: the store did not answer: [10.89.0.16:20160](1) context
deadline exceeded`. Its schedule was `disk=none dseed=5d40003bdb5e0071 | run K=pd1@53+27
P=pd1@18+1 T=42:-2592000 W=ddb740e7,p1,q1,r250 G=tikv1@67:trunc,seed=7c0ecfff L=tikv1@33+1`: a
clock step of 30 days back before step 42 among other faults. `hydo run` of that schedule on
base4 gave the same error (`fp=b0eb3e40c859476d`), and the same schedule without `T=` passed.
From step 70 on, every bank transfer failed after the client's 12 s limit, while list appends
and raw operations went on. The stores' logs showed the cause's shape. tikv3 asked for the
status of one transaction every 2 s for minutes (`check_txn_status-470515353185431291-…`). Its
lock's start timestamp and the reader's current timestamp were 1,073,167 apart, 4.09 ms of
physical time, after minutes of guest time.

**The directed schedules.** The workload's `anytime_crash` makes the crash that seed 4 needed
happen in every run. On image base7:

| schedule | verdict | `fp=` |
|---|---|---|
| `disk=none \| run` (twice) | `OK`; `writable` held 2.18 s after the heal | `b6f157e6448f0008` |
| `disk=none \| run T=20:-3600` (twice) | `VIOLATION [writable] not satisfied 97.03s after heal, deadline 90s (last check rc=1: the accounts cannot be written: [10.89.0.14:20160](1) context deadline exceeded)` | `5c55bec66f806d2d` |
| `disk=none \| run T=20:-300` (twice) | the same violation | `5e36930d71d80471` |
| `disk=none \| run T=20:-150` | the same violation | `ea5feca12068144a` |
| `disk=none \| run T=20:-60` | `OK`; `writable` held 17.06 s after the heal | `8876ec667198dfce` |

The length of the outage follows the step. The step comes 10 s into the workload, and the heal
comes about 40 s later. A step of 60 s left the accounts unwritable until 17 s after the heal,
when the wall clock had passed the time before the step. Steps of 150 s and more outlast the
check's 90 s.

A look inside the first `T=20:-3600` run at its end (`hydo inspect exec`) showed the three
parts of the cause at work:

- `tkc check writable` failed with `the accounts cannot be written: [10.89.0.14:20160](1)
  context deadline exceeded`.
- PD's leader logged `the logical time may be not enough` about every 44 s of guest time, each
  time with about 131,100 logical values used. Each such log line is the 1 ms step of the
  physical part.
- A store was asked for the status of the lock of transaction `470515353303748261` every 2 to
  3 s. The current timestamps of those requests (`470515353304482167` and later) were 733,906
  apart from the lock's, 2.8 ms of physical time, after minutes of guest time. The lock's time
  to live is 3,000 ms.

On image base6 (before `eventually_writable`), `T=20:-3600` and `T=20:-300` raised the failed
commands from 22 to 32 of 154, and `T=20:30` (a step forward) left them at 21. The reads did not
block there: a reader pushes a live lock's `min_commit_ts` past its own start and reads on. In
seed 4 the readers of the accounts timed out too; why that run's reads could not get past the
lock was not traced.

**The cause.** Three parts, at the tag v8.5.8:

1. PD's `UpdateTimestamp` in `pkg/tso/tso.go` keeps the physical part of the TSO while the wall
   clock is behind it. It moves the physical part to the wall clock only when the clock is ahead
   of it (`jetLag > UpdateTimestampGuard`). Otherwise it adds 1 ms only after half the logical
   counter is used, 131,072 timestamps, and returns without a change. This is the documented
   behaviour ("the physical timestamp remains unchanged while the logical timestamp steadily
   advances").
2. TiKV decides whether a lock has expired by the physical parts of two timestamps:
   `lock.ts.physical() + lock.ttl < current_ts.physical()` in
   `src/storage/txn/actions/check_txn_status.rs` (lines 55 and 176).
3. A writer that meets the lock asks for the primary's status with a current timestamp from PD.
   With the physical part standing still, the lock's 3,000 ms never pass, so TiKV neither rolls
   the transaction back nor lets the writer through. The writer backs off until its own deadline.

So after a step back of the PD leader's wall clock, every lock of a client that crashed inside
its commit (or of a commit whose client gave up after its prewrite) stays until the wall clock
passes the physical time PD had reached before the step. That lasts as long as the step. The
other paths that measure time by TSO physical parts (pessimistic locks and their waits, async
commit's `min_commit_ts`) were not tried.

A step back of the PD leader's clock happens when NTP corrects a clock that ran ahead, and the
deployment guide's NTP does that by a step when the offset is large. A move of PD's leadership
from a member whose clock ran ahead to one whose clock is right should have the same effect,
because the new leader starts from the saved timestamp window; that was not run.

**The label.** A clock effect: the TSO's frozen physical part is documented, and nothing is lost.
It breaks a promise of the transaction model, that a crashed client's locks are resolved after
their time to live, for as long as the step lasts. Valkey's and Temporal's clock effects were
reported for the same reason.

**The duplicate search** found no report of it (section "The duplicate searches").

**No native run.** The cause is in three places of code and a documented behaviour, so the
playbook's rule skips a reproduction with Docker.

**The regression** is `hydo/regressions/crashed-txn-locks-after-clock-step.schedule`.

### Finding 2 (a known bug, tikv/pd#11596): a PD member that restarted shortly before a power cut never starts again

**How it showed.** Seed 12 of the second exploration, on image base7, ended in `VIOLATION
service cannot recover (committed data unrecoverable) (tikv1)`. Its schedule was
`disk=cut dseed=72b4b958fb4479ee cf=1186 | run K=pds.primary@57+2 N=stores.ring@40-43:loss,100%
T=63:-30 W=86f364de,p4,q1,r250`: a partition of the stores, a kill of PD's leader, and a
whole-guest power cut at the 1,186th disk flush. `hydo repro --transcript` of the explorer's run
gave the same verdict and fingerprint (`3436eacd1e6a154a`). The shrinker kept the kill of PD's
leader, the partition and the cut, and dropped the clock step.

A look inside the replay at its end showed that the store was not the cause. In the recovery
boot pd3 stopped at once with `[FATAL] ["run server failed"] [error="[PD:leveldb:ErrLevelDBOpen]
leveldb: manifest corrupted (field 'comparer'): missing [file=MANIFEST-000004]"]`, and pd1 stopped
10 s later with `run server failed ... context deadline exceeded`. With one PD member left, tikv1
could not reach PD (`failed to connect to all addresses`), so its readiness check never passed.
PD's members count as ready at once in this example (section "What hydo lacked"), so hydo named
the store.

**The directed schedules.** On image base8, `disk=cut | run K=pds.all@40+1 C=46` (all three PD
members restart, and the guest loses its power 2.5 s of guest time later) gave the same
violation (`fp=41525298e7723d4d`). A kill of one member (`K=pd3@40+1 C=46`) passed: two members
were left, and so did the cut alone (`disk=cut | run C=46`). At the end of the violating run pd1
and pd2 had stopped with the same `manifest corrupted (field 'comparer'): missing
[file=MANIFEST-000004]`, and pd3 10 s later with `context deadline exceeded`. A second run gave
the same fingerprint.

**The cause**, at the tag v8.5.8:

1. PD keeps region metadata in a LevelDB (`pkg/storage/kv/levedb_kv.go`, `leveldb.OpenFile(path,
   nil)`), and `go.mod` pins `github.com/syndtr/goleveldb v1.0.1-0.20190318030020-c3a204f8e965`.
2. Every open writes a new MANIFEST (`newManifest` in `leveldb/session_util.go`). That version
   only flushes the MANIFEST to the operating system (`jw.Flush()`), then writes and syncs
   `CURRENT`, which names it (`SetMeta`).
3. A power cut before the kernel writes the MANIFEST back leaves `CURRENT` pointing at an empty
   MANIFEST, and every later open fails.

goleveldb fixed it in 2021 (commit `eb135432c5`, "Add fsync after creating new manifest").

**The label.** A known bug: tikv/pd#11596 ("pd-server never starts again after a power loss
within about 30 s of starting: `leveldb: manifest corrupted`"), opened on 2026-11-16 and closed by
pull request #11598, which bumps goleveldb on PD's master. The backports #11600, #11601 and #11602
target the `release-nextgen` branches, and none targets `release-8.5`. So v8.5.8, the newest
release, has it. The exploration reached it blind.

**The regression** is `hydo/regressions/known-pd-11596-region-storage-after-power-cut.schedule`.

## The duplicate searches

Issues and pull requests, open and closed, with `gh search issues --include-prs`, on
2026-11-17. A phrase that found something is listed with what it found.

| repository | phrases | what they found |
|---|---|---|
| tikv/tikv | `clock jump`, `clock back`, `system time back`, `time fallback`, `clock skew`, `clock drift` | #9392 (a panic when the system time went back, fixed in 2021), #17995 (QPS falls to zero with one store's clock 5 minutes behind), #18055 (panics with the PD leader's clock 5 minutes ahead), RocksDB's clock-skew fixes (#18102), the lease read's monotonic clock (#964, #1127): none about lock expiry |
| tikv/tikv | `lock ttl`, `lock ttl clock`, `lock never expire`, `lock is not expired`, `ttl expire tso`, `check_txn_status`, `txnLockFast`, `resolve lock timeout`, `lock wait timeout clock` | #5471 (check a lock's TTL in cleanup, 2019), #16158 (lock waits and expired pessimistic locks), #12754 (raw backup's "resolve lock timeout"): none about a TSO that stands still |
| tikv/tikv | `TSO fallback`, `physical time`, `max-drift`, `invalid max_ts update`, `action-on-invalid-update` | #19756 and #17917 (the max_ts limit), #20136 (resolved-ts stops after a rejected max_ts update), #18057 |
| tikv/tikv | `monotonic time jumped back` | #16623, #14518 (panics, closed) |
| tikv/pd (finding 2) | `goleveldb`, `manifest corrupted`, `ErrLevelDBOpen`, `region storage corrupted`, `leveldb power` | #11596 (the same bug, closed on 2026-11-16), #11598 (the fix on master), #11600 to #11602 (backports to the `release-nextgen` branches), #4195 (replace goleveldb) |
| tikv/pd | `clock`, `clock backward tso`, `system time fallback`, `time fallback`, `time goes back`, `jet lag`, `NTP`, `UpdateTimestamp`, `jetLag`, `logical time may be not enough`, `tso physical time does not advance` | #8806 (the `clock offset` log message), #10137 (an update when the logical part is used up), #1186 and #1173 (tests of a time fallback, 2018), #11356 (a panic on a clock rollback in the heartbeat tracer): none about locks |
| tikv/client-go | `clock`, `lock ttl clock`, `tso physical`, `resolve lock expired` | nothing related |
| pingcap/tidb | `clock back lock`, `system clock rollback`, `time goes back tso`, `lock not expired clock`, `NTP clock lock`, `pd clock backward` | nothing |

The documentation was read too: TiDB's page on the TSO (`tso.md`), the check before deployment
(`check-before-deployment.md`, the NTP section), and TiKV's configuration file page
(`storage.max-ts`).

## The explorations

All four explorations used `--strategy process --new-faults --more-faults --cluster-faults
--storage-faults` on one worker. The fourth explored without clock steps, through a copy of the
manifest beside `hydo.yaml`:

```sh
sed '/^faults:/s/, clock//' hydo.yaml > explore-noclock.yaml
hydo explore --manifest explore-noclock.yaml --seeds 41-84 --strategy process --new-faults \
  --more-faults --cluster-faults --storage-faults
```

No swarm exploration ran. The phase reached its limit of 5 hours after the fourth exploration,
because the directed runs of the two findings took the machine's one VM for about 2 hours. The draws included kills and pauses of single services and of
the groups' selectors (`K=pds.all`, `K=stores.one.3`, `P=pds.minority.1`), partitions and one-way
loss among PD and the stores, whole-guest power cuts (`C=`, `cf=`), strict and plain power losses
of one store (`L=`), corrupted files of one store (`G=`: zeroed, flipped, truncated), and clock
steps from 30 s to 30 days either way.

| exploration | image | seeds | schedules run | failures by cause |
|---|---|---|---|---|
| ex1 | base4 | 1-10 of 1-24 (stopped for the directed runs) | 10, and the confirmation of seed 4 | seed 4: finding 1 (`ERROR invariant bank_live could not be evaluated`); seed 6: the example's PD readiness after `K=pds.all` (`ERROR pd1 restart failed`), fixed in base5; seed 9: a flipped bit in tikv2's RocksDB `OPTIONS` file stops tikv2 at its restart (`ERROR tikv2 restart failed (its files were corrupted)`), by design |
| ex2 | base7 | 11-13 of 11-40 (its 40-minute limit ended it during the shrinks) | 3, and 33 shrink runs | seed 12: finding 2 (`VIOLATION service cannot recover`), shrunk to `K=pds.primary@57+1 N=stores.isolate@40-41:loss,100%` with the cut; seed 13: finding 1 (`VIOLATION [writable]`), shrunk to `T=11:-3600` and a truncated file that does not matter |
| ex3 | base8 | 41-43 of 41-80 (stopped: a backward clock step reaches finding 1 in most schedules, and its shrink takes the machine) | 3, and 33 shrink runs | seed 42: finding 1 (`T=31:-300` among five faults) |
| ex4 | base8 without clock steps (`explore-noclock.yaml`, a copy of `hydo.yaml` whose `faults:` leaves out `clock`) | 41-84 (its 80-minute limit) | 44 schedules, 9 confirmations and 23 shrink runs, 33.7 runs an hour | eight stores that did not start after `G=` damaged a file (seeds 42, 44, 45, 47, 63, 66, 67, 82), by design; seed 83: finding 2, shrunk to one restart of pd1 before the cut at the 1,125th flush |

Seeds 1, 2, 3, 5, 7, 8 and 10 of ex1 passed, among them a power cut at step 80 with a strict
power loss of tikv1 (seed 1), a kill of all three stores and a power cut at the 932nd flush
(seed 5), and clock steps of 30 days back (seeds 5 and 7) without a crashed transaction.

### Not bugs: a store with a damaged file does not start again

`G=` flips bits in, zeroes or cuts one file of one store, and a store whose file was damaged
before a restart did not start again in nine schedules (ex1 seed 9, ex4 seeds 42, 44, 45, 47,
63, 66, 67 and 82). The files were RocksDB's `OPTIONS` file (seeds 9, 42 and 66), RocksDB's
write-ahead log `db/000004.log` (seeds 47 and 63), a cut SST file (seed 67), RocksDB's zeroed
`MANIFEST` (seed 82), and `last_tikv.toml` (seeds 44 and 45), TiKV's copy of the configuration it
last ran with. The other two stores kept every region available, and no history
broke. A store that refuses to start on a damaged file serves nothing wrong, so the example
counts these as by design.

One of them is close to a gap. `get_last_config` in `src/config/mod.rs` panics with `invalid auto
generated configuration file` when `last_tikv.toml` does not parse, although a missing file is
fine and TiKV writes the file again from its current configuration (`persist_config`). So a
damaged copy of the configuration, which holds no data, stops the store at every start until an
operator deletes it. It breaks no documented promise and was not judged a candidate.

## What hydo lacked

In the order of the time each cost:

1. **Memory per explorer worker.** One worker of a 7,680 MiB guest used about 19 GB of the
   bare-metal machine, so two more workers failed to make their VMs (`ERROR harness boot vm`), and
   a `hydo run` beside the explorer ended at once with `the engine exited`, without a reason.
   Every exploration and every directed run went one at a time. Cost: about 1 hour of machine
   time, and four directed runs lost before the reason was clear.
2. **A killed group restarts one member at a time.** After `K=pds.all`, hydo restarted pd1 and
   waited for its readiness check, which needs a PD quorum, before it started pd2 and pd3: `ERROR
   pd1 restart failed` (seed 6). PD's members now count as ready at once, and the client waits
   for the cluster. Cost: 20 minutes.
3. **Three copies of one image.** Each image service gets its own unpacked tree by default, and
   three TiKV trees of about 1 GB each made an 8,836 MiB disk. `hydo build`'s message says to make
   the sizes smaller; `root.share_images: true` was the fix, and the import does not propose it.
   Cost: 10 minutes.
4. **The services of a shared image tree saw one `/tmp`.** MANIFEST.md says that each gets its
   own `/tmp`, but TiKV's address lock (`$TMPDIR/<lock dir>/0.0.0.0_20160`) was found by the
   second store. Each store now listens on its own address. Cost: 5 minutes.
5. **TiKV's gRPC client did not reach PD by name.** PD answered HTTP by name in the guest, but
   TiKV's calls to `pd1:2379` ended in `DEADLINE_EXCEEDED` for three minutes. With addresses it
   worked at once. The cause (the resolver of gRPC's C core in a guest without DNS) was not
   traced. Cost: 5 minutes.
6. **No look at a run beside the explorer.** `hydo inspect exec` needs its own VM, so the evidence
   of a failure waits until the explorer ends. One `inspect exec` with a `wget` to a store that
   did not answer ran past 20 minutes.
7. **The clock audit missed the TSO.** `hydo rules scan --kind clock` ranked Azure and GCP token
   code, a test, and PD's scheduler pauses first. It does not see a timestamp that is compared
   with a time to live (`lock.ts.physical() + lock.ttl`), which is the place of finding 1.
8. **No way to crash the client inside an operation.** hydo kills services, not the workload at
   a point of its code, so the crash inside a commit needed client-go's test helpers in the
   client.

## Time

| phase | wall time | of it waiting for machines |
|---|---|---|
| 1. Scouting | 12 minutes (limit 45 minutes); hydo built on the bare-metal machine meanwhile | none |
| 2. Setup to a first clean run | 30 minutes (limit 3 hours) | about 20 minutes of image builds and runs |
| 3 and 4. Exploration, directed runs, judging | 4.9 hours (limit 4 to 5 hours); stopped at the limit without the swarm exploration | most of it |
| 5. Duplicate search and write-up | 1.5 hours, most of it beside the explorations | none |
