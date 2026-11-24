# Kvrocks under hydo: a master, two replicas, WAIT and failovers

Apache Kvrocks (github.com/apache/kvrocks) is a key-value database that speaks the Redis protocol
and keeps its data in RocksDB. A master replicates its RocksDB write-ahead log to its replicas.
`WAIT n` blocks until `n` replicas have acknowledged the writes before it, and a replica that
cannot resume from the master's log takes a full synchronization: it fetches a checkpoint of the
master's database and swaps it in. This example runs a master with two replicas and a client
that fails the master over to a replica, as Redis Sentinel does, in one guest of the Mac engine,
and looks for bugs that nobody reported.

## The short answers

- **What was found?** Two new candidates. Nothing was posted.
  1. **New: a node that has just rejoined by a full synchronization does not start again after
     a power loss.** The replica writes the checkpoint's files (SST files, the MANIFEST,
     `CURRENT`, a WAL) without a sync, renames them into place, and destroys its old database. A
     power cut of the guest, or a power loss of that node alone, in the next seconds leaves the
     fetched SST files empty, and the node stops at every start with `Corruption: Sst file size
     mismatch between expected 2080 and file system 0`. This happens with
     `rocksdb.write_options.sync yes`, the durable setting
     (`regressions/node-unstartable-after-full-sync-and-power-cut.schedule`,
     `regressions/node-unstartable-after-full-sync-blind.schedule`).
  2. **New, a clock effect that breaks `WAIT`: after a backward step of the wall clock, `WAIT`
     answers 0 for the step's length, although both replicas hold every write.** A replica
     sends its periodic acknowledgement only when the wall clock has moved a second past the
     last one, and an idle master forces none. The master's `INFO replication` reports a lag for
     both replicas for as long
     (`regressions/wait-answers-0-after-a-backward-clock-step.schedule`).
- **What held?** With `rocksdb.write_options.sync yes` and `use-rsid-psync yes`, every write that
  `WAIT 2` confirmed stayed as acknowledged, also across failovers and the late writes of a client
  that had not learned of a failover yet. That held through kills, pauses, partitions and one-way
  loss, one node's power loss, damaged files, power cuts of the whole guest and clock steps, as
  long as no node had to start again right after a full synchronization (finding 1). Every read
  of both replicas right after `WAIT 2` saw the write. Every `MULTI`/`EXEC` transaction and every
  Lua script had all of its writes or none on every node. No counter went past its `INCR`s, no
  `SET ... PXAT` key went away before its deadline, and no replica served a value that no write
  sent. After the heal the replicas held what the master held, key for key.
- **What is documented or the environment?** With Kvrocks's default `rocksdb.write_options.sync
  no`, a power cut of the guest lost 8 of 70 acknowledged writes in the first run of seed 1, as
  `kvrocks.conf` says it may; with `yes` no power cut lost one. `use-rsid-psync no` was not
  explored (section "The explorations"). A damaged `CURRENT` file, SST file or WAL stops its
  node at the next start: RocksDB detects the damage, and Kvrocks refuses to open the database. `CONFIG REWRITE` writes the
  configuration without a sync, which is the open issue apache/kvrocks#3710; no run showed it.
- **What did hydo lack?** The section "What hydo lacked": the scan of write routes and the clock
  audit found nothing in Kvrocks's C++, a reaction whose event never came passed without a word,
  and a grid of `--reactive-at` with `--seeds` ran one point per seed.

## The version

- Kvrocks 2.17.0, the newest release (2026-09-19), the official image `apache/kvrocks:2.17.0`:
  index `sha256:a1e99b3653fb900d5a17b80a9487f74079edc6d5e9b70e3103c01cbfc0d5146a`, linux/arm64
  `sha256:a56ba7b968d4e0900159af2864606dcf45ade840e90a26f8d45979e4ac4813af`. The image's revision
  is the commit `185d4221`.
- The causes were read at the tag `v2.17.0` and on the default branch `unstable` at `ff8bb662`
  (2026-11-24), where both are unchanged.
- hydo: `main` at `b8c698d7` (0.5.0 and Milestones 344 to 349), the Mac engine, one worker of
  1,024 MiB.

## The promises

The promises come from `kvrocks.conf` of 2.17.0 and from the semantics of the Redis commands that
Kvrocks implements. No owner of Kvrocks confirmed them; the ones marked "unconfirmed by the
owner" are read from the code and the Redis documentation only.

| promise | source |
|---|---|
| with `rocksdb.write_options.sync yes`, an acknowledged write survives a machine crash | `kvrocks.conf`: "If this flag is disabled, and the machine crashes, some recent writes may be lost" |
| a replica that acknowledged a change does not lose it at a machine failure | `kvrocks.conf`, `replication-group-sync`: "It would still guarantee replica would not lose any data with machine failure once it has acked the change" |
| `WAIT n` answers once `n` replicas acknowledged the writes before it | the `WAIT` command (pull requests #3061 and #3075); unconfirmed by the owner for Kvrocks |
| with `use-rsid-psync yes`, a replica of another history takes a full synchronization | `kvrocks.conf`, `use-rsid-psync` |
| a write that `WAIT 2` confirmed survives the loss of the master | follows from the two rows above; unconfirmed by the owner |
| a replica never serves a value that no master had | unconfirmed by the owner |
| a transaction and a Lua script apply all of their writes or none | Redis semantics; Kvrocks commits `EXEC` as one RocksDB batch (`src/commands/cmd_txn.cc`); unconfirmed by the owner |
| an expiry does not delete a key before its deadline | Redis semantics; unconfirmed by the owner |
| a node starts again after a power loss | follows from the first two rows; unconfirmed by the owner |

Kvrocks's own failover tools are for its cluster mode (kvrocks-controller), and its
documentation names Redis Sentinel for a master with replicas. The example has no Sentinel: the
client fails over with Sentinel's steps.

## The project

`docker-compose.yaml` runs three nodes, `kv1`, `kv2` and `kv3`, from the official image, each
with a volume for `/var/lib/kvrocks`. `config/start.sh` copies `config/kvrocks.conf` into the
volume as `node.conf` at the first start and adds `slaveof kv1 6666` to the replicas; Kvrocks
rewrites that copy at every start and at `CONFIG REWRITE`, so it cannot live in the read-only
image. The template changes only these settings from the defaults:

- `rocksdb.write_options.sync yes` and `use-rsid-psync yes`, from the environment
  (`KVROCKS_SYNC`, `KVROCKS_RSID`), the build parameters `sync` and `rsid` of `hydo.yaml`;
- one `db-name` for every node, since a replica refuses a master of another name;
- smaller memory settings (`workers 4`, a 64 MiB block cache, 8 MiB memtables), since the
  defaults reserve a 4 GiB block cache.

The nodes run as root, so that a fresh volume needs no change of owner; the image's user is 999.
`hydo compose` imported the file (`hydo/hydo.yaml`), with these changes: each node's
`/var/lib/kvrocks` is a file system of its own (`storage:`), the readiness checks ask `PING` with
the test client, and the client `kvc` is in the image. Without faults the three nodes take about
13 MiB each.

## The workload

`client/` is the Go client `kvc` on go-redis v9.23.0, without its retries, so that one command is
sent once (`sh build.sh` builds it). Its writes, one command file each in `hydo/workload/`:

- registers: `SET` of a new key, `SET ... PXAT` with a deadline 3 s to an hour ahead, `SET` or
  `HSET` over a known register, `CAS` (Kvrocks's compare-and-set) from the last value, `DEL`,
  and `HSET` of a new hash field;
- `INCR` of one of four counters;
- members: `RPUSH`, `SADD` and `ZADD` of a new element;
- `MULTI`, `SET`, `HSET`, `SADD`, `RPUSH`, `EXEC` on four new keys, and `EVAL` of a script that
  `SET`s and `SADD`s two new keys;
- `serial_driver_failover`: in one call of two, it promotes a replica with `SLAVEOF NO ONE`,
  writes the new master's name to a synced file, sends one late `SET` to the old master as a
  client does that has not learned of the failover, points the other nodes at the new master,
  and runs `CONFIG REWRITE` on each, as Sentinel does;
- `serial_driver_flush`: `FLUSHMEMTABLE` on the master, so that its data reaches SST files, as
  a larger data set does by itself.

The token `M` (the `knobs:` entry of `hydo.yaml`) sets the client's mode. With `wait`, the default, a write is acknowledged when the
master replied and `WAIT 2 5000` answered 2; a reply without that is an unknown outcome. With
`master`, the master's reply alone acknowledges a write, and the failover does nothing. Each
write is recorded in `$HYDO_ACKED`, `$HYDO_OUTCOMES` and the history `writes`. An error reply that
means Kvrocks did nothing (`READONLY`, `LOADING`, `EXECABORT`, a `CAS` or `DEL` that found
another value) is a failed write; a timeout or no answer is unknown.

`hydo rules scan --kind writes` found no route in Kvrocks (section "What hydo lacked"). The
command table (`MakeCmdAttr` in `src/commands/`) flags 143 commands `write`. The workload covers
the families above; the others (streams, JSON, Bloom and Cuckoo filters, time series, search,
geo, `HyperLogLog`, bit operations, `RENAME`, `MOVE`) and the admin commands that drop data
(`FLUSHALL`, `FLUSHDB`) have no command.

## The rules

| check | promise | file |
|---|---|---|
| `writes` (`type: messages`, `acked_outcome` with `effect: recorded`) | every acknowledged write is there at the end, unless a later write of its register replaced it or its deadline passed; no refused write took effect | `hydo.yaml`, `kvc readback` |
| `eventually_converge` | within 120 s of the heal both replicas are connected and hold what the master holds, key for key | `checks/eventually_converge` |
| `eventually_wait` | within 120 s of the heal `WAIT 2` on the master answers 2 | `checks/eventually_wait` |
| `final_atomic` | on every node each transaction and script has all of its writes or none | `checks/final_atomic` |
| `final_counters` | each counter is at least its acknowledged `INCR`s and at most those and the unknown ones | `checks/final_counters` |
| read after `WAIT 2` | a write that `WAIT 2` confirmed is on both replicas at once (the write's command exits 100) | `kvc op` |
| read your writes | with `M=master`, a `CAS` or `DEL` of a known register finds its last acknowledged value | `kvc op` |
| no phantom read | a read of any node returns only values that some write sent | `anytime_read` |

## Without faults, and the regressions

Two runs of `disk=none | run` gave one fingerprint on the first image, `2c217adf40766fc4`, with
72 writes and one failover. The fingerprints below are of the image before the expiry check's fix
(section "Not bugs"); the fixed client's image did not run (open item 179). A run has about 70
writes and one to four failovers.

| file | finding | fingerprint |
|---|---|---|
| `node-unstartable-after-full-sync-and-power-cut.schedule` | 1, directed | `25cccdf92225932b` |
| `node-unstartable-after-full-sync-blind.schedule` | 1, as the explorer found and shrank it | `f8292ba2526b369d` |
| `wait-answers-0-after-a-backward-clock-step.schedule` | 2 | `b4bc897a4805d129` |

`hydo repro --regressions` gives each one's violation.

## What was found

### 1. New: a node that has just rejoined by a full synchronization does not start again after a power loss

**The schedule.** `disk=cut dseed=0 | run W=6311ab23,p4,q3,r500 A=kvrocks.failover_done+5500:cut`
ends `VIOLATION service cannot recover (committed data unrecoverable) (kv1)`. The restarted kv1
writes `Failed to open the database: Corruption: Sst file size mismatch between expected 2080 and
file system 0` and exits, at every start. The blind form, `disk=cut dseed=0 | run C=40
W=d6d260d0,p3,q2,r500,oanytime_read`, is one power cut of the guest at the end of the workload.
With one node's power loss (`L=kv2@22+3` in seed 22 of e2) kv2 stops the same way:
`Sst file size mismatch between expected 1735 and file system 0 sstable:
/var/lib/kvrocks/db/000014.sst`.

**What happens.** A failover makes another node the master. The old master took one more write
after the promotion, so its history differs, and `PSYNC` answers `wrong replication id of the
last log`. A replica whose sequence is not in the master's log gets `sequence out of range`. Both
switch to a full synchronization. Then:

1. `ReplicationThread::fetchFile` (`src/cluster/replication.cc` lines 1040 to 1112 in 2.17.0,
   1056 to 1128 on `unstable`) writes each file of the master's checkpoint with
   `WritableFile::Append` and never calls `Sync` or `Fsync`. `SwapTmpFile`
   (`src/storage/storage.cc` line 1428, 1427 on `unstable`) renames `<file>.tmp` to `<file>`.
2. `Storage::RestoreFromCheckpoint` (`src/storage/storage.cc` lines 500 to 554) renames the
   database directory to `db.tmp`, renames the checkpoint directory to `db`, opens it, and
   destroys `db.tmp`. No directory is synced.
3. RocksDB's open writes a new MANIFEST and `CURRENT` and syncs them, so after the cut they name
   the fetched SST files with their sizes. The fetched files' data is still in the page cache
   until the kernel writes it back, about 30 s later, and a power cut leaves them at length 0.
4. RocksDB refuses the database, and Kvrocks exits at every start. The node's old database is
   gone, so it has nothing to fall back on.

The node is a replica, so the data is still on the master. But the replica does not come back
without an operator, who must delete its database, and while it is down the master has one
replica less, for `WAIT` and for the next failover.

**How often.** The grid r3 of `--reactive-at kvrocks.failover_done`, a whole-guest cut 0.5 s to
23 s after a failover, failed 14 of 30 times. Seeds 1 to 10, which ran the default composition,
all passed; why was not traced. Blind, e2 (process strategy, seeds 21 to 80) found it in 10 of 60
schedules, by a power cut of the guest and by one node's power loss. In e3 (the master's reply as
the acknowledgement, no failover) seed 40 reached it without a failover: `G=` flipped a bit in
the master's WAL before step 14, and after the power cut the replica kv2, whose own files were
not damaged, did not start again. Its log was not read: the replica's reason, and whether a full
synchronization came first, are not confirmed.

**Why the first scans passed.** A checkpoint of a small data set holds only a WAL, which RocksDB's
open replays into a new, synced SST file. Only with SST files in the checkpoint does the fetched
data matter. The workload's `FLUSHMEMTABLE` gives the master SST files, as a data set larger than
the memtables does by itself.

**Duplicate search.** No issue or pull request names the unsynced fetch (section "The duplicate
searches"). Open neighbours are #3669 (a crash when a full synchronization fails after the
database was closed) and #3727 (a reopen after a failed full synchronization).

**No native run.** The cause is plain in the code: no sync on the path from the socket to the
database.

### 2. New, a clock effect that breaks `WAIT`: after a backward clock step, `WAIT` answers 0 although the replicas hold every write

**The schedule.** `disk=none dseed=0 | run M=master T=17:-300 W=740c6cfa,p1,q3,r500` ends
`VIOLATION [wait] not satisfied 125.18s after heal, deadline 120s (last check rc=1: WAIT 2 5000
on the master kv1 answered 0; it reports slave0 ip=10.89.0.13,port=6666,offset=26,lag=64 and
slave1 ip=10.89.0.12,port=6666,offset=26,lag=64)`. In the same run `eventually_converge` held
0.10 s after the heal: both replicas held every key. The same schedule without `T=` passes, and
`T=17:-648000` fails the same way. The explorer found it blind in 9 of e3's 40 schedules and
shrank it to the clock step alone.

**The cause.**

1. A replica acknowledges in `ReplicationThread::sendReplConfAck` (`src/cluster/replication.cc`
   lines 666 to 682 in 2.17.0, 674 on `unstable`). Unless forced, it sends `REPLCONF ACK` only
   when `util::GetTimeStamp() - last_ack_time_secs_ >= 1`. `util::GetTimeStamp` reads
   `std::chrono::system_clock` (`src/common/time_util.h` line 30), the wall clock. After a step
   back, the difference is negative until the clock passes the old value.
2. The master forces an ack with `_getack` only inside a bulk of write batches, and only for a
   `WAIT` that was waiting when it sent the bulk (`FeedSlaveThread::loop`, lines 233 to 240). An
   idle master sends `ping` every 1,000 rounds of 2 ms (`checkLivenessIfNeed`, lines 112 to 120),
   and the replica answers a ping with an ack that is not forced (line 746).
3. So after the step no ack arrives. `GetReplicasReachedSequence` (`src/server/server.cc`
   line 813) counts no replica, `WAIT` answers 0 at its timeout, and `INFO replication` reports
   the lag of the last ack.

With the default mode, each write's `WAIT` came with a bulk that carried `_getack`, and no run
showed the effect. A client that calls `WAIT` after its write's batch went out, or after a quiet
spell, waits the step's length. Monitoring that reads the lag from `INFO replication` sees
replicas behind for as long. The general idea of reading a monotonic clock was discussed in
#1466 (closed in 2023); the time-throttled ack came later, with #3061 and #3220.

**No native run.** The cause is plain in the code.

### Not bugs

- **A mistake of the expiry check.** In e4, `T=17:-648000` alone failed `writes.acked-outcome`:
  `setex.e9.0.v9t1795478418347 was acknowledged and ended succeeded, but its effect is not
  there`. The key's deadline was 18.3 s after the guest clock's start, and the clock stepped back
  at 24.0 s, so the key had expired as set before the step. The read-back compared the deadline
  with the clock after the step and required the key. The client now records the latest clock
  that the run saw (`/data/kvc/maxclock`) and requires a key only while that clock is before the
  deadline too. The fixed client did not run yet: the Mac's disk was low (open item 179).
- **Detected damage.** In e1, e2 and e3, 16 schedules damaged `CURRENT`, an SST file, a WAL or
  `node.conf` with `G=`. The node then stopped at its start: RocksDB checks its files, and
  Kvrocks does not start with a damaged configuration.
- **The ack throttle under writes.** With a `WAIT` after every write, the backward steps of e1
  and e2 broke nothing, because each `WAIT` forced an ack (finding 2).
- **`SLAVEOF` refuses the promoted node for a moment.** An old master lists the promoted node
  among its replicas until a send to it fails, and refuses `SLAVEOF` to it with `can't replicate
  your own replicas` (`src/commands/cmd_server.cc` line 1148). The client tries again for up
  to 20 s, as a failover tool must.

## The duplicate searches

`gh search issues --repo apache/kvrocks --include-prs`, open and closed, on 2026-11-24:

| phrase | what it found |
|---|---|
| `Sst file size mismatch` | nothing |
| `fullsync fsync`, `full sync power`, `fullsync corruption`, `crash after fullsync` | nothing |
| `RestoreFromCheckpoint` | #3669, a crash when a full synchronization fails; another cause |
| `fullsync` | #3727, #3669, #2833, #3822, #3811, #3769: failed or slow synchronizations, none after a power loss |
| `checkpoint fsync`, `sync checkpoint` | #3769, #3559, #3483: the checkpoint's lifetime and file names |
| `power loss`, `fsync` | #3710 and #3712, `CONFIG REWRITE` without a sync; #3602 and #3603, `FLUSHWAL` |
| `NewTmpFile`, `SwapTmpFile`, `fetch file sync`, `replica corruption` | nothing |
| `WAIT replica`, `WAIT command`, `sendReplConfAck`, `replconf ack` | #3061, #3075, #3117, #3220 (the `WAIT` features), #3157 and #3197 (flaky tests); none about the clock |
| `last_ack_time`, `WAIT returns 0`, `WAIT timeout replica`, `replication lag wrong` | nothing |
| `system clock`, `time jump`, `ntp`, `clock` | #1466 (a proposal to read `steady_clock`, closed), #983, #1038; none about the ack |

## What held, and why

- `rocksdb.write_options.sync yes` syncs the WAL before a reply, so no power cut lost a write
  that the master acknowledged (e3, with `M=master`), and none lost a write that `WAIT 2`
  confirmed (e2).
- `use-rsid-psync yes` sent every node with a late write to a full synchronization, so after
  every failover the replicas held the new master's data and nothing else.
- `EXEC` and `EVAL` commit one RocksDB batch, which replication ships whole, so no node ever held
  part of a transaction.
- `SET ... PXAT` stores the absolute deadline, so clock steps moved no deadline; reads hide an
  expired key on every node.

## The explorations

| name | options | seeds | schedules | failures |
|---|---|---|---|---|
| e1 | first image (no late write, no flush); process strategy, `--cluster-faults --storage-faults` | 1-20 | 20 | 3, all detected damage |
| r1 | `--reactive-at kvrocks.failover_done`, cuts 0.5 s to 23 s after it, image with the late write | the grid | 10 | 0: the checkpoints held no SST file |
| r2 | the same with `FLUSHMEMTABLE`, failover in one call of three | 1-3 | 3 | 0: no failover in these runs, so no cut |
| r3 | the same, failover in one call of two | 1-30 | 30 | 14, all finding 1 |
| e2 | final image; process strategy, `--cluster-faults --storage-faults` | 21-80 | 60 | 16: finding 1 (10), detected damage (6) |
| e3 | `M=master` (built as a parameter then); process strategy, `--cluster-faults --storage-faults` | 1-40 | 40 | 17: finding 2 (9), detected damage (7), finding 1 (1) |
| e4, `sync=yes rsid=yes` | `--configs --configs-mode one`; process strategy | 1-20 | 20 | 6: finding 1 (5), the expiry check's mistake (1) |
| e4, `sync=no rsid=yes` | the same, stopped for the Mac's disk and memory before its shrinks | 1-20 | 20 | 7 first runs: finding 1 (5), the expiry check's mistake (1), and 8 acknowledged writes lost at a power cut (seed 1), the documented cost of `sync no` |

e4's third combination, `use-rsid-psync no`, did not run: the Mac had 5 GB of free disk and its
swap was full, so the exploration was stopped before it built that image. Without its shrinks and
its table, `hydo explore --configs` did not label seed 1's loss; the label here is from
`kvrocks.conf`.

`hydo stats` judged e2 flat over its last 15 runs: no new verdict or property outcome after run
37 of 60. All ran on one worker of 1,024 MiB, at 240 to 560 runs an hour.

## What hydo lacked

In the order of the time each cost:

1. **A reaction whose event never comes passes without a word.** In r2, three schedules of
   `A=kvrocks.failover_done+...:cut` passed, and no failover had run, so no cut came. Nothing in
   the report said that the reaction never fired. It took a look at the consoles.
2. **`--reactive-at` with `--seeds`.** `--seeds 1-3` ran the grid's first three points, one per
   seed, not the grid three times. The whole grid three times needed `--seeds 1-30`. `hydo guide
   faults` says "more seeds run it again with each seed's composition", which reads the other
   way.
3. **The clock audit found nothing.** `hydo rules scan --kind clock` named 0 functions in 255
   files. Kvrocks reads the wall clock through its own `util::GetTimeStamp`
   (`src/common/time_util.h`), which the audit does not follow, as open items 152 and 171 say
   of other projects' clocks. Finding 2's place was found by reading the code.
4. **The scan of write routes found nothing.** `hydo rules scan --kind writes` read one file and
   found no route: a Redis-compatible server's commands are a table of `MakeCmdAttr` entries
   with a `write` flag, not routes. The 143 write commands were listed by hand (open item 175).
5. **A power loss with a failed restart has no log in its diagnosis.** For `ERROR kv2 restart
   failed (after its power loss)` the `hydo: diagnosis:` line names only the error, while
   `logs/kv2.log` has RocksDB's reason. After a whole-guest cut the line quotes the log.
6. **A regression cannot carry a build parameter.** The regressions of one folder run on one
   image. Finding 2 needed the client's other mode, first built as a parameter; it became the
   token `M` so that one image carries all three regressions.
7. **No reaction cuts one service's power.** A reaction can cut the whole guest (`:cut`), kill
   or stop a service, but not take one node's power (`L=`). Finding 1 with one node's power loss
   came only from blind `L=` schedules.
8. **Passing runs counted as vacuous** (open item 177): with the hydo of `b8c698d7`, 17 of 20,
   44 of 60 and 23 of 40 passing runs carried the warning, although `acked-outcome` judged about
   70 writes in each. Milestone 350 fixed it: a claim with nothing to judge beside one that
   judged is no vacuous pass.
9. **A tree build in a target folder of its own has no release.** hydo uses `HYDO_RELEASE_DIR`
   only when its program runs from `<checkout>/target/...` (`checkout_of` in
   `hydo/crates/hydo-cli/src/ctx.rs`). Built with `CARGO_TARGET_DIR` outside the checkout, as
   worktrees beside each other must, `hydo build` looked for the guest files beside the target
   folder and stopped (`cannot read .../target/guest/tc-closure.txt`). A folder with
   `SHA256SUMS`, links to the release's files and copies of the three programs worked.

## Time

| phase | wall time |
|---|---|
| scouting (the configuration, the replication code, the issues) | 40 minutes |
| setup to the first clean run (the client, the Docker check, the import) | 1 hour 10 minutes |
| exploration, shrinking and judging | 2 hours 30 minutes |
| duplicate search and write-up | 1 hour |
