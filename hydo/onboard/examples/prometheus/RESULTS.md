# Prometheus under hydo: remote write, scrapes, out-of-order samples and snapshots

Prometheus (github.com/prometheus/prometheus) is a monitoring system with a time series
database. A Prometheus agent scrapes targets and forwards every sample to a central server with
remote write. The server keeps the samples in its TSDB: a head in memory with a write-ahead log
(WAL), and blocks on disk. This example runs an exporter, a Prometheus agent, and a central
Prometheus server from the official image in one hvx guest. It hunts for bugs that nobody
reported in what the documentation promises about acknowledged samples, the WAL, compaction,
out-of-order samples, retention, clock steps, and snapshots.

## The short answers

- **What was found?** One new candidate and two known bugs with a new detail, none posted:
  1. **New: a snapshot with the head holds no out-of-order sample of the head.** `POST
     /api/v1/admin/tsdb/snapshot` creates "a snapshot of all current data." The storage
     documentation recommends snapshots for backups. With `out_of_order_time_window` set, every
     out-of-order sample that the server has not yet compacted into a block is missing from the
     snapshot, while queries return it. `DB.Snapshot` writes the head block from the in-order
     chunks only. Every snapshot of every run without faults lacked such samples, up to 207 of
     them (`regressions/snapshot-without-ooo-head.schedule`).
  2. **Known (prometheus#8312, closed), with a new cause: after a backward clock step, every
     scrape gets the timestamp of the last scrape before the step.** It keeps it until the clock
     is there again. The scrape loop aligns a scrape's time to its grid when the difference is at most
     the tolerance, and a negative difference always is. The server then drops each scraped
     sample of an existing series as a duplicate of a timestamp: 81,585 samples in the 4 minutes
     after a step of 5 minutes. Its out-of-order window of 10 minutes covers the step, and the
     bug scrub that closed #8312 named that window as the remedy. With
     `--no-scrape.adjust-timestamps` every scrape keeps its own time
     (`regressions/scrape-times-frozen-after-clock-step.schedule`).
  3. **Known (prometheus#11496, open), with a new trigger: after a backward clock step, remote
     write holds back new samples until the wall clock reaches the time its queue started.** The
     WAL watcher sends only samples newer than its start. Without finding 2's alignment, the
     agent's samples after a step back of 5 minutes reached the server 70 s after the heal.
- **What held?** With the two findings' checks left out, every rule held in 104 blind schedules and 35 directed runs. The faults were kills, pauses, network faults, and clock steps from 30 days back to a year ahead. They were also power cuts at steps and at 30 disk flushes, one service's power loss, damaged files, and kills and cuts at the moment of writes, new blocks, and snapshots. Every acknowledged sample stayed queryable, also the out-of-order ones and those resent as a retry, and every snapshot kept its in-order samples after a power cut.
- **What is documented or by design?** Section "Known and documented behaviour" has four cases:
  - The server's own power loss loses the unsynced head.
  - A step back of more than the out-of-order window stops ingestion.
  - A forward step of a year lets retention delete the older blocks.
  - A damaged block file is shared by the snapshots that hard-link it.
  - Since Milestone 375, with one service's own clock: an agent more than 10 minutes ahead of the
    server loses every sample it scrapes, and a server more than its out-of-order window ahead of
    its senders refuses theirs (section "One node's own clock").
- **What did hydo lack?** Section "What hydo lacked": a check cannot tell which faults its run
  had, every service shared one clock until Milestone 375, and memory on a shared machine.

## The version

Prometheus v3.16.0 (2026-11-05) was GitHub's latest release when the onboarding started
(2026-11-23). The sources were read at the tag `v3.16.0` (commit `e2e28ade`). The agent and the
server run from the image `prom/prometheus:v3.16.0` on Docker Hub: the index digest is
`sha256:e0498764…`, amd64 `sha256:89c1a644…`, arm64 `sha256:c5230064…`. The code of the three
findings is the same on `main` at `f1486b67` (2026-11-23).

## The promises

The documentation says:

- Storage (`docs/storage.md`): "The current block for incoming samples is kept in memory and is
  not fully persisted. It is secured against crashes by a write-ahead log (WAL) that can be
  replayed when the Prometheus server restarts." The WAL syncs a segment when the next one
  starts, which happens at every head compaction (`Head.truncateWAL`), and at a clean shutdown;
  no sync follows a single write.
- Backups (`docs/storage.md`): "Snapshots are recommended for backups. Backups made without
  snapshots run the risk of losing data that was recorded since the last TSDB block was
  created." The snapshot API (`docs/querying/api.md`) creates "a snapshot of all current data."
  Its `skip_head` skips "data that is only present in the head block."
- Out-of-order samples (`docs/configuration/configuration.md`): "An out-of-order/out-of-bounds
  sample is ingested into the TSDB as long as the timestamp of the sample is
  >= TSDB.MaxTime-out_of_order_time_window."
- Retention: blocks are deleted when their newest sample is older than the retention, counted
  from the newest block (`BeyondTimeRetention`); the documentation's flag text says "How long to
  retain samples in storage".
- Remote write: the receiver answers `204` after its appender committed the samples to the WAL
  and the head; a `4xx` answer is final, and a sender retries a `5xx` or no answer.

Under test, each promise in the test's own words:

1. A sample that remote write acknowledged is queryable with its value after kills, pauses,
   network faults, and clock steps. A power cut of the whole guest, or a power loss of the server
   alone, keeps it when it lies in a block that the server listed after the acknowledgement. The
   rest of the head may be lost, because the WAL had no sync for it.
2. Retention deletes only samples older than one day before the head's newest sample.
3. Out-of-order samples within the window are kept like in-order ones.
4. A snapshot holds every sample acknowledged before the snapshot request, also after a power
   cut.
5. A scrape that a read found in the server stays there, by the rule of promise 1.
6. After the heal, the samples that the agent scrapes reach the server.
7. Two scrapes a second apart carry two timestamps.

## The project

`docker-compose.yaml` runs the shape of a central server behind an agent:

- `exporter`: the test client's `promc serve`. Each scrape gets a new series
  `hydo_scrape{n="<k>"}` and the stable series `hydo_scrape_count` with the value `k`, and the
  exporter logs each served scrape with the guest's uptime in `/data/exp/served.log`.
- `agent`: `prometheus --agent`, which scrapes the exporter every second and forwards every
  sample to the server with remote write (`config/agent.yml`, a batch at least every second).
- `prometheus`: the central server with `--web.enable-remote-write-receiver` and
  `--web.enable-admin-api`. It scrapes itself every second, keeps out-of-order samples up to
  10 minutes behind its newest sample, and keeps one day of data (`config/prometheus.yml`).

Three settings differ from production, for a test of minutes:

- `--storage.tsdb.min-block-duration=1m` (a hidden flag "for use in testing"; the default is
  2 h) and `--storage.tsdb.max-block-duration=10m`. So a run of 4 minutes sees about three head
  compactions with their WAL checkpoints, out-of-order compactions, and a compaction of three
  blocks into one.
- `--storage.tsdb.max-block-chunk-segment-size=16MB` (hidden too). The server preallocates the
  default of 512 MB for every block's chunk file. The first clean run stopped every compaction
  with `no space left on device` on the server's file system of 512 MiB.
- The administration API is on, for the snapshots.

`hydo compose` imported the Compose file. The changes from the import:

- The server's and the agent's data directories are file systems of their own (`storage:`), the
  targets of `G=` and `L=`. The agent's unused anonymous volume `/prometheus` is left out.
- The exporter is the client's own program in the guest's root instead of a busybox image.
- The readiness checks ask each Prometheus's `/-/ready`.
- Five build parameters (`hydo.yaml`, `params:`):
  - `ooo_snapshots` (`judge`, or `skip` for the explorations), the rule of finding 1;
  - `scrape_times` (`judge`, or `skip` for the explorations), the check of finding 2;
  - `powerloss` (`no`, or `yes` for the explorations with `--storage-faults`), the rule after a
    power loss of the server alone;
  - `adjust` (the default `--scrape.adjust-timestamps`, or `--no-scrape.adjust-timestamps`),
    finding 2's comparison;
  - `server_config` (`prometheus`, or `prometheus-stale` for the stale-series compaction of the
    second round).

The guest has 2,048 MiB. The engine of one worker holds about 5 GB of the host, with its
snapshots in memory. Another onboarding shared the bare-metal machine, so the explorations ran
with one worker and at most two VMs at a time, below 10 GiB.

## The workload

The client `promc` (`client/main.go`, Go's standard library only; remote write 1.0's protobuf
and snappy are written by hand) has one command per operation:

| command | operation |
|---|---|
| `first_settle` | waits until the server and the agent are ready and the agent's scrapes reach the server |
| `parallel_driver_write` | one remote write of 2 new series `hydo_rw{w=<entry>,k=<0..1>}` with 3 samples each: `k=0` at the client's wall clock `t0`, `t0+1` and `t0+2` ms, in order; `k=1` 60 s earlier, which the server takes as out-of-order samples. Each fourth write is sent again after its `204`, as a sender's retry after a lost answer. After each acknowledgement it records the blocks that the server lists |
| `anytime_read` | a query of every sample during the faults; exit 100 when a sample acknowledged 30 s or more before is missing or has another value, while the server answered. It also records every scrape that the server has |
| `anytime_snapshot` | `POST /api/v1/admin/tsdb/snapshot` with the head, then `promtool tsdb dump` of the snapshot inside the server's image (`hydo-oci prometheus promtool ...`); exit 100 when the snapshot lacks a sample acknowledged before the request |

A sample's value is a function of its series and its timestamp (`valueOf`), an integer below
1e9 that the TSDB stores exactly. A `2xx` answer is acknowledged: the client writes
`w~<entry>~<t0>~<uptime>~<boot>~<server start>~<in order>` to `$HYDO_ACKED`. The server's start
time comes from `/api/v1/status/runtimeinfo`. The last field says whether `t0` was within 25 s of
the head's newest sample: after a backward clock step the in-order series becomes an
out-of-order one. A `4xx` answer is failed, and no answer or a `5xx` after three tries is
unknown. Other records in `$HYDO_ACKED`: `b~...` for each block that the server listed
(`/api/v1/status/tsdb/blocks`), `s~...` for each snapshot, `x~...` for each scrape that a read
found. Two writes per round of 2 s in 120 steps make about 230 writes and 1,400 samples a run.

Each write and snapshot writes an assertion record of kind `sometimes` for reactions:
`prom.write_acked`, `prom.block_seen` and `prom.snapshot_acked`.

## The rules

| rule | promise | where |
|---|---|---|
| `samples` | promises 1 to 3 and 5: every acknowledged sample is queryable with its value (`/api/v1/query` of `hydo_rw[100y]` at 2100). After a power cut, a sample must be there when a block that the server listed after the acknowledgement holds it. The same holds after a power loss of the server in an image built with `powerloss=yes`. An out-of-order block counts only for out-of-order samples acknowledged 5 s before it. A sample older than one day before the head's newest sample may be gone. Every scrape that a read found must be there by the same rule | `checks/final_samples` |
| `snapshots` | promise 4: every snapshot still holds every sample acknowledged before it, also in the recovery boot after a power cut | `checks/final_snapshots` |
| `scrape` | promise 6: of the scrapes the exporter served 10 to 45 s ago, at least one is in the server. Not judged while the clock is more than the out-of-order window behind the head's newest sample, because the server then refuses every new sample by design | `checks/eventually_scrape`, `within=120 after=heal` |
| `times` | promise 7: two scrapes that the exporter served at least 900 ms apart carry two timestamps in the server | `checks/final_times` |
| `compose.commands` | the anytime read and the snapshot's own check | `workload/anytime_read`, `workload/anytime_snapshot` |

Error logs are findings (`errors: true`, `level=ERROR`); no run of the final images logged one.

## Without faults, and the regressions

`disk=none | run` passes on the exploration image (`--set ooo_snapshots=skip --set
scrape_times=skip`) with one fingerprint over two runs. A run takes about 25 s in the engine and
40 s of wall time on hvx. On the default image the same schedule fails with finding 1, with
fingerprint `a5a53e35802ad822` twice.

The two regressions in `hydo/regressions/` expect a violation on the default image:

| regression | finding |
|---|---|
| `snapshot-without-ooo-head` | 1: `disk=none \| run`, every snapshot lacks the out-of-order head |
| `scrape-times-frozen-after-clock-step` | 2: `disk=none \| run T=20:-300`, 199 of 240 pairs of scrapes share one timestamp; the verdict line names finding 1 first, and `result.json` has the finding `times` |

Both regressions gave their violation in directed runs, with one fingerprint over two runs each (`a5a53e35802ad822` and `bd59337c12da9aa9`). Those runs used the image before the client's last changes; `hydo repro --regressions` on the final image was not run.

## What was found

### 1. New: a snapshot with the head holds no out-of-order sample of the head

**Label: a bug of the project, new as far as the search found. No fault is needed.**

Every clean run of the default image fails `snapshots` and the snapshot command's own check:

| schedule | result |
|---|---|
| `disk=none \| run` (`ooo_snapshots=judge`, the default) | VIOLATION: the first snapshot lacks 6 samples of 2 writes, later ones up to 207 samples of 69 writes, every one an out-of-order sample; fingerprint `a5a53e35802ad822` twice |
| the same with `--set ooo_snapshots=skip` | OK: every in-order sample, and every out-of-order sample of an out-of-order block listed before the snapshot, is in it |

The missing samples are the series `k=1` of each write, 60 s behind the head's newest sample, and
after a backward clock step also the series `k=0`. Queries return all of them; the snapshot
lacks those that the server has not yet compacted into an out-of-order block. The count falls
after each head compaction (from 207 to 12) and grows again.

The cause, at the tag `v3.16.0`:

1. `DB.Snapshot` (`tsdb/db.go` line 2586) hard-links the blocks and then writes the head as one
   more block: `head := NewRangeHead(db.head, mint, maxt)` and `db.compactor.Write(dir, head,
   mint, maxt+1, nil)` (lines 2611 to 2616).
2. A `RangeHead`'s index lists only the in-order chunks of each series: `headIndexReader.Series`
   (`tsdb/head_read.go` line 197) calls `appendSeriesChunks` (line 363), which walks
   `s.mmappedChunks` and `s.headChunks` and never the out-of-order chunks in `s.ooo`.
3. The querier knows this: `DB.Querier` wraps the same `RangeHead` with
   `NewHeadAndOOOQuerier` when the range overlaps the out-of-order head (`tsdb/db.go` lines 2651
   and 2681 to 2684). `Snapshot` has no such step, and no call of `compactOOOHead`.
4. The out-of-order head is compacted only after a compaction of the in-order head (`DB.Compact`,
   line 1638 to 1642), every 2 hours with the default block length. So a snapshot of a server
   with `out_of_order_time_window` lacks up to about 3 hours of its out-of-order samples. They are
   only in `wbl/`, which the storage documentation says to leave out of a backup.

The documentation recommends snapshots for backups. `docs/storage.md` says: "Backups made without
snapshots run the risk of losing data that was recorded since the last TSDB block was created."
The API creates "a snapshot of all current data." The code is the same on `main` at
`f1486b67`. `promtool tsdb dump` reads the snapshot, after an empty `wal` folder is made in it:
the tool refuses a directory without one (open pull request #20416). No native run was made: the
cause is plain in the code.

### 2. Known: after a backward clock step, every scrape gets one timestamp until the clock is there again

**Label: a known bug, prometheus#8312, with a cause that the issue does not name and a remedy
that does not work. A bug scrub closed the issue with the words "this behaviour still exists."
A clock effect that breaks a promise.**

| schedule | result |
|---|---|
| `disk=none \| run` | OK: each of 240 pairs of scrapes a second apart has two timestamps |
| `disk=none \| run T=20:-300` (5 minutes back) | VIOLATION `times`: 199 of 240 pairs carry one timestamp, `1795392052071` from scrape 1042 on. The server dropped 81,585 of its own scraped samples as duplicates of a timestamp, the agent 200 samples as out of order. Fingerprint `bd59337c12da9aa9` twice |
| `T=20:-540` | the same 199 of 240 pairs |
| `T=20:-300` with `--no-scrape.adjust-timestamps` on both | no pair shares a timestamp |

The cause is the scrape loop's alignment, `scrapeLoop.run` in `scrape/scrape.go` at the tag:

1. Line 1436 takes `alignedScrapeTime := time.Now().Round(0)`, a wall-clock reading. Each
   iteration takes `scrapeTime := time.Now().Round(0)` (line 1450).
2. Lines 1456 to 1458 advance `alignedScrapeTime` by the interval while `scrapeTime` is an
   interval or more ahead of it.
3. Lines 1460 to 1462: `if scrapeTime.Sub(alignedScrapeTime) <= tolerance { scrapeTime =
   alignedScrapeTime }`. The tolerance is at most 2 ms, but the test has no lower bound. After a
   step back the difference is negative, and the test holds at every scrape. So every scrape
   takes the last aligned time before the step, until the wall clock passes it.
4. The samples of a series that exists already are then duplicates of a timestamp with other
   values. The head refuses them (`memSeries.appendable`, `tsdb/head_append.go` lines 706 to
   716), also with an out-of-order window, so the window does not help.

In #8312 a maintainer wrote that the scrape takes the actual time when it is more than 2 ms
before or after `alignedScrapeTime`. Line 1460 does it only after. The bug scrub closed the issue
with: "It should now be possible to recover more cleanly by turning on Out-Of-Order handling with
a sufficient window." The third run shows that the out-of-order window of 10 minutes kept none
of the server's own samples after a step of 5 minutes. After a step back of more than 10
minutes, the agent's frozen timestamps are also more than 10 minutes ahead of the server's clock.
The receiver then refuses the whole batch as too far in the future (the known error kinds of
`hydo/known-errors.yaml`). The code is the same on `main` (lines 2151 to 2175). Open issue
#20524 and pull request #20525 change the anchor's moment, not this test. The clock audit named
this function second (`hydo rules scan --kind clock`, `scrape/scrape.go:1456`).

### 3. Known: after a backward clock step, remote write holds back new samples

**Label: a known bug, prometheus#11496 (open), with a new trigger. A clock effect.**

The WAL watcher of remote write sends only samples whose timestamp is later than the moment it
started: `w.SetStartTime(time.Now())` in `Watcher.loop` (`tsdb/wlog/watcher.go` line 286) and
`if s.T > w.startTimestamp` in `readSegment` (line 560). #11496 reports it for out-of-order
samples. A backward clock step makes every new sample older than that moment, as long as the
step is larger than the queue's age. Finding 2's comparison ran without the alignment. There
`T=20:-300` held the agent's new series back: the check `scrape` held 70 s after the heal,
against 0.03 s in the other runs. With the alignment of finding 2, the frozen timestamps
are later than the start, so they pass the watcher, and finding 2 hides this one.

### Known and documented behaviour

| failure | label | where |
|---|---|---|
| a power loss of the server alone (`L=prometheus@12+3`) lost the samples of the first 12 writes, all acknowledged before it | by design: the WAL syncs a segment only when the next one starts, at a head compaction; `docs/storage.md` says that the head is "not fully persisted." The rule asks only for samples of a block listed before the loss | e3, seed 43, before the rule knew the server's start |
| after a backward clock step of more than the out-of-order window (10 minutes), the server refuses every new sample of the agent. The error is `too old sample`, or `too far in the future` for finding 2's frozen times, and the agent drops the batch | documented: `out_of_order_time_window`; a `400` is final for a sender, and the receiver refuses the whole request (known, prometheus#9214) | the first exploration's seeds 3, 4 and 7, with steps of 1 and 30 days; the check `scrape` now does not judge while the clock is that far behind |
| after a forward clock step of a year, the server's own scrapes a year ahead make retention delete every older block | by design: retention counts from the newest block, not from the wall clock; the rule lets a sample older than a day before the head's newest sample go | e2, two schedules with `T=<step>:31536000`, both OK under the rule |
| a damaged block (`G=prometheus@47+2:flip`, a bit of a block's `index`) makes `promtool tsdb dump` of every later snapshot fail | the environment: a snapshot hard-links the block's files, so it shares the damaged file | e3, seed 45 |
| `promtool tsdb dump` refuses a snapshot directory: `read WAL: find last checkpoint: open .../wal: no such file or directory` | known: open pull request #20416 lets a read-only database open a directory without a WAL | every snapshot; the client makes an empty `wal` folder first |

## The duplicate searches

Each search ran on 2026-11-23 over prometheus/prometheus's issues and pull requests, open and
closed, and its discussions, with the phrases below. The history of `tsdb/db.go` (the last 100
commits) was read for snapshots and out-of-order changes.

| finding | phrases | what they found |
|---|---|---|
| 1, the snapshot without the out-of-order head | `snapshot out-of-order`, `snapshot out of order`, `snapshot ooo`, `snapshot head out of order`, `out of order snapshot`, `snapshot OutOfOrder`, `snapshot API out of order`, `snapshot out_of_order_time_window`, `snapshot withHead`, `db.Snapshot`, `NewRangeHead ooo`, `RangeHead out-of-order`, `out-of-order head snapshot`, `OOO head compaction snapshot`, `snapshot wbl`, `tsdb snapshot missing data`, `snapshot missing samples`, `snapshot data missing`, `snapshot incomplete`, `snapshot not all data`, `snapshot does not include`, `snapshot lost samples`, `backup snapshot missing`, `backup out of order samples`, `admin snapshot`, `skip_head`, `snapshot head block`, `tsdb snapshot head`, `ooo samples missing`, `out-of-order samples missing` | no report. #20416 (open) is the dump of a directory without a WAL; #20183 (open) deletes out-of-order samples through the delete API; #5105 and #5686 (closed) are snapshot errors of 2019. No commit of `tsdb/db.go` touches `Snapshot` |
| 2, the frozen scrape times | `AlignScrapeTimestamps`, `alignedScrapeTime`, `timestamp tolerance`, `scrape timestamp alignment`, `adjust-timestamps`, `adjust timestamps`, `scrape same timestamp`, `scrape timestamp stuck`, `clock jump scrape`, `clock backwards`, `clock moved backwards`, `time moved backwards`, `time went backwards`, `system time changed`, `duplicate sample for timestamp clock`, `duplicate_timestamp`, `NTP`, `date -s`, `clock change out of order window` | #8312 (closed by a bug scrub that believed the behaviour still exists) is the same symptom; a maintainer named `alignedScrapeTime` there. Discussion #10169 and #8243 (closed) are the "out of bounds" after a reboot or a suspend. #20524 and #20525 (open) are another alignment bug, ticks before the anchor. #7976 and #9283 (merged) added the tolerance |
| 3, remote write after a step back | `watcher startTimestamp`, `Done replaying WAL`, `remote write clock`, `remote write skips samples after restart`, `remote write out of order whole batch`, `remote write 400 drops batch`, `out of order sample from remote write`, `remote write duplicate sample for timestamp` | #11496 (open) is the watcher's start time, reported for out-of-order samples. #18485 (open) proposes a restart from a saved segment. #9214 (open) is the whole batch dropped for one bad sample |

## The explorations

All runs used hvx on the bare-metal machine with the 0.5.0 Linux release. Another onboarding ran
on the machine at the same time, with runs of 10 to 11 GB. So each exploration ran with one
worker, and at most two VMs ran at once.

| exploration | image | options | schedules | failures |
|---|---|---|---|---|
| e1 | the first exploration image (without the checks `times` and the scrapes' records) | `--seeds 1-30 --strategy process --new-faults --more-faults`, 2 workers | 24: the host's out-of-memory killer ended an engine, and the exploration stopped | none. 5 power cuts at steps, 4 at a disk flush, 16 kills, 21 pauses, 19 clock steps |
| e2 | the same | `--seeds 1-40 --strategy process --new-faults --more-faults`, 1 worker | 40 | none. 8 power cuts at steps, 7 at a disk flush (`cf=163` to `cf=954`), 24 kills, 34 pauses, 25 network faults, 31 clock steps from 30 days back to a year ahead. Three new error kinds, all of clock steps (`hydo/known-errors.yaml`) |
| e3 | `powerloss=yes` | `--seeds 41-46 --strategy process --new-faults --more-faults --storage-faults` | 6 | seed 45: a flipped bit of a block's `index` (`G=prometheus@47+2:flip`), shared by every later snapshot: the environment. Two earlier starts of e3 failed seed 43 (`L=prometheus@12+3`) by the rules' mistakes, fixed before this one |
| e3b | the same | `--seeds 46-70` with the same options and `--no-shrink` | 25 | seed 58: a zeroed range of a snapshot's `tombstones` file (`G=prometheus@32:zero`): the environment. 9 schedules had `G=`, 10 a power loss `L=` of one service, 6 a power cut at a step |
| e4 | the final exploration image | `--seeds 71-79 --strategy process --new-faults --more-faults --reactive fault --kill-now --react-events prom.write_acked,prom.block_seen,prom.snapshot_acked` | 9: the out-of-memory killer ended its engine | none. Reactions: a `kill-now-prometheus` 1 ms after a snapshot's answer, cuts at and 200 ms after a write's acknowledgement, and stops of the server and the agent at a new block |
| directed | the final exploration image, and `powerloss=yes` for `L=` | power cuts at 19 disk flushes `cf=60` to `cf=1140`; `kill-now-prometheus` 5 to 40 ms after `prom.block_seen` (6 runs); cuts at the same delays (4 runs); `C=40`, `C=80`, `C=110`; `L=prometheus@60+2:strict`, `L=prometheus@90+2:strict`, `L=agent@60+2:strict` | 35 | none |

Two explorations were still running at the hand-off: seeds 80 to 100 of the reactive one, and
seeds 101 to 120 of the stale-series compaction (`server_config=prometheus-stale`). Their
results are not in this page (open item 170).


## What hydo lacked

1. **A check cannot tell which faults its run had.** A power loss of the server alone (`L=`)
   may lose what the WAL had not synced, and a kill must lose nothing. Both restart the server
   in the same boot. The rules read the server's start time from its runtime information and take
   the build parameter `powerloss`, and the explorations with `--storage-faults` use an image of
   their own. Two versions of the rule were wrong on e3's seed 43 (`L=prometheus@12+3`), before the
   start time was in the block and snapshot records.
2. **One clock for every service.** The agent and the server share the guest's clock, so a server
   whose clock alone runs ahead of its senders cannot be tested. That case is #8312's and #10169's
   field report (open item 170, the idea "Clocks per service"). Since Milestone 375 `clock: own`
   tests it (the section "One node's own clock").
3. **Memory on a shared machine.** One hvx worker with a guest of 2,048 MiB held about 5 GB of
   the host. The explorer warned that 4 workers can take 40 GB. The host's out-of-memory killer
   ended an engine of the first exploration beside the other onboarding's runs, and the
   exploration stopped after 24 schedules.
4. **A command's output stays in the guest** (as open item 157). The client copies the lines that
   explain a failure to the console (`say` in `client/main.go`).
5. **`hydo-oci` by its path.** `exec.Command("hydo-oci", ...)` in the Go client failed with
   `executable file not found in $PATH`; `/bin/hydo-oci` works. `hydo guide services` says so now.
6. **A block's chunk files are preallocated at 512 MB.** The first clean run stopped every
   compaction with `no space left on device` on the server's file system of 512 MiB, and the
   only sign was 8 error-log lines. The example sets the hidden flag
   `--storage.tsdb.max-block-chunk-segment-size=16MB`.

## One node's own clock (Milestone 375)

The runs above step the guest's clock, so the agent's and the server's clocks move together
(item 2 of "What hydo lacked"). Milestone 375 gave the server and the agent wall clocks of their
own (`clock: own`, `docs/design/service-clocks.md`) and stepped one of them, while the exporter,
the client and the checks kept the guest's clock. The manifest is `skew/hydo.yaml`:
`hydo/hydo.yaml` with `clock: own` on `prometheus` and `agent` and the name
`prometheus-skew`, built with `--set ooo_snapshots=skip --set scrape_times=skip` as the
explorations' images are. Its folders `workload/` and `checks/` are links to `hydo/`'s, and its
saved regression is in `skew/regressions/`. The runs used hvx on the bare-metal machine, this
tree's `hydo` over the 0.6.0 Linux release's guest files, and the onboarding's client
(`promc-amd64` `efc52a60…`).

### By design: a sender more than 10 minutes ahead of the server loses every sample, and a server more than its out-of-order window ahead of its senders refuses theirs

| schedule | verdict |
|---|---|
| `T=agent@10:300` (the agent 5 minutes ahead) | OK |
| `T=agent@10:660` (11 minutes ahead) | `VIOLATION [scrape]`: none of the 35 scrapes served 10 to 45 s ago is in the server |
| `T=agent@10:900` (15 minutes ahead) | the same |
| `T=prometheus@10:300` (the server 5 minutes ahead) | OK |
| `T=prometheus@10:660` and `T=prometheus@10:900` | OK; `eventually_scrape` says `not judged`, and the server logs `Out of order sample from remote write` for every sample of the agent |
| `T=agent@10:-900` (the agent 15 minutes behind) | OK |
| `T=agent@0:50000ppm`, `T=prometheus@0:50000ppm` | OK |

- **The cause.** The remote-write receiver refuses a sample whose timestamp is more than
  `maxAheadTime`, 10 minutes, ahead of the server's own clock (`storage/remote/write_handler.go`
  line 53 at v3.16.0): `out of bounds: timestamp is too far in the future`. The agent treats the
  answer, a 400, as a non-recoverable error and drops the batch (`queue_manager.go`). So every
  sample that an agent more than 10 minutes ahead scrapes is lost, for as long as its clock is
  ahead. The other way, the server's own scrapes take its newest sample ahead, and a sample older
  than the newest one by more than the out-of-order window (10 minutes in the example) is out of
  bounds, which the example's check calls by design (`not judged`).
- **The label.** By design, a clock effect. The 10 minutes are a fixed limit that guards the
  TSDB against a sender with a wrong clock (prometheus#13573, a sender's clock in 2098, broke a
  TSDB). Discussion prometheus#10169 and #8243 are the field reports of the other way. Five
  minutes either way held.
- **The fingerprints.** The table above ran on the first image of the manifest; the final
  image (`bbb797c9…`, the fingerprint epoch 6) gave:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run` | OK | `c4193436778f8185` |
| `disk=none \| run T=agent@10:300` | OK | `ec36af3d9534d723` |
| `disk=none \| run T=agent@10:900` | `VIOLATION [scrape]` | `53ae7f9900601e58` (two runs) |
| `disk=none \| run T=prometheus@10:900` | OK, `scrape` not judged: the clock is 899 s behind the server's newest sample | `436888e860eea95a` |

  The regression is `skew/regressions/agent-ahead-of-the-server-loses-its-samples.schedule`.

- **The exploration.** `hydo explore --seeds 1-20 --strategy process --new-faults
  --service-clocks` on the first image of this manifest drew a service's own clock in 8 of the 20
  schedules: steps of 1.5 s to 5 minutes and drifts of 200 and 50,000 ppm, alone or with the
  two services apart. Every schedule passed. Its three new error kinds came from steps of the
  guest's clock of 30 days back, as in the explorations above. The drawn steps are at most 5
  minutes, below the 10 minutes of both limits.

## Time

The onboarding started at 09:59 UTC on 2026-11-23.

| phase | wall time |
|---|---|
| 1. Scouting: the storage and API documentation, the WAL, the write handler, the watcher, the snapshot code | 25 minutes |
| 2. Setup to the first clean run: the import, the client, the chunk files' preallocation, `hydo-oci` by its path, `promtool` and the snapshot's `wal` folder | 35 minutes |
| 3. Explorations and directed runs, one or two VMs beside the other onboarding | 2 hours 40 minutes |
| 4. Judging: the three causes in the code, the comparison without the alignment, the rules after the server's own power loss | inside phase 3 |
| 5. The duplicate searches and the write-up, beside the last runs | 1 hour |

