# ClickHouse with two replicas under hydo

ClickHouse is a column-oriented database. Its `ReplicatedMergeTree` tables keep a copy on each
replica and coordinate the copies through ClickHouse Keeper. This example runs ClickHouse
v26.10.6.18 as two replicas of one shard with three Keeper servers, and tests two promises of
replicated inserts: a retried insert is written once, and a quorum insert is acknowledged only
when enough replicas hold it.

## The short answers

- **What was found?**
  1. **A known bug: after a power cut, a lost merged part stops replication for good.** With
     `fsync_after_insert = 1`, and the fsync after a merge and after a fetch at their defaults
     (off), one power cut at the end of the workload leaves the two replicas apart: acknowledged
     rows stay on one replica only, and `SYSTEM SYNC REPLICA` never returns. The minimal schedule
     is `disk=cut | run C=40`. The stuck fetch of a merged part that no replica holds is known
     ([#29778](https://github.com/ClickHouse/ClickHouse/issues/29778),
     [#53143](https://github.com/ClickHouse/ClickHouse/issues/53143), open); no report found
     names a power cut as its trigger. With every fsync setting on, the schedule passes.
  2. **The documented trade-off: with the default fsync settings a power cut loses acknowledged
     inserts.** `hydo explore --configs` explored the three values of the build parameter
     `fsync` with the same seeds and labeled the loss by design. It is not reported.
  3. **A new candidate: a Keeper without a quorum spins on a closed session.** A server that
     gives up its session sends `Close` and closes its socket. The Keeper's handler then polls
     the closed socket without a pause until the session timeout, 30 s. In seed 20 two handler
     threads of one Keeper took about a third of the guest's CPU. No report found names it, and
     nothing was posted (the section "The busy Keeper threads").
  4. **A new candidate on one server: after a power cut, a `MODIFY COLUMN` of a column that a
     later `DROP COLUMN` removed fails for good.** Every later mutation of the part, also an
     acknowledged `ALTER UPDATE`, waits behind it until `KILL MUTATION`. The variant `single/`
     shows it with `disk=cut | run C=6`. Nothing was posted (the section "One server: a mutation
     of a dropped column after a power cut").
  5. **A known bug on one server: an acknowledged `ALTER ADD COLUMN` and an acknowledged
     `EXCHANGE TABLES` revert after a power cut.** The table's new metadata file is swapped into
     place without a sync of its folder
     ([#111348](https://github.com/ClickHouse/ClickHouse/issues/111348), open). The variant
     `single/hydo-metadata.yaml` shows it with `disk=cut | run C=2` (the section "One server: the
     table metadata after a power cut").
- **What held?** No retried insert was stored twice: not a retry after an error or a timeout, not
  a deliberate second send of an acknowledged block, not with `insert_deduplication_token`, and
  not with async inserts. Every acknowledged quorum insert (`insert_quorum = 2`, parallel or
  sequential) was on the other replica at once, and every read with
  `select_sequential_consistency = 1` had every block acknowledged before it. No block was
  partial, and no block that ClickHouse refused before the write appeared. With the fsync after
  an insert and a merge on, no acknowledged insert was lost under kills, pauses, partitions,
  clock steps and power cuts. Only a lying disk lost some, which is the disk's own loss.
- **What did hydo lack?** Seven things, in the section "What hydo lacked". The first cost the most
  machine time: in seven runs the guest's clock nearly stopped after the Keepers lost their
  quorum, and each run waited 900 s (fixed with `guest.exact_tick`, Milestone 319). hvx's limit
  of 4 GiB for root and data disk together shaped the example.

## The version

ClickHouse v26.10.6.18 (2026-11-18, the tag `v26.10.6.18-stable`) is the newest stable release.
The images are the official `clickhouse/clickhouse-server` and `clickhouse/clickhouse-keeper` of
that build, under their tag `26.10.6`, pinned by digest in `hydo/fetch-images.sh` and in
`hydo/hydo.yaml`. The example names the tag with three numbers, because hydo's check of tracked
files reads a version of four numbers after a dash or a quote as an address.

| image | linux/amd64 digest |
|---|---|
| `clickhouse/clickhouse-server:26.10.6` | `sha256:9084cd79bde01fa4a99060d8d9cf71acdc4d3df44fce586230f566890277b2d6` |
| `clickhouse/clickhouse-keeper:26.10.6` | `sha256:ca2691948bf647fdcfebd48dc3ed304a0f1e19702afde0840504d7a6ba844d52` |

Two defaults of this release matter for the promises under test. Since 26.2 `async_insert` is on
by default, so a plain `INSERT` goes through the asynchronous insert queue unless the client
sets `async_insert = 0`. Since 26.2 `deduplicate_insert` is `enable`, so both kinds of insert are
deduplicated, with one hash kept in Keeper under `deduplication_hashes`
(`replicated_deduplication_window` 10,000 blocks, `replicated_deduplication_window_seconds` one
hour).

## The project

ClickHouse's repository has no Compose file for a replicated cluster, so this example has its
own: [docker-compose.yaml](docker-compose.yaml). It runs one shard with two replicas, `ch1` and
`ch2`, and three ClickHouse Keeper servers, `keeper1` to `keeper3`.

| file | what it is |
|---|---|
| `docker-compose.yaml` | the five services, the input of `hydo compose` |
| `conf/server.xml` | the servers' `config.d/hydo.xml`: the macros, the three Keepers, the cluster, and smaller pools and caches |
| `conf/keeper.xml` | the Keepers' configuration: three servers, the log and snapshots on the data disk |
| `build.sh` | builds `bin/chc-amd64` from `client/`, a static Linux program |
| `client/main.go` | `chc`, the test client: the schema, the inserts, the reads, the drain, and the final rules (standard library only) |
| `hydo/hydo.yaml` | the importer's five services, with the changes below, the messages check, and the parameter `fsync` with its `configs:` entry |
| `hydo/workload/` | three parallel drivers, one serial driver, and one anytime read |
| `hydo/checks/` | the two final rules over both replicas |
| `schedules/` | finding 1's schedule, which needs the image built with `--set fsync=insert` |

```sh
sh build.sh                     # needs Go 1.27.3
cd hydo && sh fetch-images.sh   # the two images by digest, network once
hydo build hydo.yaml            # on Linux (hvx or KVM); --set fsync=insert or --set fsync=1
hydo run "disk=none | run"
hydo explore --seeds 1-20 --strategy process --new-faults --cluster-faults
hydo explore --configs hydo.yaml --seeds 1-12 --strategy process --new-faults
```

The build parameter `fsync` sets the fsync settings of every table: `0` keeps ClickHouse's
defaults, which sync nothing; `insert` sets `fsync_after_insert` and `fsync_part_directory`; `1`
also sets `min_rows_to_fsync_after_merge`, `min_compressed_bytes_to_fsync_after_merge` and
`min_compressed_bytes_to_fsync_after_fetch` to 1. The example is for x86_64 guests only (hvx and
KVM). Its runs used hvx only.

The import was changed in these places:

- Every service runs its program directly, as root, on a fresh data directory (`entrypoint:`
  and `user: ""`). The images' `entrypoint.sh` changes the owners of directories in the image's
  root, which is read-only in the guest, and the first run ended with `chown:
  /var/log/clickhouse-keeper: Read-only file system`.
- The guest's CPU is `guest.cpu: v3`. On the default x86-64-v2 guest the Keeper stopped at start
  with SIGILL.
- The Keepers count as ready when they start (`ready: "true"`). The importer's check, a connect
  to 2181, the first port of the image's `EXPOSE`, never passes, because the Keepers listen on
  9181. A connect to 9181 failed after a kill of all three Keepers (the section "What hydo
  lacked"). The servers' readiness is a query (`chc ready`).
- The two servers share one unpacked image, and the three Keepers another
  (`root.share_images`). The data disk is 1,024 MiB, because the hvx engine takes at most 4 GiB
  of root and data together, and the server's program alone is 894 MB.
- The servers' `conf/server.xml` sets smaller thread pools and caches, and turns off the system
  log tables. The first start with `background_pool_size` 2 stopped with `Code: 36`: the default
  thresholds of `merge_tree` need more slots. The pool has 4 threads with a ratio of 8.

## The workload

The client `chc` speaks ClickHouse's HTTP interface. Each workload command writes one block: the
rows `(blk, i, n, v)` with `i` from 0 to `n - 1`, where `blk` is the command's id and `n` is 1 to
30. No other command writes these rows. The four tables are `ReplicatedMergeTree` tables of one
shard, ordered by `(blk, i)`, created on both replicas by `provision:`:

| table | how its blocks are inserted | command |
|---|---|---|
| `t_dedup` | `async_insert = 0`; in half of the blocks with `insert_deduplication_token = b<blk>`, else by the block's hash | `parallel_driver_dedup` |
| `t_async` | `async_insert = 1`, `wait_for_async_insert = 1`, a busy timeout of 200 ms | `parallel_driver_async` |
| `t_qpar` | `insert_quorum = 2`, `insert_quorum_parallel = 1`, `insert_quorum_timeout = 10000`; async or not, by the seed | `parallel_driver_quorum` |
| `t_qseq` | `insert_quorum = 2`, `insert_quorum_parallel = 0`, `async_insert = 0` | `serial_driver_qseq` |

Every insert is tried up to four times with the same rows, on the same replica or the other, by
the seed, until a try is acknowledged. After an acknowledgement the client sends the block once
more in one insert of three: a client that lost the answer. Each try is classified:

- acknowledged: the server answered 200;
- refused for certain: the connection was refused, or the server raised one of the exceptions
  that ClickHouse raises before it writes a part (`TOO_FEW_LIVE_REPLICAS`,
  `UNSATISFIED_QUORUM_FOR_PREVIOUS_WRITE`, `TABLE_IS_READ_ONLY`, `READONLY`, `TOO_MANY_PARTS`,
  `UNKNOWN_TABLE`);
- unknown: everything else, such as a timeout, a lost connection, `UNKNOWN_STATUS_OF_INSERT`, or a
  quorum that timed out.

A block is acknowledged when one try was, unknown when no try was and one was unknown, and
refused otherwise. Each try writes a `HYDO-HIST` record of the history `ch` before the request
and after the answer: `send <table> b<blk>`, with `ok`, `fail` or `info`.

After an acknowledged quorum insert the client reads the block from the other replica. With
`insert_quorum = 2` on two replicas both hold it now. The anytime command `anytime_read` reads
`t_qseq` with `select_sequential_consistency = 1`, or `t_qpar` without it, on one replica, and
every block acknowledged before the read must be there, whole and once.

## The rules

| rule | promise | where |
|---|---|---|
| `inserts.no-loss` | every acknowledged block is read back | the messages check `inserts` in `hydo.yaml`, drain `chc drain` |
| `inserts.at-most-once` | no block is stored twice on one replica, also after a retry of an acknowledged or unknown insert | the same check |
| `inserts.fan-out` | each replica (the groups `ch1` and `ch2`) holds every acknowledged block | the same check |
| `final_converge` | after the heal both replicas hold the same blocks of every table, with the same rows | `hydo/checks/final_converge` |
| `final_blocks` | every block is whole on both replicas, and no block whose every try was refused is stored | `hydo/checks/final_blocks` |
| `compose.parallel_driver_quorum.no_violation`, `compose.serial_driver_qseq.no_violation` | an acknowledged quorum insert is on the other replica at once | the drivers (exit status 100) |
| `compose.anytime_read.no_violation` | a read with `select_sequential_consistency = 1` (or of `t_qpar`) has every block acknowledged before it | `hydo/workload/anytime_read` |

The drain waits for each replica to answer, runs `SYSTEM SYNC REPLICA` on each table, at most
60 s per table and 120 s per replica, and then reads every block of every table. It records one
delivery per stored copy of a block (`b<blk>;o=<replica>.<copy>`), so a block stored twice is two
deliveries in one group.

## Without faults, and the regressions

Each image was built from the committed sources on the bare-metal machine with hydo 0.4.0's
release and this tree's `hydo`, `hydo-hvx` and `hydo-build`. Each schedule ran from a fresh boot
with `hydo run`.

| build | image (`*-disk.img`, sha256) | schedule | verdict | fingerprint |
|---|---|---|---|---|
| `hydo build hydo.yaml` | `aaed39f4d7160a3c…` | `disk=none \| run`, twice | OK, 78 commands | `8174700d4ae25fa3` |
| the same | the same | `disk=cut \| run C=40` | OK | `2d0c5a1749498b67` |
| `--set fsync=insert` | `9b5182d245477560…` | `disk=none \| run` | OK | `a000c048e4dc0d82` |
| the same | the same | `disk=cut \| run C=40`, twice | VIOLATION `[converge] the replicas differ in 21 blocks after the heal` | `b9f68cd13b21c4c3` |
| `--set fsync=1` | `96b2ed99370aff63…` | `disk=none \| run`, twice | OK, 78 commands | `23aed071fdf281b0` |
| the same | the same | `disk=cut \| run C=40` | OK | `b4a5182bf1a376a7` |

A run without faults takes about 37 s on the bare-metal machine when it is quiet, and 130 to 240
s when other sessions load it.

The example has no saved regression in `hydo/regressions/`, because finding 1 needs the build
parameter `fsync=insert` and every build replays that folder. Its schedule is in
`schedules/known-merged-part-lost-at-power-cut.schedule`:

```sh
hydo build hydo/hydo.yaml --set fsync=insert
hydo run "disk=cut | run C=40"   # VIOLATION [converge], b9f68cd13b21c4c3
```

## Finding 1 (a known bug): after a power cut, a lost merged part stops replication for good

**The label.** A known bug of ClickHouse: a fetch of a merged part that no replica holds stays
in the replication queue for good, while another replica holds the part's source parts
([#29778](https://github.com/ClickHouse/ClickHouse/issues/29778), closed without a fix,
[#33508](https://github.com/ClickHouse/ClickHouse/issues/33508), and the open
[#53143](https://github.com/ClickHouse/ClickHouse/issues/53143)). This example reaches it with a
power cut alone, on tables with `fsync_after_insert = 1`. No report found names that trigger.

**The schedule.** On the image built with `--set fsync=insert`:

```text
disk=cut | run C=40
```

The power goes off before step 40, at the end of the workload, and the disk keeps only what was
synced (`disk=cut`). In the recovery boot `final_converge` fails: `the replicas differ in 21
blocks after the heal`, with the fingerprint `b9f68cd13b21c4c3` in each of two runs. The
messages check's `fan-out` fails with it, and `no-loss` holds: every acknowledged block is on one
replica at least. Blind, the exploration E1 found it in seeds 1 and 5. The separation ran on the
image of E1 and on an earlier build with `fsync=insert`:

| schedule | verdict |
|---|---|
| seed 1: `disk=cut dseed=4127cc1347fae6ba \| run K=ch1@1+8 N=keeper3-*@34-39:loss,72% C=40 P=ch1@12+1 T=20:30 W=3e71cf9e,p1,q2,r1000` | VIOLATION, 13 blocks |
| without `N=`, `P=` and `T=` | VIOLATION, 11 blocks |
| without the cut, `K=ch1@1+8` alone | OK |
| the cut alone, `C=40` with seed 1's composition | VIOLATION, 13 blocks |
| the same with `disk=none` (the guest's unwritten pages lost, the disk's writes kept) | VIOLATION, 13 blocks |
| the cut before step 20 | VIOLATION, 1 block |

**What the replicas hold.** A `hydo inspect` session at the end of seed 1's run on the image of
E1 showed the state, and 13 more minutes of guest time did not change it:

- `ch2` holds every insert of `t_dedup`, as the 13 parts `all_0_0_0` to `all_12_12_0`.
- `ch1` holds only `all_4_4_0` to `all_12_12_0`. The rows of four acknowledged blocks, in the
  parts `all_0_0_0` to `all_3_3_0`, are on `ch2` only.
- The replication queue of each replica has two `GET_PART` entries, for the merged parts
  `all_0_4_1` and `all_0_9_2`. Each fails with `No active replica has part <part> or covering
  part (NO_REPLICA_HAS_PART)` and is put aside by the exponential backoff. `SYSTEM SYNC
  REPLICA` never returns.

**The cause.** The steps, by the code of the tag `v26.10.6.18-stable`:

1. A replica merges the small parts of the inserts into `all_0_9_2`. A merge syncs its part only
   when `min_rows_to_fsync_after_merge` or `min_compressed_bytes_to_fsync_after_merge` is set
   (`MergeTreeSettings::needSyncPart`, `src/Storages/MergeTree/MergeTreeSettings.cpp:3540`), and
   both are 0 by default. The other replica takes the merged part with a fetch instead of the
   small parts that it has not fetched yet. A fetch syncs only with
   `min_compressed_bytes_to_fsync_after_fetch` (`src/Storages/MergeTree/DataPartsExchange.cpp:650`),
   also 0.
2. The power cut loses the merged part on both replicas. The source parts survive on the replica
   that merged: they were synced when they were inserted, and an outdated part stays on disk for
   `old_parts_lifetime`. That replica loads them again, as the replicas' parts above show.
3. At the restart each replica finds `all_0_9_2` in Keeper but not on disk, removes it from
   Keeper and queues a fetch of it (`StorageReplicatedMergeTree::removePartAndEnqueueFetch`,
   `src/Storages/StorageReplicatedMergeTree.cpp:4904`).
4. No replica has `all_0_9_2`, so the fetch fails (`src/Storages/StorageReplicatedMergeTree.cpp:2863`)
   and the part check looks for it. It finds the source parts, whose first and last block
   numbers match the missing part's, and stops there: "Hoping that it will eventually appear as a
   result of a merge" (`ReplicatedMergeTreePartCheckThread::searchForMissingPartOnOtherReplicas`,
   `src/Storages/MergeTree/ReplicatedMergeTreePartCheckThread.cpp:174` and `:271`). No replica
   has a merge in its queue for that range, only the fetch, so the merged part never appears and
   the replica that lacks the source parts never gets them.

**The consequence.** The acknowledged rows of the lost range live on one replica only, and the
replicas stay apart until an operator acts. `SYSTEM SYNC REPLICA` hangs, and a failure of the
replica that holds the rows loses them.

**With every fsync setting on** (`--set fsync=1`) `disk=cut | run C=40` passes, and so did seed
1's schedule. So did every power cut of E2 and E3, whose tables synced merged parts but not yet
fetched ones, and of E4. On the default image (`fsync=0`, nothing synced) `disk=cut | run C=40`
passes too; why that image keeps the replicas together under this one cut was not traced. The
settings exploration below has more schedules of it.

**The native run.** None. The cause is plain in the code and in the replicas' state, and the
bug is known.

**The regression.** None in `hydo/regressions/`, because every build replays that folder and
the schedule fails only with `--set fsync=insert`. The schedule is
`schedules/known-merged-part-lost-at-power-cut.schedule`.

## The settings: the documented trade-off of the default fsync settings

`hydo explore --configs hydo.yaml --seeds 1-8 --strategy process --new-faults --no-shrink` built
one image per value of `fsync` and explored each with the same eight schedules. Four of them have
a power cut (`C=40`, seeds 1, 5, 6 and 8):

| `fsync` | the documentation's note (`tradeoff:`) | failed | the failures |
|---|---|---|---|
| `0`, the default | nothing is synced; a power loss of every replica can lose acknowledged parts | 3 of 8 | seeds 1 and 5: `no-loss`, a block acknowledged with `insert_quorum = 2` was not read back on `ch1`; seed 6: `converge` |
| `insert` | a merged part and a fetched part are not synced | 3 of 8 | seeds 1, 5 and 6: `converge`, the replicas differ in 10 to 15 blocks (finding 1); `no-loss` held |
| `1` | none | 0 of 8 | |

Seed 8's power cut passed on every value. Without shrinks the table of `hydo explore --configs`
lists no class of failure and ends "No combination failed", although 6 of its 24 runs failed. A
run with `--schedule` and seed 5's schedule did the same (the section "What hydo lacked"). So
seed 5 went through the command once more, with the shrinker:

```sh
hydo explore --configs hydo.yaml --seeds 5-5 --strategy process --new-faults
```

On the default value the shrinker took 81 tries in 22 minutes and kept the power cut and the
quorum inserts alone:

```text
disk=cut dseed=978180e49ee2ae24 | run C=40 W=:p1,r500,cparallel_driver_quorum,10.7.0.304266442,17.12.0.2022668922,22.15.0.1722227985,24.16.0.2103814160,27.19.0.591750499,34.24.0.73548128,35.25.0.1861408385,43.30.0.649141409,45.31.0.216919445,47.33.0.722241519,48.34.0.1106371267,50.35.0.125710602
```

It gives `VIOLATION [inserts.no-loss] b10 was acknowledged to process 10 at 24.050 s and never
delivered in group ch1 of channel t_qpar` with the fingerprint `a40341cdfd2dfa90` in both of
its confirmations. A block that ClickHouse acknowledged with `insert_quorum = 2`, so on both
replicas, is not on `ch1` after the power cut.

On `insert` each try of the shrink waited for the replicas, about 1.5 minutes, so the shrink was
stopped after 8 tries, and the table lists that value as not explored. With `1` the schedule
passed. The table then says of the loss on `0`:

```text
found only with fsync=0; not with fsync=1
by design: the setting's documented trade-off, not a bug
```

The command built its own images through the image cache; the one of `0` is `a1ec6d18…`.

The loss on the default value is ClickHouse's documented trade-off. `fsync_after_insert` and
`fsync_part_directory` are off by default, and the documentation describes the fsync of an
inserted part as an option that costs insert performance. It is not reported. The failures with
`insert` are finding 1: the data is not lost, but replication stops for good, which the
settings' documentation does not say.

## The busy Keeper threads (a new candidate): a handler of a closed session spins until the session times out

**The label.** A ClickHouse Keeper candidate, not reported upstream, and nothing was posted. It
wastes CPU and ends by itself. It is not the guest's environment and not hydo's faults: the
partition holds only the links between the Keepers, and the client closes its own socket. It is
the busy work behind the slow guest clock of Milestone 319 (the section "What hydo lacked").

**What happens.** When a server gives up its Keeper session, its client sends a `Close` request
and then shuts its socket down without waiting for the answer. A Keeper without a quorum cannot
answer the `Close`. Its handler thread for that connection then runs without a pause until the
session timeout ends the session, 30 s here. With one such session per server, two handler
threads of one Keeper took about a third of the guest's one CPU between them.

**The evidence.** Seed 20's schedule ran on the image built from the committed example with
`--set fsync=insert`, which sets `guest.exact_tick`. The run gave Milestone 319's verdict,
fingerprint `2be840e3c29f36dd` and transcript hash `ctl=897572c6272fa6fd`. Milestone 319's run
on the image without the key found the loop on `keeper3`. In this run it was on `keeper2`. A `hydo inspect` session at stop 364112, 16 s after the
servers ended their sessions, showed this:

- `keeper2`'s log has `Received session ID 3` on handler thread 333 and `Received session ID 4`
  on thread 338, at 00:02:33.8 of the guest's clock.
- At 00:03:14.02, `ch1` logged `Finalizing session 3` after `Received error in heartbeat response:
  Connection loss`, and `ch2` logged `Finalizing session 4` with `Hardware error: Connection loss`.
- Threads 333 and 338 were runnable (`R` in `/proc/<pid>/task/<tid>/stat`) in every sample. In
  50 ms of guest time they ran 9.2 ms and 8.2 ms by `schedstat`. In 100 samples of each
  thread's kernel stack, every top frame was `exit_to_user_mode_prepare`: the return from a
  system call or an interrupt.
- `keeper2`'s client port 9181 held two connections in `CLOSE_WAIT`, one from `ch1` and one from
  `ch2`. Each had a receive queue of 1, which is the client's FIN and no unread byte.
- `ruok` answered `imok`. `mntr`, `stat` and `cons` answered `This instance is not currently
  serving requests`, so the handlers' sessions could not be listed.

The session then went on in steps of 5 s of guest time. At 00:03:40 both connections were still
in `CLOSE_WAIT` and both threads were runnable. At 00:03:46 both connections were gone and both
threads slept. That is 30 s after the `Close`. The servers ask for a session timeout of 30 s,
and the Keepers allow it. From then on thread 333 kept 0.62 s of user time and 2.28 s of system
time. Thread 338 kept 0.63 s and 2.29 s.

**The cause in the code.** The file is `src/Server/KeeperTCPHandler.cpp` at the tag of
v26.10.6.18 (commit `68f652e0c1121f4ea60052edd98dc1db889eb4b4`, the hash in the Keeper's log):

- The main loop waits in `poll_wrapper->poll(session_timeout, *in)` (line 598). Linux uses the
  `::poll()` branch (lines 192 to 220), because `POCO_HAVE_FD_EPOLL` is a private definition of
  Poco's network library (`base/poco/Net/CMakeLists.txt`, line 7). That branch asks for `POLLIN`
  on the socket and on the response pipe.
- The loop reads the socket only when `result.has_requests && !close_received` (line 610). A
  read that finds the end of the stream ends the session (line 612). After a `Close` request,
  `close_received` is true (line 626), and the loop waits for the `Close`'s response without
  reading the socket.
- A socket with a FIN that nobody read reports `POLLIN` at once on every `poll()`. So each pass
  returns at once, does nothing, and polls again.
- Only the response to the `Close` or the session timeout ends the loop (line 711). The timeout
  counts from the last request, the `Close` (line 636).

The client's side is `ZooKeeper::finalize` in `src/Common/ZooKeeper/ZooKeeperImpl.cpp`: it
sends the `Close` (line 1551) and shuts the socket down (line 1583) without waiting for the
answer. A Keeper that has a quorum answers the `Close` when its Raft log commits it, and that
answer ends the loop. No run measured how long the loop lasts then. The same code is on
ClickHouse's `master` on 2026-11-19 (line 620 there).

**What would fix it.** After the `Close`, the handler can stop asking for `POLLIN` on the socket,
or end the session when a read finds the end of the stream.

**Why there was no native run.** The cause is plain in the code, so the candidate was not
reproduced outside hydo. Such a run needs a Keeper cluster without a quorum and a client that
ends its session.

**Its part in the runs that never ended.** On the image without `guest.exact_tick`, the 30 s of
the session timeout are guest time, and the slow clock of Milestone 319 made them last longer than
the engine's limit. So the loop kept the CPU busy for the whole run. With the exact tick the loop
ends after 30 s of guest time, and the run goes on to its verdict.

## One server: a mutation of a dropped column after a power cut (a new candidate)

**The label.** A ClickHouse candidate, not reported upstream, and nothing was posted. It is not
hydo's damage: the power cut drops only the writes that ClickHouse did not sync, and ClickHouse
itself detaches the parts that it finds broken at the start. The loss of the unsynced mutated
parts is the documented cost of the default `min_rows_to_fsync_after_merge = 0`. A mutation
queue that can never move again is not documented.

**The variant.** `single/` runs one server from the same image with one `MergeTree` table that
syncs every inserted part (`fsync_after_insert = 1`, `fsync_part_directory = 1`). The table has
one part without the column `c`. The workload adds `c`, inserts a second part, which stores `c`,
changes the type of `c`, drops `c`, and updates the row of the second part. ClickHouse
acknowledges each `ALTER` when its mutation is done. The check `mutations_done` says that every
mutation ends done and that the acknowledged update stays applied:

```sh
sh hydo/fetch-images.sh x86_64
cd single && hydo build hydo.yaml
hydo run "disk=none | run"        # OK
hydo run "disk=cut | run C=6"     # the power goes off after the last ALTER was acknowledged
```

**What happens.** On hvx with hydo 0.5.0 and the image `fa9732112e98ab3f`, `disk=cut | run C=6`
fails `mutations_done` with `3 mutations not done, row 2 note ''`, fingerprint
`b1992b2c7f5ea453` in two runs. In the recovery boot:

- `system.detached_parts` has `broken-on-start_all_1_1_0_3` to `_5` and
  `broken-on-start_all_2_2_0_3` to `_5`;
- `system.parts` has `all_2_2_0` active at data version 2 with the columns `id`, `note`, `c`, and
  `all_1_1_0_5`, mutated again after the start;
- the `MODIFY COLUMN` (`mutation_3.txt`), the `DROP COLUMN` (`mutation_4.txt`) and the `UPDATE`
  (`mutation_5.txt`) are not done, each with `Unknown expression identifier c ...
  (UNKNOWN_IDENTIFIER)`, and the server logs the error about once a second.

With `--set merge_fsync=1` (`min_rows_to_fsync_after_merge = 1`) the same schedule passes. The
blind run that first showed it had one server and a workload of all the write types. It gave the
same state for `MODIFY COLUMN c46` after the same three `ALTER`s. The acknowledged `ALTER UPDATE`s
behind it were never applied. `KILL MUTATION` of the failing `MODIFY` frees the queue; a native
check in Docker showed that.

**The cause in the code** (the tag of v26.10.6.18; `master` on 2026-11-25 has the same code):

- A mutated part is synced only with `min_rows_to_fsync_after_merge` or
  `min_compressed_bytes_to_fsync_after_merge` (`MergeTreeSettings::needSyncPart`,
  `src/Storages/MergeTree/MergeTreeSettings.cpp`, line 3540). The mutation files and the
  table's metadata are synced. So after the cut the mutations are unfinished again for the
  source parts, and the metadata has no `c`.
- `StorageMergeTree::selectPartsToMutate` puts every unfinished mutation of a part, from the
  first one on, into one mutation of the part (`src/Storages/StorageMergeTree.cpp`, lines 2566
  to 2640).
- `splitAndModifyMutationCommands` keeps the `MODIFY`'s `READ_COLUMN c` when the source part has
  `c`, without checking that the table still has it (`src/Storages/MergeTree/MutateTask.cpp`,
  lines 992 to 1000).
- `MutationsInterpreter` reads `c` (`src/Interpreters/MutationsInterpreter.cpp`, lines 1535 to
  1538 and 1673 to 1677) and resolves it against the current metadata (line 1967), which throws.
  The part keeps its data version, so the `DROP` and the `UPDATE` behind the `MODIFY` never run
  on it.
- Without a restart the state cannot happen: `DROP COLUMN c` is refused while a mutation that
  reads `c` is unfinished (`MergeTreeData::checkDropOrRenameCommandDoesntAffectInProgressMutations`,
  `src/Storages/MergeTree/MergeTreeData.cpp`, line 14105).

ClickHouse issue #120975 names the same check in `splitAndModifyMutationCommands` for an empty
part that a restart brought back, and says that a part with rows would still fail. Its fix,
#125293, changed only a test. The searches for this candidate are in the next section.

## One server: the table metadata after a power cut (a known bug, Milestone 384)

**The label.** A known bug: ClickHouse issue
[#111348](https://github.com/ClickHouse/ClickHouse/issues/111348) (open) and its pull request
[#111405](https://github.com/ClickHouse/ClickHouse/pull/111405) (open) name this cause.
Nothing was posted.

**Why it ran.** The sync audit (`hydo rules scan --kind sync`, Milestone 381) lists two places
of ClickHouse's table metadata that no finding of this example had named. They are
`renameExchange` and `renameExchangeIfSupported` in `src/Common/atomicRename.cpp`, a rename
without a sync of its folder. The audit traced them to `DatabaseAtomic::renameTable` and
`DatabaseAtomic::commitAlterTable`.

**The variant.** `single/hydo-metadata.yaml` runs the server of `single/hydo.yaml` with three
`MergeTree` tables in the default database, whose engine is `Atomic`. Each table has one synced
part. Step 0 runs `ALTER TABLE t ADD COLUMN d UInt32 DEFAULT 5`, and step 1 runs `EXCHANGE
TABLES a AND b`. Each writes its word to `$HYDO_ACKED` after ClickHouse answered. The check
`metadata_kept` says that an acknowledged `ADD COLUMN` and an acknowledged `EXCHANGE TABLES`
stay after a restart:

```sh
sh hydo/fetch-images.sh x86_64
cd single && hydo build hydo-metadata.yaml
hydo run "disk=none | run"       # OK
hydo run "disk=cut | run C=2"    # the power goes off right after the EXCHANGE TABLES's answer
```

**What happens.** On hvx with this tree's hydo and the image `dd365579233483cf`:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK | `86c85c691fbe72a2` |
| `disk=cut \| run C=1`, right after the `ADD COLUMN`'s answer | `VIOLATION [metadata_kept]`: the column `d` is gone | `4855fa01f5e06905` |
| `disk=cut \| run C=2`, right after the `EXCHANGE TABLES`'s answer | `VIOLATION [metadata_kept]`: the column `d` is gone, and `a` holds `'a'` again | `86516e86259630a9`, twice |
| `disk=cut \| run C=5`, 3 s after the second answer | the same violation | `5ddd5e7b217a3e8d` |

In the recovery boot `system.tables` lists the three tables with the metadata times of their
creation. The schedule is saved as `single/schedules/alter-and-exchange-undone-after-power-cut.schedule`.

**The cause in the code** (the tag of v26.10.6.18, and `master` on 2026-11-27):

- `DatabaseAtomic::commitAlterTable` writes the new metadata to a temporary file and syncs it
  (`fsync_metadata`, on by default). It then swaps the file into place with
  `renameExchangeIfSupported` (`src/Databases/DatabaseAtomic.cpp`, line 495 on `master`) and
  syncs no folder.
- `EXCHANGE TABLES` swaps the two tables' metadata files with `renameExchange` the same way.
- On ext4 a rename reaches the disk when the journal commits, by default within 5 s or at the
  next sync of any file. One server whose tables take no insert syncs nothing in that time.
  So a power cut 3 s after the second answer still loses both changes.

Issue #111348 shows the same for `DROP TABLE`, `RENAME TABLE` and `EXCHANGE TABLES`, and pull
request #111405 also syncs the folder at the commit of an `ALTER`.

## The duplicate searches

Each phrase was searched in ClickHouse's issues and pull requests, open and closed, with `gh
search issues --repo ClickHouse/ClickHouse --include-prs`, on 2026-11-18.

| phrase | what it found |
|---|---|
| `Hoping that it will eventually appear as a result of a merge` | #53143 (open), #10368, #64266 (open), #33508 and #29778 by other phrases: the stuck fetch of a merged part. None names a power cut |
| `searchForMissingPartOnOtherReplicas` | #53143, #42882 (the order of the replicas' parts), #125953 (REPLACE PARTITION after a power loss) |
| `No active replica has part or covering part` | #10368 (2020, closed by #25820, which replaces lost parts with empty parts) |
| `NO_REPLICA_HAS_PART` | #125953, #119811 (a torn detached part wedges the queue), #118002 (a duplicate part wedges the queue); other causes |
| `removePartAndEnqueueFetch` | #125953, #106893 (a race with the quorum cleanup); other causes |
| `fsync after merge`, `min_rows_to_fsync_after_merge` | #111269 (text indexes after a merge), #111330, #111577, #111318: power-loss durability of other paths |
| `min_compressed_bytes_to_fsync_after_fetch` | #111456, #111432 (MOVE PARTITION after a power loss), #64906 (a question about durability) |
| `merged part power loss`, `lost merged part`, `power loss replicas diverge`, `replicas differ after power`, `GET_PART merged part missing`, `outdated parts power loss`, `SYSTEM SYNC REPLICA timed out power` | nothing |
| `replication queue stuck forever` | #117107 (a duplicated `ALTER_METADATA` entry): another cause |
| `fsync_after_insert` | #130060, #130061 (the group fsync of 26.x), #111330 (object storage), #111577: other paths |
| `insert_quorum`, `insert_quorum data loss`, `select_sequential_consistency` | #131950 and #131948 (`insert_quorum = 'auto'` on one replica), #97557 (sequential consistency with Distributed tables), #93864, #43867; none was reached |
| `deduplication_hashes`, `unified deduplication` | #121174 and #120904 (the in-memory deduplication cache), #116151 (the umbrella of deduplication issues); none was reached, because every block of the workload is unique |

The changelog of the releases up to 26.10 lists no change to the search for a missing part, and
the last change of `ReplicatedMergeTreePartCheckThread.cpp` on `master` (2026-09-18) is about
`CHECK TABLE`.

The phrases of the busy Keeper threads were searched on 2026-11-19 with `gh api search/issues`
and the query `repo:ClickHouse/ClickHouse <phrase>`. It covers open and closed issues and pull
requests. On that day `gh search issues --include-prs` returned nothing for any phrase of several words.

| phrase | what it found |
|---|---|
| `close_received`, `KeeperHandler CPU` | nothing |
| `KeeperTCPHandler` (135 results, the first 12 read), `KeeperTCPHandler CPU`, `KeeperTCPHandler close`, `KeeperTCPHandler poll`, `KeeperTCPHandler poll returns` | Other causes. #119393 and #119450: a TLS connection stalls the shutdown. #118014: connections close at the shutdown. #111102: the Keeper ends active sessions of read-only workloads. #114401: a session request is lost at a change of leader |
| `keeper CLOSE_WAIT` | #55718, #55833 (connection pools of S3 disks), #42295: other components |
| `keeper busy loop`, `keeper 100% CPU`, `keeper CPU spin`, `keeper busy wait`, `keeper thread spinning`, `keeper high CPU quorum`, `keeper CPU no quorum`, `keeper CPU leader election`, `keeper cpu usage spike`, `keeper epoll`, `keeper half-closed`, `keeper close session CPU`, `keeper Close request response`, `keeper session close hang` | nothing about a handler's loop; #102419 (a refactoring of the dispatcher's sessions, closed without merge) is the nearest |

The last commits to `KeeperTCPHandler.cpp` on `master` (2026-09-11 to 2026-10-31) change timeout
messages, the handshake of an expired session, statistics, and the shutdown of TLS connections.

The phrases of the mutation of a dropped column were searched on 2026-11-25 with `gh search
issues --repo ClickHouse/ClickHouse` and `gh search prs`. The searches covered open and closed
items.

| phrase | what it found |
|---|---|
| `mutation stuck dropped column` | #134094 (a `DROP COLUMN c, ADD COLUMN c` in one `ALTER` with a skip index on `c`), #63545 (`Not found column` on replicated tables, no reproduction), #113459 (a crash inside `RENAME COLUMN`): other causes |
| `mutation UNKNOWN_IDENTIFIER` | #120975 (closed): an empty part that a restart brought back fails with `UNKNOWN_IDENTIFIER` until cleanup drops it. Same check in `splitAndModifyMutationCommands`; the issue says a part with rows would still fail, and its fix #125293 changed only a test. #130353, #130110: `ADD COLUMN` and `UPDATE` in one `ALTER` |
| `broken-on-start mutation` | #132832 (a `ReplicatedMergeTree` merge lost at a host crash, `NO_REPLICA_HAS_PART`), #125904, #113179: other paths |
| `splitAndModifyMutationCommands`, `splitAndModifyMutationCommands READ_COLUMN` | #120869 (`DROP COLUMN` accepted with a column the table lacks), #123602 (a mutation queued before `RENAME COLUMN`), #119354 (merged: a column absent from the table in some part formats); none covers a `MODIFY` of a dropped column on a part with rows |
| `MergeTreeMutationEntry` (the first run's search; #111380 read again) | #111380 and #111416: the mutation files are not synced after their rename, so an acknowledged `DELETE` or `KILL MUTATION` can be lost; here the files survived |

The phrases of the table metadata after a power cut were searched on 2026-11-27 with the same
command.

| phrase | what it found |
|---|---|
| `fsync_metadata` | #111348 (open): an acknowledged `DROP TABLE`, `RENAME TABLE` or `EXCHANGE TABLES` on an `Atomic` database reverts after a power loss, because the commit's rename is not synced. Its pull request #111405 (open) syncs the folder at that commit and at the commit of a `CREATE` or an `ALTER`. Also #115549 (open: a replica stays read-only when a power loss reverts an applied `ALTER`'s local metadata), #111381 and #111322: other files of the same kind |
| `renameExchange fsync`, `fsync directory metadata`, `power loss metadata`, `ALTER lost after power`, `metadata directory fsync`, `EXCHANGE TABLES power`, `power failure table metadata`, `sync directory after rename`, `renameat2 fsync` | nothing |

## The explorations

Every exploration ran on hvx with one worker. The first three ran without shrinks
(`--no-shrink`) on images of earlier versions of the client, with the same workload and checks:
E1 on the tables with `fsync=insert`, E2 and E3 on tables with the fsync after a merge too, but
not yet after a fetch. CFG and E4 ran on the committed sources.

| exploration | image | options | seeds | schedules | failures by cause |
|---|---|---|---|---|---|
| E1 | `fsync=insert` | `--strategy process --new-faults --cluster-faults` | 1-8 (stopped) | 8 | finding 1 (seed 5; seed 1 in its first try), a recovery boot without memory for its VM (seed 1, the machine's), a Keeper not ready after a kill of all three (seed 6, hydo's restart order) |
| E2 | `insert` and merges | the same | 1-20 | 20 | the guest's clock nearly stopped after a ring partition of the Keepers (seed 20; Milestone 319) |
| E3 | the same | the same and `--more-faults` | 21-40 | 20, 4 of them with `cf=` | the guest's clock nearly stopped: seeds 27 and 40 (a ring partition of the Keepers) and 33 (a kill of two Keepers and a forward clock step) |
| CFG | each value of `fsync` | `--configs`, `--strategy process --new-faults --no-shrink` | 1-8 per value | 24 | `0`: 3 (2 lost blocks, 1 `converge`); `insert`: 3 (finding 1); `1`: none. Then seed 5 alone with shrinks (the section on the settings) |
| E4 | `fsync=1` | `--strategy swarm --new-faults` | 41-52 | 12 | 6, each with a disk fault: a lying disk lost acknowledged blocks (seeds 42, 46, 51) or left `ch1` without an answer (49), and after disk I/O errors `ch1` did not start again (43, 44). The disk broke its own promise; the 4 power cuts with `disk=cut` passed |

The faults the explorations drew were kills of a service and of groups (one replica, both
replicas, a majority of the Keepers), pauses, network faults inside and between the groups
(partitions, one-way loss, delays), clock steps from 30 days back to a year forward, power cuts
at a step and at a disk flush (`cf=`), and in E4 a lying disk and disk I/O errors. The clock audit
(`hydo rules scan --kind clock` on ClickHouse's `src/Storages`, `src/Coordination`,
`src/Common/ZooKeeper`, `src/Interpreters`, `src/Processors/Sinks` and `src/Server`) listed 24
functions; its first three places, two in the merge strategy picker and one in the replica delay,
gave the schedules `T=1:-300 K=ch2@2+3`, `K=ch2@2+3 T=10:-300` and `K=ch2@2+3 T=10:300`. All three
passed.

## What hydo lacked

In the order of the time each gap cost:

1. **The guest's clock nearly stopped after the Keepers lost their quorum.** Seed 20 of E2 and
   seeds 27 and 40 of E3, each with a ring partition of the Keepers (`N=keepers.ring`), and seed
   33 of E3, with a kill of two Keepers and a forward clock step, ended `TIMEOUT workload` after
   900 s. Seed 20 did it in three runs and seed 27 in two, seven runs in all. The agent had
   answered its last command, and nothing in the guest ended. Milestone 319 found the cause in
   hvx's guest kernel: its clock counts a tick as 999,848 ns while its timer sets ticks 1 ms
   apart, so after two minutes each tick was due 19 ms ahead on the virtual TSC. Two
   `KeeperHandler` threads of `keeper3` kept the CPU busy, and the guest's clock ran about 1.2 s
   in 100 s of host time. Those threads are ClickHouse's own loop (the section "The busy Keeper
   threads"). With `guest.exact_tick: true`, now in `hydo/hydo.yaml`, the loop ends after the
   session timeout, and seed 20 ends with finding 1's verdict. Open item 164.
2. **The hvx engine's disk limit.** The first run stopped at `HVX_SET_DISK: Invalid argument`:
   hvx takes at most 4 GiB of root and data disk together (`HVX_DISK_MAX`). The server's program
   is 894 MB and the Keeper's 566 MB. The example shares each image between its services
   (`root.share_images`) and keeps the data disk at 1,024 MiB, so the image is 3.3 GB. A third
   replica or a larger data set does not fit. Open item 157.
3. **Memory on a shared machine.** One explorer worker with a 4 GiB guest held about 7 GB of the
   host. With two of this onboarding's VMs and the other sessions, the host's out-of-memory
   killer ended one run, and one recovery boot could not make its VM (seed 1 of exploration E1).
   The scripts then held one lock for every VM of the onboarding. Open item 157.
4. **A group's restart and the Keepers' readiness.** After `K=keepers.all@3+1` hydo restarts the
   members one at a time, each after the one before is ready. A Keeper opens its client port
   only with a quorum, so `keeper1` was not ready after 120 s and the run ended `ERROR keeper1
   restart failed` (seed 6 of E1). The Keepers now count as ready when they start
   (`ready: "true"`), as open item 147 says for TiKV's PD.
5. **The import.** `hydo compose` took the Keeper's port from the image's `EXPOSE` (2181; the
   Keepers listen on 9181), kept the images' `entrypoint.sh`, which changes owners in the
   read-only root (`chown: /var/log/clickhouse-keeper: Read-only file system`), and did not
   propose `guest.cpu: v3`, without which the Keeper stopped with SIGILL. Each cost a build and a
   run of about 10 minutes. The playbook's table of setup problems lists the first two kinds.
6. **`hydo explore --configs` without shrinks.** With `--no-shrink` its table lists no class of
   failure and ends "No combination failed", although 6 of its 24 runs failed. With `--schedule`
   it did the same for 2 failed runs of 3: the explorations' `exploration.json` lists no
   failure. A third run, of one seed with shrinks, gave the classes. Open item 157.
7. **A command's output stays in the guest.** A workload command's output goes to
   `/tmp/hydo/compose/log.<n>` in the guest, and a run directory has no copy. The client writes
   each failed try to the console as well (`chc: <table> b<blk> on <host>: <error>`), which
   showed that 26.2's default `async_insert = 1` refused every sequential quorum insert.

On the shared machine a shrink candidate of a power-cut schedule took 1 to 5 minutes. So E1 to
E4 ran with `--no-shrink`, and the minimal schedule of finding 1 came from directed runs. The
settings exploration shrank, and its shrinks took most of its time.

## Time

The onboarding ran on 2026-11-18 and 19 on the bare-metal machine, beside two other onboardings.

| phase | wall time | what took it |
|---|---|---|
| scouting | 30 minutes, beside the setup | the settings' documentation in `src/Core/Settings.cpp` and `MergeTreeSettings.cpp`, and the sink's retry code |
| setup to the first clean run | 50 minutes | five image builds of about 4 minutes each: the disk limit, the entrypoint, SIGILL, the pool sizes, and 26.2's `async_insert` |
| explorations and judging | 4 hours 10 minutes | E1 to E3, about 30 directed runs, two `hydo inspect` sessions; seven runs that each ran into the 900 s limit; waits for memory |
| E4 and the settings exploration | 2 hours 15 minutes | 12 swarm schedules; three images and 24 schedules, a run of one schedule, and the shrinks of seed 5 |
| the duplicate search and the write-up | 1 hour, beside the machine time | |
| the tests and the goldens | 1 hour 30 minutes of machine time, twice | the controller's corpus test takes 24 minutes; the image tag changed after the first recording |
