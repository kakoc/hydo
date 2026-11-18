# The VictoriaMetrics cluster under hydo: replicated samples across crashes, partitions, and clock steps

VictoriaMetrics (github.com/VictoriaMetrics/VictoriaMetrics) is a time series database. Its
cluster version splits the work into three programs: `vminsert` takes the writes, `vmstorage`
stores the samples, and `vmselect` answers the queries. With `-replicationFactor=2`, vminsert
keeps two copies of every sample on two vmstorage nodes, and vmselect removes the second copy
when it reads. This example runs two vmstorage nodes, one vminsert, and one vmselect from the
official images in one hvx guest. It hunts for bugs that nobody reported in what the cluster
documentation promises about replication.

## The short answers

- **What was found?** Three new candidates, all clock effects, none posted upstream:
  1. **A backward clock step deletes the current month's data on every vmstorage node.** Since
     v1.141.0 a watcher drops every monthly partition that starts later than now plus
     `-futureRetention` (2 days by default). After the clock goes back 20 days on 2026-11-18,
     the partition `2026_11` starts more than 2 days after the stepped clock. Within a minute
     both nodes delete it, with its index, and every sample acknowledged before the step is
     gone for good. Near the start of a month a step of a little more than 2 days is enough.
     v1.140.0 keeps the partition (`regressions/month-deleted-after-clock-step.schedule`).
  2. **After a backward clock step, vmstorage holds new samples in memory for the step's
     length.** The buffers of new rows and new index entries store their flush deadline in
     wall-clock milliseconds. After a step back of 1 hour, samples and series that were waiting
     stay unsearchable and are not written to disk. A read 30 s later misses acknowledged
     samples with both nodes up. A crash of both nodes 40 s after the step loses 360
     acknowledged samples; the same crash without the step loses 60, the last seconds that the
     documentation names (`regressions/samples-held-after-clock-step.schedule`).
  3. **A backward clock step makes vminsert keep one copy of a batch, so one node's power loss
     later loses acknowledged samples.** The connection between vminsert and vmstorage checks
     its deadline against a cached clock that lags behind by up to a second. Right after a step
     back every write to both nodes fails with `i/o timeout`, vminsert drops both connections,
     and the batch it sends next gets one copy. vmstorage1 still holds it in memory (finding 2),
     and its power loss 40 s later loses 3 acknowledged samples for good, while vmstorage2 ran
     the whole time. The step alone and the
     power loss alone lose nothing (`regressions/one-copy-after-clock-step.schedule`).
- **What held?** Without clock steps, losing one storage node lost no acknowledged sample. That
  held under kills, pauses, partitions, one-way loss, and one node's power loss, also with the
  strict form that turns off the writeback. vmselect returned each sample once and with its
  value, from both nodes and from one: deduplication across the replicas worked in every run,
  in the export and in MetricsQL's `count_over_time` and `sum_over_time`.
- **What is known or documented?** Section "Known and documented behaviour" has six cases:
  - A crash of both nodes loses the last seconds.
  - A node that comes back after a crash stays without its lost samples. A later loss of the
    other node then misses them: the cluster has no repair (issue #2613).
  - vminsert answers before vmstorage has the data (issue #8677).
  - A new series is invisible for up to 10 s to a query whose tag filter vmstorage cached
    (issue #5555).
  - vmstorage drops a sample more than 2 days ahead of its clock after vminsert answered `204`.
  - A damaged file stops vmstorage at its start, or returns another value.
- **What did hydo lack?** Section "What hydo lacked": the 64 KiB console of hvx, a check whose
  last line is long, and the shrinks of detected corruption.

## The version

v1.156.0 (2026-11-09) was GitHub's latest release when the onboarding started (2026-11-18). The
cluster sources were read at the tag `v1.156.0-cluster` (commit `02bcbd7e`). The images are
`victoriametrics/vmstorage`, `vminsert` and `vmselect` at the tag `v1.156.0-cluster` on Docker
Hub (Alpine 3.24.3 with one static program each):

| image | index digest | amd64 | arm64 |
|---|---|---|---|
| vmstorage | `sha256:069da202…` | `sha256:0d20231d…` | `sha256:ad337ea9…` |
| vminsert | `sha256:3f3fa42a…` | `sha256:81989baa…` | `sha256:222063ac…` |
| vmselect | `sha256:3ac723ba…` | `sha256:8c7b1015…` | `sha256:d2df9d90…` |

`hydo/fetch-images.sh` fetches them by digest, and v1.141.0 and v1.140.0 for finding 1. The code
of the three findings is the same on `master` at `cbf98ebd` (2026-11-18).

## The promises

The cluster documentation (`docs/victoriametrics/Cluster-VictoriaMetrics.md`, section
"Replication and data safety") says:

- `-replicationFactor=N` on vminsert "instructs vminsert to store N copies for every ingested
  sample on N distinct vmstorage nodes. This guarantees that all the stored data remains
  available for querying if up to N-1 vmstorage nodes are unavailable."
- "The cluster must contain at least 2*N-1 vmstorage nodes, where N is replication factor, in
  order to maintain the given replication factor for newly ingested data when N-1 of storage
  nodes are unavailable." With
  two nodes, a sample written while one node is down has one copy.
- vmselect needs `-dedup.minScrapeInterval=1ms` "so they could de-duplicate replicated samples
  obtained from distinct vmstorage nodes during querying". With `-replicationFactor=2`, vmselect
  gives a full answer while one node is down.

The cluster shares its storage with the single-node version, whose documentation (`README.md`,
"Storage" and "Troubleshooting") limits durability. It says that "the last few seconds of
inserted data may be lost on unclean shutdown", for example `kill -9`. It also says that data
"becomes available for querying in a few seconds after inserting." This example does not count
that loss on one node. With two replicas it matters only when both crash.

Two promises are under test:

1. Losing one storage node loses no acknowledged sample: with both nodes back, and with one of
   them down, vmselect returns every acknowledged sample.
2. A query returns each sample once, with the right value.

## The project

`docker-compose.yaml` starts the cluster as the documentation does: `vmstorage` with
`-storageDataPath=/storage`, `vminsert` with `-storageNode=vmstorage1:8400,vmstorage2:8400
-replicationFactor=2`, and `vmselect` with `-replicationFactor=2 -dedup.minScrapeInterval=1ms`.
Each component gets a share of the guest's memory (`-memory.allowedBytes`). vmstorage keeps the
same dedup interval, as the section "Deduplication" advises, and 100 years of data
(`-retentionPeriod=100y`), so that a forward clock step does not delete the samples under test.
`hydo compose` imported it. The changes from the import:

- Each vmstorage node's `/storage` is a file system of its own (`storage:`), the target of `G=`
  and `L=`. The two nodes form the group `storage`.
- The readiness checks ask each component's `/health`.
- The release is a parameter, `vm` (`--set vm=v1.140.0`), and the test client `vmc` is in the
  image (`../build.sh`).
- `explore-nocorrupt.yaml` is the same manifest without `corrupt` in `faults:`, for explorations
  without `G=`.

## The workload

The client `vmc` (`client/main.go`, Go's standard library only) has one command per operation:

| command | operation |
|---|---|
| `first_settle` | waits until vminsert reaches both nodes and vmselect answers an export |
| `parallel_driver_write` | one `POST /insert/0/prometheus/api/v1/import`: 2 new series `hydo_s{w=<entry>,k=<0..1>,t=<t0>}` with 3 samples each, at the client's wall clock `t0`, `t0+1` and `t0+2` ms |
| `anytime_read` | an export of every sample during the faults; exit 100 when a sample acknowledged 30 s or more before is missing, returned twice or has another value, while vmselect answered |

A sample's value is a function of its series and its timestamp (`valueOf`), an integer below
1e9 that VictoriaMetrics stores exactly. A `2xx` answer is acknowledged: the client writes
`w~<entry>~<t0>~<uptime>~<boot>` to `$HYDO_ACKED` and `ack` to `$HYDO_OUTCOMES`. A `4xx` answer
is failed, and a `5xx` or no answer is unknown. Every write is also a send of the messages
history `vm`: the sample `m<entry>.<j>`, where `j` numbers the write's 6 samples. Two writes per
round of 500 ms in 60 steps make about 120 writes and 700 samples a run.

## The rules

| rule | promise | where |
|---|---|---|
| `vm` (a `type: messages` check) | `/api/v1/export` from 1970 to 2100 returns every acknowledged sample (`no-loss`). It returns each once (`duplicates: forbidden`: a second copy carries another system id). It returns each with its value (`no-phantom`: a sample with another value is a delivery that no write sent) | `hydo.yaml`, the drain `vmc drain` |
| `query` | every acknowledged series has its 3 samples and their sum in `count_over_time` and `sum_over_time` over a window from an hour before the first write to a minute after the last | `checks/final_query` |
| `compose.commands` | the anytime read: promise 1 with one node down, and promise 2 during the faults | `workload/anytime_read` |

The drain waits up to 120 s for both nodes, and until 15 s after the last acknowledged write,
because of the tag filter cache (section "Known and documented behaviour"). Then it calls
`/internal/force_flush` on both nodes and reads until three exports 3 s apart agree. The flush
makes the samples of finding 2 searchable, so only the anytime read sees them missing. The query
asks with `latency_offset=1ms`, because vmselect hides the last 30 s of samples by default
(issue #5555).

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint per image. On the final image
(`sha256(explore-ob-victoriametrics-disk.img)=da0e7ab0…`) it gives `9386f5eab3082c58` twice;
with the rounds of 1 s of the regressions (`W=740c6cfa,p2,q2,r1000`) it gives
`cb0237a41b672c7f` twice. A run takes about 5 s in the engine and 50 s of wall time on hvx.

The three regressions in `hydo/regressions/` are the findings' minimal schedules; each expects
a violation. Each uses rounds of 1 s, so that the run lasts long enough for the watchers that
run every minute. `hydo repro --regressions` on the final image gives all three as expected:

| regression | fingerprint |
|---|---|
| `month-deleted-after-clock-step` | `ed7eab9cd7aa0756` |
| `samples-held-after-clock-step` | `295f21500237d2ee` |
| `one-copy-after-clock-step` | `f52ca18bd47eb06a` |

Each gave the same fingerprint in a second `hydo run`. hydo warns that their `W=` is in the seed
form, so the composition comes from the current command list; a change of the commands changes
the runs.

## What was found

### 1. New: a backward clock step deletes the current month's data on every vmstorage node

**Label: a bug of the project, new as far as the search found (a clock effect that breaks a
promise). Present from v1.141.0; v1.140.0 keeps the data.**

Blind, seeds 4, 7 and 18 of the first exploration drew a step back of 30 days
(`T=<step>:-2592000`), and each lost every sample acknowledged before the step. Seed 5 drew it
too and stopped earlier, at a damaged file. The directed
runs separate it:

| schedule | what happens |
|---|---|
| `disk=none \| run T=20:-1728000 W=740c6cfa,p2,q2,r1000` (20 days back) | all 162 samples acknowledged before the step lost; the same fingerprint twice |
| `T=20:-1555200` (18 days back) | OK: the stepped clock, 2026-10-31, is 2 days before the partition starts |
| `T=20:-2592000` with rounds of 500 ms | OK: the run ends before the watcher's first tick |
| the 20-day step on v1.141.0 | the same loss |
| the 20-day step on v1.140.0 | the partition stays on disk |

The cause is `lib/storage/table.go` at the tag:

1. `retentionWatcher` (line 446) runs every minute. It reads the wall clock (`fasttime`) and
   sets `maxTimestamp := nowMsecs + tb.s.futureRetentionMsecs` (line 459).
2. A partition with `ptw.pt.tr.MinTimestamp > maxTimestamp` (line 464) is dropped:
   `ptw.scheduleToDrop()` (line 482), and `pt.Drop()` removes its `small`, `big` and `indexdb`
   directories (`partition.go` line 239).
3. The partitions are per month. The current month's partition starts on its first day, so a
   clock that goes back to more than 2 days before that day deletes the whole month.

An inspection of the run's end (`hydo inspect exec`) shows `/data/vol/s1/data/small` and
`indexdb` with `2026_10` only on both nodes; without the step `2026_11` holds every sample. On
v1.140.0 after the same step both nodes still have `2026_11`.

The flag's help says: "Data with timestamps bigger than now+futureRetention is automatically
deleted." The pull request that added it (#10718, v1.141.0) says that a restart with a smaller
`-futureRetention` deletes future partitions. Neither names samples whose own timestamps were
the present when they were written. A clock that goes back by days happens. Examples are a host
that boots with a wrong hardware clock before NTP corrects it, a restored virtual machine, and
a mistaken `date -s`. The deletion takes one tick of a minute, and it cannot be undone
when the clock is right again. Two replicas do not help, because both nodes read the same
time. Issue #248 (2019) was a different case of the same consequence: a drift at a month's
start rotated the index twice and lost November.

### 2. New: after a backward clock step, vmstorage holds new samples in memory for the step's length

**Label: a bug of the project, new as far as the search found (a clock effect that breaks a
documented limit).**

The explorations reached it in many schedules with a step back (`T=<step>:-30`, `-300`,
`-3600`, `-86400`), as anytime reads that missed acknowledged samples while both nodes were up.
The directed runs on the final image:

| schedule (`W=740c6cfa,p2,q2,r1000`) | result |
|---|---|
| `disk=none \| run` | OK |
| `T=10:-3600` | the read misses 12 samples acknowledged 30 s or more before, both nodes healthy; the drain, which calls `/internal/force_flush`, finds them |
| `T=10:-30` | the same: 12 samples missed |
| `T=10:3600` (forward) | OK |
| `K=storage.all@50+2` (both nodes killed) | 60 acknowledged samples lost: the last seconds |
| `T=10:-3600 K=storage.all@50+2` | 360 acknowledged samples lost, written up to 40 s before the kill |

The cause is the flush deadline of the in-memory buffers:

1. `lib/storage/raw_row.go`: when a shard of new rows gets its first row, `updateFlushDeadline`
   stores `time.Now().Add(pendingRowsFlushInterval).UnixMilli()` (lines 248 and 325), a
   wall-clock millisecond 2 s ahead.
2. `flush` and `appendRawRowsToFlush` (lines 251 and 328) compare it with
   `time.Now().UnixMilli()` and flush only when the deadline has passed. Later rows join the
   shard without a new deadline.
3. After a step back of 1 hour the deadline is an hour ahead. The shard's rows are neither made
   searchable nor written to disk until the wall clock reaches it, or until the shard holds
   8 MiB of rows. A crash in that hour loses them.
4. `lib/mergeset/table.go` does the same for the pending items of the index (lines 208, 278,
   811 and 828), so a new series can stay invisible too.

`time.Now().UnixMilli()` drops Go's monotonic reading. The in-memory parts already compare
`time.Time` values (`partition.go` line 774), which a step does not move.

### 3. New: a backward clock step makes vminsert keep one copy, and one node's power loss then loses acknowledged samples

**Label: a bug of the project, new as far as the search found (a clock effect that breaks a
promise).**

| schedule (`W=740c6cfa,p2,q2,r1000`) | result |
|---|---|
| `L=vmstorage1@50+2` | OK |
| `T=10:-3600` | no sample lost at the end (finding 2's read only) |
| `T=10:-3600 L=vmstorage1@50+2` | 3 acknowledged samples lost for good: the series `k=1` of write 16 |

vminsert's log at the step:

```text
2026-11-17T23:00:12.106Z warn .../netstorage.go:350 cannot send 198 bytes with 3 rows to -storageNode="vmstorage1:8400": cannot write rpcName "writeRows_v1": cannot write data size 12: i/o timeout; closing the connection to storageNode and re-routing this data to healthy storage nodes
2026-11-17T23:00:12.106Z warn .../netstorage.go:350 cannot send 198 bytes with 3 rows to -storageNode="vmstorage2:8400": cannot write rpcName "writeRows_v1": cannot write data size 12: i/o timeout; ...
2026-11-17T23:00:12.941Z warn .../netstorage.go:230 cannot make a copy #2 out of 2 copies according to -replicationFactor=2 for 198 bytes with 3 rows, since a part of storage nodes is temporarily unavailable.
```

vmstorage2 never got those 3 rows: it added 495 rows of 165 series, one series fewer than the
writes. vmstorage1 still held them in memory 40 s later, because of finding 2, and lost them at
its power loss. Without finding 2 the batch would be on vmstorage1's disk, in one copy, until
that node's disk is lost. The cause of the one copy:

1. `lib/vminsertapi/client.go` line 31: `SendRPCRequestToConn` sets the write deadline to
   `time.Now().Add(timeout)`, 60 s ahead on the stepped clock.
2. `lib/handshake/buffered_conn.go`: `Write` and `Read` call
   `deadlineExceeded(bc.writeDeadline, fasttime.UnixTimestamp())`, which compares the deadline
   with `fasttime`, a copy of the wall clock that a goroutine refreshes once a second.
3. For up to a second after a step back, `fasttime` still holds the old time, an hour after the
   new deadline, so every write fails with `os.ErrDeadlineExceeded` (`i/o timeout`). A step
   forward of more than 60 s does the same to a request that is in flight.
4. vminsert marks both nodes broken (`netstorage.go` line 350), and `sendBufToReplicasNonblocking`
   accepts a batch with one copy (line 230). Nothing makes the second copy later (issue #2613).

The documentation's guarantee is for "the stored data" while up to N-1 nodes are unavailable.
Here no node was unavailable when the batch was written; the clock step alone removed its
second copy.

### Known and documented behaviour

| failure | label | where |
|---|---|---|
| a kill or a power loss of both nodes loses the samples of the last seconds (60 samples in 5 to 10 s) | documented: "The last few seconds of inserted data may be lost on unclean shutdown" | e2 seeds 33, 36, 38, 39, 55, 61, 62, 65 and 67; e1 seed 12 and e2 seeds 36 and 39 also without their clock steps |
| a node that lost its unflushed samples at a power loss, and the other node down later: vmselect answers as complete and misses them (e1 seed 29: `L=vmstorage1@29+2 K=vmstorage2@47+12`) | known: the cluster does not repair a replica, issue #2613; the guarantee counts only the data a node has | e1 seed 29, e2 seed 63 |
| a write while one node is down gets one copy, lost with that node | documented: the rule of `2*N-1` nodes | e2 seed 63 |
| vminsert answers `204` before any vmstorage node has the samples | known: issue #8677 (open, no answer) | no run of `K=vminsert` lost a sample |
| a new series is invisible to a query whose tag filter vmstorage cached before, for up to 10 s, also after `/internal/force_flush` | known: issue #5555 (a comment reports it after `force_flush`) and pull request #11116 (the 10 s interval of `lib/mergeset`) | the first clean runs; the drain waits 15 s |
| a sample stamped more than 2 days after vmstorage's clock is dropped (`vm_rows_ignored_total{reason="big_timestamp"}`), and vminsert answered `204` | documented: "VictoriaMetrics doesn't accept samples with timestamps bigger than now+2d"; vmstorage acknowledges before it processes (the blog post on vminsert) | e2 seed 31 and e1 seed 25: the write at a forward step of a year, while `fasttime` still held the old year |
| a damaged file (`G=`) stops vmstorage at its start | the environment: detected corruption | 11 of e1's 30 schedules |
| a zeroed range of a data file returns another value from vmselect, also with the intact replica up | the environment: vmstorage keeps no checksum of a data block; the documentation says that replication "doesn't save from disaster" | e1 seed 27 |

## The duplicate searches

Each search ran on 2026-11-18 over VictoriaMetrics/VictoriaMetrics's issues and pull requests,
open and closed, and its discussions, with the phrases below. The CHANGELOG from v1.141.0 to
v1.156.0 was read for `futureRetention`, `clock` and `flush`.

| finding | phrases | what they found |
|---|---|---|
| 1, the month deleted | `futureRetention`, `future partition`, `future partitions deleted`, `partition deleted`, `partition dropped`, `drop partitions`, `month partition`, `retentionWatcher`, `retention watcher`, `scheduleToDrop`, `getMinMaxTimestampsForAge`, `clock jump`, `clock backwards`, `clock went back`, `clock skew`, `system clock`, `system time`, `time changed`, `time drift`, `time sync`, `NTP`, `chrony`, `RTC`, `date set back`, `time in the past`, `wrong system time`, `data deleted after time change`, `data disappeared`, `current month data deleted`, `lost a month of data`, `data lost after reboot` | no report. #10718 (merged, v1.141.0) added the deletion of future partitions; #10881 and #11752 adjust it. #248 (2019, fixed) lost a month's index after a drift at a month's start, through another path. #10076 (closed) is an index rotation after a time jump in v1.87. #9025 (closed) asks what a clock change does, and the answer names duplicate or missing points only |
| 2, samples held in memory | `flushDeadlineMs`, `pendingRowsFlushInterval`, `flush deadline`, `rawRows flush`, `raw rows`, `pending rows`, `inmemoryDataFlushInterval`, `monotonic`, `time.Now`, `data not visible`, `not visible after` | no report. #11672 and #11794 change the raw rows' buffers, #11703 asks about a long `-inmemoryDataFlushInterval`, #5555 is the 30 s latency offset |
| 3, one copy after a step | `fasttime`, `fasttime clock`, `deadlineExceeded`, `BufferedConn`, `i/o timeout clock`, `i/o timeout vminsert`, `cannot write rpcName`, `cannot make a copy`, `partial replication`, `replicationFactor lost`, `replication data loss`, `vminsert data loss`, `vminsert crash` | no report of the cause. #2613 (open) reports the one copy while a node is down, and #8677 (open) the answer before the send |

## The explorations

All runs used hvx on the bare-metal machine, with this tree's `hydo` over the 0.4.0 Linux
release, with workers of 2,048 MiB.

| exploration | image | options | schedules | failures |
|---|---|---|---|---|
| e1 | the first image, whose manifest named the release without the parameter | `--seeds 1-30 --strategy process --new-faults --storage-faults --cluster-faults` (stopped during its shrinks) | 30 | 11 errors of detected corruption. 10 violations, each with a clock step: 3 of finding 1 (seeds 4, 7, 18), and the others findings 2 and 3 with kills or the documented loss of both nodes |
| e2 | the parameterized image, `explore-nocorrupt.yaml` | `--seeds 31-70 --strategy process --new-faults --storage-faults --cluster-faults --no-shrink` | 40 | 20: 16 with a clock step, 3 with a fault of both nodes, 1 with two single-node faults in sequence (seed 63) |
| e3 | the final image | `--seeds 71-110 --strategy swarm --new-faults --storage-faults --cluster-faults --no-shrink` | 40 | 24, in 387 s with one worker: 10 errors of detected corruption, 5 violations with `G=`, and 9 without it. Of the 9, 4 had a clock step, 3 a fault of both nodes, and 2 one node down and the other's power loss within a few seconds (seeds 92 and 96). Its 15 new error kinds were vmstorage's reports of damaged files and of dropped connections |

Of e2's 40 schedules, the 20 without a failure are 8 without a clock step (2 of them with
faults of both nodes) and 12 with steps that met no write in the windows of findings 2 and 3.
Without a clock step, e2 lost samples only when both nodes failed within a few seconds of each
other: both together, or one down and the other crashing (seed 63). e2 took 273 s for 40
schedules, 531 runs an hour.

## What hydo lacked

1. **hvx's console holds 64 KiB.** The first client named each sample by its timestamp and
   wrote 12 samples per write. The history records passed 64 KiB, the drain's records were cut
   off, and the messages check called every sample lost. The client now names a sample by its
   write and its number, 6 per write, about 46 KiB a run. The guide's hazard topic names the
   limit; nothing warned during the run that the history was cut.
2. **A check's long last line splits one finding into signatures.** The read's first violation
   line ended at different lengths, so the explorer counted three signatures of one failure.
   The last line is now short, with the numbers on the line before it.
3. **The explorer shrinks every error of detected corruption.** In e1 the shrinks of
   `restart failed (its files were corrupted)` took most of an hour. The exploration was stopped,
   and e2 left `G=` out through a copy of the manifest (open item 140 has the same gap).
4. **One clock step per schedule.** The last `T=` wins (open item 65). So no schedule steps the
   clock back and then forward again, to show that the deleted month stays deleted when the
   clock is right; the code and the files show it.
5. **The clock audit does not see `fasttime`.** `hydo rules scan --kind clock` listed 21
   functions, the first two in vmalert and vmselect; none of the three findings' places. They
   read a cached wall clock (`fasttime.UnixTimestamp()`) or `time.Now().UnixMilli()`, which the
   audit does not follow as a deadline.

## Time

The onboarding started at 19:05 UTC on 2026-11-18. Two other onboardings shared the bare-metal
machine, and its free memory fell to 2 GB at times.

| phase | wall time |
|---|---|
| 1. Scouting: the cluster documentation, vminsert's replication code, the images | 30 minutes |
| 2. Setup to the first clean run, twice: the import, the client, the console limit, the tag filter cache | 70 minutes |
| 3. Directed runs and e1 | 50 minutes |
| 4. Judging: the three causes in the code, the versions, e2, the separations | 100 minutes |
| 5. The duplicate searches and the write-up | 60 minutes |
