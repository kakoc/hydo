# SurrealDB under hydo: the storage engine and the transactions

SurrealDB (github.com/surrealdb/surrealdb) is a multi-model database with its own query language,
SurrealQL. Release 4.0 removed RocksDB, so a server keeps its data in SurrealKV
(github.com/surrealdb/surrealkv). SurrealKV is a log-structured merge tree with a write-ahead log
(the WAL) and a value log for large values. This example puts one SurrealDB server under hydo. It
hunts for bugs that nobody reported in the storage engine, the transactions, the live queries,
and the changefeeds.

## The short answers

- **What was found?** One new bug, in SurrealKV, and two known SurrealDB bugs.
  1. **New: one damaged value of the value log makes the next start drop every later commit.**
     SurrealKV keeps a value over 4 KiB in its value log and a pointer to it in the WAL. When one
     such value is damaged on the disk and no later commit of the WAL segment has a large value,
     the next open ends the WAL before that commit. Every later commit is dropped, also the small
     ones, which do not depend on the damaged bytes, and SurrealDB starts with a warning only.
     Those commits were acknowledged and synced (`sync=every`), and SurrealKV's own sync marks
     say so. The blind explorations found it 7 times: in 3 of 40 seeds on hvx and in 4 of 60 on
     the Mac. Docker reproduces it without hydo (`native/vlog-tail-cut.sh`). Reported as
     surrealdb/surrealkv#536 (2026-11-17).
  2. **Known (surrealdb#7606): changefeed versionstamps go back after a backward clock step and a
     restart.** A changefeed reader that resumes with `SHOW CHANGES ... SINCE` then skips the
     new changes.
  3. **Known (pull request surrealdb#7672, open): a write to a table with a changefeed or an
     `ASYNC` event waits for the length of a backward clock step.** A step of one day back ended
     the Mac runs with `exit budget exhausted`.
- **What held?** No acknowledged commit was lost to these faults:
  - a kill, a pause, or a power loss of the service;
  - a power cut, also at the k-th disk flush on hvx;
  - disk I/O errors.

  The list-append transactions kept snapshot isolation, and the registers stayed linearizable.
  The bank total stayed 800, and the indexes and the graph agreed with their tables. Every
  acknowledged event was marked by its `ASYNC` event. Every live query reported its own client's
  commits in order. Damaged files that SurrealKV detects stop the server with an error. The
  experimental upgrade of a 3.3.5 RocksDB directory to 4.0.1 held too, also with power cuts
  inside the 4.0.1 open (section "The upgrade from 3.x").
- **What did hydo lack?** Section "What hydo lacked": six gaps, none fixed here.

## The version

SurrealDB 4.0.1 (2026-11-09) was the newest release when the onboarding started (2026-11-16).
It ran from SurrealDB's image `surrealdb/surrealdb:v4.0.1`, which holds SurrealKV 1.2.0. The
image's index digest is `sha256:408eb8e5…`; amd64 is `sha256:d9501f6d…`, arm64
`sha256:3a9db34b…`. SurrealKV's `main` was read at its release 1.4.0 (2026-11-11); its recovery
has the same rule as 1.2.0 (section "The cause").

The community build of 4.0 has two storage engines: memory and SurrealKV. TiKV moved to a
separate repository (`surrealdb-tikv`), so there is no store that several nodes share. A cluster
of SurrealDB nodes therefore needs TiKV, and this example runs one node only.

## The promises

Each check of the next section states one of these, quoted from SurrealDB's documentation:

- Durability, with the default `sync=every` (docs/running/file-backed): "a commit is synced to
  disk before it returns".
- Isolation (docs/transactions-and-isolation): "Every SurrealDB transaction runs under snapshot
  isolation." "Concurrent writes to the same key cannot both commit; one transaction must retry."
- Atomicity: "A transaction always commits or cancels as a whole." A commit whose outcome is not
  known is reported as such; every other error means nothing was applied
  (docs/surrealql/transactions).
- Changefeeds (the `SHOW` statement): a changefeed is "a log of the table's changes in
  versionstamp order." "Versionstamps monotonically increase."
- `ASYNC` events (`DEFINE EVENT`): "The event is enqueued within the same transaction as the
  document change." "If the transaction fails, the event is never queued." "An event runs at
  least once."
- Live queries (docs/learn/querying/real-time/live-queries): "Notifications reflect committed
  work." "The notifications for the commits of one client arrive in the order of those commits."

## The project

The folder was started as a new user starts one: a Compose file with one service
(`docker-compose.yaml`), then `hydo onboard`. The import's service is kept, with three changes:

- the client in the guest reaches the server on `127.0.0.1`, so the bridge network is gone;
- the readiness check is a query of the client, `sdbc ready`;
- SurrealKV's directory is a file system of its own (`storage:`), so that `G=` corrupts its files
  and `L=` cuts its power alone.

The client `sdbc` (`client/`, Go, built by `build.sh`) sends SurrealQL to the HTTP endpoint
`/sql` and, for live queries, to the WebSocket endpoint `/rpc`. An answer whose statements all
say `OK` is acknowledged. No answer is unknown, and so is an error that says "The transaction's
commit outcome is unknown." Every other error is failed. The client records each operation in
`$HYDO_OUTCOMES`, and the history checks' operations as hydo's history records.

| driver | what one command does | checks |
|---|---|---|
| `parallel_driver_la` | one transaction of 1 to 4 appends and reads on eight lists, as one request | `list_append_la` at `strong-snapshot-isolation` |
| `parallel_driver_reg` | a read, a write or a compare-and-set of one of five registers | `linearizable_reg` (`model=cas`) |
| `parallel_driver_bank` | a transfer between two of eight accounts, refused below zero | `always_bank_live`, `final_bank` |
| `parallel_driver_blob` | a record of 1 byte to 20 KiB, half of them above SurrealKV's 4 KiB value-log threshold | `final_blobs`, `final_changefeed` |
| `parallel_driver_ev` | a record whose `ASYNC` event writes a mark | `eventually_marks` (within 60 s of the heal) |
| `parallel_driver_rel` | an edge between two persons, or the deletion of an earlier edge | `final_graph` |
| `parallel_driver_user` | a user with an address of a pool of 24 (a `UNIQUE` index), or a change of an earlier user | `final_index` |
| `parallel_driver_live` | a `LIVE SELECT`, six creations, and their notifications | the command itself (exit 100) |

`final_blobs` also checks that no record whose creation failed exists. `final_changefeed` checks
that the blob table's changefeed has each record of the table once, and that a creation
acknowledged before another was sent has the lower versionstamp. `final_graph` compares
`->follows->` and `<-follows<-` with the edge table. `final_index` compares each lookup through
an index with a scan `WITH NOINDEX`.

`faults:` lists every fault kind, `clock` too. A backward clock step reaches the two known bugs
(findings 2 and 3) through the changefeeds and the `ASYNC` event, which both take a stamp from a
clock that stalls after such a step. Two ways keep them out of a blind exploration:

- the build parameter `feeds=no`, which leaves the changefeeds and the event out of the tables;
- a copy of `hydo.yaml` without `clock` in `faults:`, next to it, which the explorer reads with
  `--manifest` (the comment at the top of `hydo.yaml` has the `sed` line).

The explorations of hvx and of the Mac's final image used the copy (`explore-noclock.yaml`), with
the image built with `feeds=yes`. The clock exploration used `feeds=no` and the clock steps.

## Without faults

| engine | image | schedule | result |
|---|---|---|---|
| Mac | first workload (4 drivers) | `disk=none \| run`, twice | OK both times, `fp=199b0a3a6d37e1b0`, 58 commands |
| hvx | 7 drivers | `disk=none \| run`, twice | OK both times, `fp=f1fe08f3aeae4a0b`, 220 commands |
| Mac | 8 drivers, live queries | `disk=none \| run`, twice | OK both times, `fp=6f50160f9598e665`, 220 commands |
| Mac | the committed folder | `disk=none \| run`, twice | OK both times, `fp=f8764c54434a8760`, 220 commands |
| hvx | the committed folder | `disk=none \| run`, twice | OK both times, `fp=0873ff5709de1091`, 220 commands |

Of the 220 commands, 16 or 17 end `failed` without faults: compare-and-set operations whose
value did not match, transfers refused below zero, and users whose address was taken.

The first clean run passed 18 minutes after the start of the onboarding.

## What was found

A finding is a bug only when a documented promise breaks or its cause is in the code. A finding
is new only when the searches of the issues, the pull requests, and the discussions of SurrealDB
and SurrealKV found no report of it.

### 1. One damaged value of the value log makes the next start drop the commits after it (new)

**What happens.** SurrealKV writes a value over 4 KiB to its value log and logs a pointer to it in
the WAL. At an open, it checks every pointer of the WAL's last segment against the value log
(`wal/tail.rs`). When a pointed-to value fails its check, and no later record of the segment
points to a value that passes, the open ends the segment before that record. That record and
every record after it are dropped. The server starts, and SurrealDB logs a warning, here in the
run of `native/vlog-tail-cut.sh`:

```text
WARN surrealkv::lsm: WAL segment 1 ends before record 76, at offset 8992: the value it points to is not in the value log. 12 record(s) and 33578208 byte(s) were left out, and the segment as it was is kept as "/data/db/wal/00000000000000000001.wal.corrupt".
```

The dropped records are commits that SurrealDB acknowledged after an fsync (`sync=every`). Most of
them are small records, which do not use the value log at all.

**How it was found.** The blind exploration on hvx (40 seeds, section "The explorations")
corrupted SurrealKV's files with `G=` in 22 seeds. Three of them changed the value log, and all
three lost acknowledged records:

| seed | the corruption | lost |
|---|---|---|
| 3 | 512 bytes zeroed in `vlog/00000000000000000001.vlog` while the server was down | 3 events, 1 blob, 3 edges |
| 10 | the value log cut to 127,640 bytes while the server was down | 3 events, 1 blob, 2 edges |
| 32 | one bit flipped in the value log of the running server, then a power loss of the service | 1 event, 2 blobs |

The Mac sweep (60 seeds) changed the value log in 4 of its 29 `G=` seeds: 111, 121, 150, and
156. The changes were zeroed bytes or flipped bits, and each of the 4 lost acknowledged records. Every other
`G=` hit the WAL's unused space, the lock file, or a file that SurrealKV checks at the open.

The explorer shrank hvx seed 3 to the corruption alone, without its kill and its pause:

```text
disk=none dseed=1717ad91294d20d2 | run W=fc94c8ea,p2,q2,r600 G=surrealdb@38+1:zero,len=512,seed=951ce4e8
```

It failed on both hvx runs on its image (`fp=aa638ed17df30148`): 3 of 17 acknowledged events, 2 of
20 blobs and 5 edges were lost. Every other check held, and no error line was logged. A shrink
candidate with a blob command after the damaged value ended with `ERROR surrealdb restart failed
(its files were corrupted)`: a later large value makes the open refuse, as SurrealKV's rule says.

`G=` chooses the place from the files of the moment, so the schedule depends on the image. On the
final image the same schedule damages a value that a later large value follows, and the open
refuses. The saved regression `regressions/vlog-tail-lost-drops-later-commits.schedule` cuts the
value log's tail instead, which removes the newest values on any image:

```text
disk=none | run G=surrealdb@40+1:trunc,file=vlog
```

It failed on the image of the explorations on hvx (`fp=6b6e85bf41bea7d4`, twice) and on the Mac
engine (`fp=8f77af7e6cb5a8b7`, twice): 13 of 28 acknowledged events were lost. On the committed
folder's image it fails on the Mac (`fp=63e1ed905f64c9b8`, 13 of 28 events lost) and on hvx
(`fp=7d5ebcc3b9ac3b16`, twice).

**Without hydo.** `native/vlog-tail-cut.sh` runs SurrealDB's image in Docker. It creates 30
records with a 9,000-byte value, each followed by a small record, and 10 more small records. It
kills the server, zeroes 512 bytes inside the newest value-log entry, and starts it again:

```text
acknowledged: big 30, small 40
read back:    big 30, small 40
zeroed 512 bytes at 267801 of /data/db/vlog/00000000000000000001.vlog (271801 bytes)
after the restart: big 29, small 29
missing small records: 110 101 102 103 104 30 105 106 107 108 109
```

Eleven small records, which share no byte with the damaged value, are gone. With `LATER_BIG=1`
the script creates one more large record after the small ones and damages the same value. Then
the open refuses, and its error names the sync mark that covers the record:

```text
ERROR surrealdb_server::cli: There was a problem with the datastore: Other error: WAL segment 00000000000000000001.wal at offset 8992: the value that record 76 points to is not in the value log, a record after it points to one that is, and a sync mark says that the segment was synced past record 76. The log is not cut automatically: configure WalRecoveryMode::SalvageValidPrefix to end it at that record.
```

The damage, the record and the sync marks are the same in both runs. Only the size of the last
record differs.

**Is it a bug?** The damage itself is outside SurrealDB's control: a disk lost bytes it had
synced. What the open does with it is SurrealKV's choice, and two of its own rules disagree:

- SurrealKV's README says of a record whose value is not in the value log: "A record whose value
  is not there, and the records after it, are not replayed." The default mode refuses to open
  only when a later record has its value.
- The design of its sync marks (pull requests surrealkv#499 and #504) refuses damage that a
  later mark says was synced, instead of cutting it. Only a disk that loses synced bytes can
  leave such damage. In this case the later commits were each synced, and a mark claims the damaged
  record: the run with `LATER_BIG=1` shows that mark in its error.

So the outcome depends on whether a later commit has a value over 4 KiB. With one, the server
refuses to start and names the damage. Without one, it drops acknowledged commits and starts.

**The cause.** In `wal/tail.rs` (`TailCheck::run`, SurrealKV 1.2.0 and `main` at 1.4.0), the
refusal needs both conditions:

```rust
if mode == WalRecoveryMode::TolerateTornTail
    && self.holds_a_value(&batch)?
    && self.synced_to(context, segment, first_end)?
```

`holds_a_value` asks whether a later record points into the value log. A later record without a
large value is counted in `dropped` and never refuses, whatever the sync marks say.

### 2. Changefeed versionstamps go back after a backward clock step and a restart (known, surrealdb#7606)

`disk=none | run T=20:-3600 K=surrealdb@25+1` broke `final_changefeed` on the first image of the
Mac engine: 24 creations got a versionstamp below one acknowledged before them. On the final
images 21 of 25 did, on both hvx runs (`fp=1a80b9b3be489c0a`) and on the Mac
(`fp=f40a0480d4fe2901`). It is saved as
`regressions/known-7606-changefeed-stamps-after-clock-step.schedule`. The changefeed's stamps come
from a clock that starts again at the wall time when the process starts. Issue #7606 (open)
describes it and its effect on a reader that resumes with `SINCE`.

### 3. Writes stall after a backward clock step (known, pull request surrealdb#7672)

With the changefeeds and the `ASYNC` event on, a backward step of one day or 30 days ended runs
on the Mac engine with `ERROR workload: exit budget exhausted`. `HlcTimeStamp::next()` waits
until the wall clock passes its last stamp. The open pull request #7672 describes it and fixes
it. With `feeds=no`, 20 schedules with clock steps of 30 s to 30 days, backward and forward,
passed every check (section "The explorations").

## The upgrade from 3.x

`upgrade/` is a variant of the example for SurrealDB's experimental upgrade of a 3.x directory in
place (`SURREAL_EXPERIMENTAL_UPGRADE_FROM_3X=true`). Its service starts 3.3.5 on RocksDB the first
time, in provisioning, and 4.0.1 on SurrealKV at every later start. So the first kill or power cut
of a run upgrades the store, with the workload running.

SurrealDB's upgrade notes (`doc/UPGRADING.md` in its repository) promise: "A process killed
mid-migration starts the migration again on the next open." The variant ran on hvx, one VM at a
time:

| schedule | runs | result |
|---|---|---|
| `disk=none \| run` (3.3.5 only) | 2 | OK both times, `fp=f85aa299c8f89a55` |
| `disk=none \| run K=surrealdb@30+1` | 2 | OK both times, `fp=affa8847754b221b` |
| `disk=cut \| run C=40` | 2 | OK both times, `fp=78e555bdca2dffd0` |
| `disk=cut cf=<k> \| run K=surrealdb@3+1`, k = 84 to 128 in steps of 4 | 12 | OK, every one |

In the runs with `K=surrealdb@30+1` the server's log (`hydo inspect logs`) shows the upgrade with
the workload running:

```text
WARN surrealdb::core::kvs::surrealkv: The directory /data/db holds a RocksDB database, which is now being migrated to SurrealKV in place. ...
INFO surrealkv::migrate::rocksdb: Migrated the RocksDB database at "/data/db": 301 version(s) in 1 table(s). The RocksDB files are kept in "/data/db/_rocksdb_backup"; ...
INFO surrealdb::core::kvs::migration: Datastore version advanced from=3.3.5 to=4.0.1
```

Of the 12 power cuts at a disk flush, the first came before the kill. The other 11 came after
`restarting surrealdb before step 4` and before the server was ready again, so inside the 4.0.1
open: the conversion or the data migrations that follow it. In each, the recovery boot finished
the upgrade, and every check held: the records written on 3.3.5, the history across both
releases, the changefeed and the live queries. Which of the 11 cut the conversion itself, and
which cut the data migrations after it, was not separated.

## The duplicate searches

Each search ran on 2026-11-16 over the issues and pull requests, open and closed, of
surrealdb/surrealdb and surrealdb/surrealkv (`search/issues`), and over their discussions
(GitHub's discussion search).

| finding | phrases | what they found |
|---|---|---|
| 1, the dropped commits | `vlog`, `value log`, `VLog CRC32`, `corrupted file`, `checksum mismatch`, `records missing after restart`, `WAL recovery`, `SalvageValidPrefix`, `records dropped`, `wal.corrupt`, `WAL segment`, `trimmed`, `vlog corruption`, `torn`, `not in the value log`, `sync mark`, `inline`, `TolerateTornTail`, `bit flip`, `checksum`, `synced commits lost`, `corruption`, `lost data after restart` | no report of this case. surrealkv#473 (closed) added the rule that ends the WAL at a record whose value is missing. surrealkv#499 and #504 (closed) made the open refuse damage that a sync mark says was synced, but only where a later record has its value. surrealkv#518 (closed) lost synced commits after a simulated crash, by another cause (a flushed memtable). surrealkv#489 (open) is about the speed of the same check |
| 2, the changefeed stamps | `changefeed versionstamp`, `clock backwards` | surrealdb#7606 (open) |
| 3, the stall | `clock backwards` | surrealdb#7672 (open pull request) |

## The explorations

| engine | image | options | seeds | failures |
|---|---|---|---|---|
| Mac | the first image with 4 drivers, clock steps on | `--strategy process --new-faults --storage-faults` | 1-12, stopped after 7 | known findings 2 and 3, and detected corruption |
| Mac | `feeds=no`, with clock steps | `--strategy process --new-faults` | 1-20 | none |
| hvx | `feeds=yes`, no clock steps, 7 drivers | `--strategy process --new-faults --more-faults --storage-faults` | 1-40 | finding 1 in seeds 3, 10 and 32; 8 seeds stopped with detected corruption |
| Mac | the final image, no clock steps | `--strategy process --new-faults --storage-faults --no-shrink` | 101-160 | finding 1 in seeds 111, 121, 150 and 156; 7 seeds stopped with detected corruption |
| Mac | the final image, no clock steps | `--strategy process --timing --preempt --no-shrink` | 201-240 | none |

That is 167 blind schedules: 27 on the Mac's first images, 40 on hvx and 100 on the Mac's final
image. Of the clock exploration's 20 seeds, 16 stepped the clock, 13 of them backward. The faults
of the hvx exploration were:

- kills in 24 seeds;
- `G=` in 22 and `L=` in 17;
- a power cut at a step in 8, and at the k-th disk flush (`cf=`) in 7.

The Mac sweep of seeds 101-160 had kills in 47 seeds, pauses in 49, `G=` in 29, `L=` in 20 and
power cuts in 12. Seeds 201-240 added tick jitter (`tj=`) in 25 seeds and forced preemption points
(`pct=`) in 17 to kills and pauses.

Apart from the known clock findings, only corrupted files broke a check. Every power cut, power
loss of the service, kill, and pause kept every acknowledged commit. No check of isolation, the
indexes, the graph, the event, or the live queries failed.

Six hand schedules on the Mac tried the disk faults that the explorer did not draw there:

- `disk=ioerr:200` failed 181 of 220 commands, and every check held: no failed write was applied.
- `disk=ioerr:500` with a kill: the restart stopped with `IO error: Input/output error (os error
  5)`, which is right.
- `disk=lying:200` with a power cut lost 12 of 12 and 21 of 21 acknowledged events. A disk that
  drops flushed writes loses them by definition, so this is no SurrealDB finding.
- `disk=cut` with power cuts at steps 25 and 50 (one after a kill) held.

## What hydo lacked

None of these is fixed here; open item 140 in AGENTS.md lists them.

1. **An exploration cannot leave out one fault kind.** The fault kinds come from `faults:` in the
   manifest file. A build parameter there (`faults: [..., "{{clockfault}}"]`) builds, but `hydo
   explore` then stops with `faults: unknown kind '{{clockfault}}'`: it reads the file, not the
   image's resolved plan. The explorer's corpus test stops on such a file too. So the example
   keeps `faults:` literal and explores without clock steps through a copy of the file next to
   it. A copy in another folder does not work, since the explorer then finds no `workload/`.
2. **No way to keep a known bug out of a blind exploration.** Half of the explorer's clock steps
   go back (`CLOCK_DELTAS` in `hydo-explore`), and every backward step reached known finding 3 or
   known finding 2. With them, 3 of the first 7 seeds on the Mac ended in a known finding. The
   example turns the changefeeds and the event off (`feeds=no`) for clock steps. A list of
   accepted findings that the explorer does not shrink, or forward steps only, would keep the
   clock in the search.
3. **Shrinking spends an hvx exploration on detected corruption.** Of the 40 hvx seeds, 8 ended
   `ERROR surrealdb restart failed (its files were corrupted)`: SurrealKV found the damage and
   refused to start, which is right. The explorer shrinks each of them like a finding. Seed 3's
   shrink took about an hour, then no console was written for 25 minutes, and the exploration was
   stopped before its report. The Mac sweep used `--no-shrink`. An option to leave the class
   `corrupted-files` with an `ERROR` verdict unshrunk would keep the time for the violations.
4. **A failed check gives no service log when no service exited.** The cause of finding 1 is one
   `WARN` line of SurrealKV at the restart. `hydo run` wrote no `logs/` for the failing runs, and
   `errors: true` reads error lines only. The line came from `hydo inspect logs` and from Docker.
5. **Versions of an image service are refused (open item 109).** The upgrade variant therefore
   starts both releases from one wrapper script. It mounts the 3.3.5 program from the root of a
   second image service, a job that only sleeps.
6. **`hydo onboard` writes `AGENTS.md` into the project (open item 121).** It wrote one into this
   example's folder; it was deleted.

## Onboarding retro

The onboarding started at 18:53 UTC on 2026-11-16. It used this tree's `hydo` over the 0.3.4
releases, on the Mac engine and on hvx on the bare-metal machine, one VM at a time on each.

- **Time to the first clean run: 18 minutes.** `hydo onboard` imported the one-service Compose
  file at once. Most of the 18 minutes went to the client. SurrealDB speaks its own query
  language, so `hydo sql-gen` did not apply. The client was written and tried against the image
  in Docker first.
- **Schedules before the first finding: about 10.** The first new finding (finding 1) came in
  the third seed of the hvx exploration, 44 minutes after the start. Before it ran 7 seeds on the
  Mac and 1 seed of the Mac's clock exploration. A hand schedule with a backward clock step reached known
  finding 2 at 19:14 UTC, before any blind schedule.
- **The findings.** One new SurrealKV bug (finding 1), found blind 7 times in 167 blind
  schedules: 3 times on hvx and 4 times on the Mac. Two known SurrealDB bugs (findings 2 and 3).
  No transaction, index, graph, event, or live-query check failed.
- **What took the most time.**
  - The hvx exploration's shrinking of detected corruption, about an hour before it was stopped
    (section "What hydo lacked", item 3).
  - Waiting for the shared Mac VM lock between the other onboardings' runs.
  - The upgrade variant: the 3.3.5 program had to come from a second image (item 5).
  - A client bug: SurrealDB writes a record id that starts with a digit in backticks
    (``lq:`5_0` ``), so the first live-query check counted no notification. The client's
    `SDB_DEBUG=1` prints every WebSocket message, and that showed it.
- **What hydo lacked.** The six items of the section "What hydo lacked".
- **What the playbook should say.**
  - For a store with its own checksums, run `--storage-faults` and read every `corrupted-files`
    run: an `ERROR ... restart failed (its files were corrupted)` is the store detecting the
    damage, and a `VIOLATION` is a loss it did not detect.
  - Mix value sizes in one workload. Finding 1 needs a large value followed by small commits; a
    workload of only small or only large values does not reach it.
  - When a known bug sits on one fault kind, put the feature behind a build parameter and explore
    the fault kind without it, instead of dropping the fault kind.
  - For a one-node database, keep the client in the guest's own network namespace (no `network:
    bridge`), and print the raw answers of the protocol in a debug mode of the client.
