# GreptimeDB in cluster mode under hydo: region failover, migration, and the object store

GreptimeDB (github.com/GreptimeTeam/greptimedb) is a time series database. In cluster mode a
metasrv keeps the cluster's metadata in etcd and assigns each region of a table to one datanode.
The frontends take the queries and the writes and send each row to its region's datanode. A
datanode writes a region's rows to the region's WAL and keeps its data files on object storage.
With the WAL in Kafka (the remote WAL) and the data on shared storage, the metasrv moves the
regions of a failed datanode to another datanode: region failover. This example runs that
production shape in one hvx guest and looks for bugs that nobody reported.

## The short answers

- **What was found?** One new candidate, not posted: **a short or zeroed copy in a datanode's
  manifest cache stops the datanode at every start.** The datanode reads its local copy of a
  region's manifest delta instead of the intact object on S3, and with the default
  `compress_manifest = false` it does not notice that the copy is short. One region that does not
  open stops the whole datanode (`regressions/manifest-cache-truncated.schedule`). hydo's damage
  of the file found it blind twice; a power loss did not leave a short copy in the runs. The
  other failures were a mistake of the example's DDL check, hydo's restart of a service, the S3
  emulator at a power cut of the guest, and damaged Kafka segments (section "What was found").
- **What held?** These promises held in every run that reached its checks:
  - Every write that a frontend acknowledged was readable afterwards, once and with its value,
    in the deduplicating table and in the append-mode table.
  - So it was through kills, pauses past the failure detector, partitions, and one-way loss of a
    datanode, a frontend, the metasrv, Kafka or etcd, and through region failovers and the
    workload's own region migrations.
  - It held through one node's strict power loss, through damaged SST files, and
    through clock steps from 30 days back to a year forward.
  - It held when the object store failed or delayed the datanodes' writes, reads and heads.
  - Every acknowledged `CREATE`, `ALTER` and `DROP` took effect, and every table that exists
    answers a query on its listed columns.
- **Where GreptimeDB defended itself.** A datanode that a pause took past its lease refuses the
  writes that reach it afterwards, because the lease counts on the monotonic clock (section
  "What held, and why").
- **What hydo lacked?** Section "What hydo lacked": the S3 emulator's objects do not survive a
  power cut of the guest, the error-log scan stops a run of a verbose system on hvx, and the
  clock audit found no place in GreptimeDB's Rust.

## The version

v1.3.1 (2026-11-13) was GitHub's latest release when the onboarding started (2026-11-23); the
newer tags are nightly builds. The sources were read at the tag `v1.3.1` (the binary reports
commit `602fdd3a`). The images:

| image | index digest | amd64 | arm64 |
|---|---|---|---|
| `greptime/greptimedb:v1.3.1` | `sha256:a1018127…` | `sha256:f539f378…` | `sha256:c026fa2b…` |
| `apache/kafka:3.9.1` | `sha256:4ceccc57…` | `sha256:5862db4a…` | `sha256:39bc3b30…` |
| `quay.io/coreos/etcd:v3.6.17` | `sha256:316f5d36…` | `sha256:c521f860…` | `sha256:960ea8d1…` |

Kafka 3.9 is the release line that GreptimeDB's own integration tests run
(`tests-integration/fixtures/docker-compose.yml`). `hydo/fetch-images.sh` fetches the images by
digest. The S3 server is hydo's S3 emulator, rclone 1.76.0, which `hydo build` fetches.

## The promises

The documentation's page "Region Failover" says: "With Kafka WAL (Remote WAL) and shared storage,
Region Failover can recover regions without losing data." The promises under test, from that
page, the page "Region Migration", and the code:

1. A write that a frontend acknowledged is readable after a crash, a pause, a partition, or one
   node's power loss of a datanode, a frontend, the metasrv, Kafka or etcd.
2. Region failover and region migration keep every acknowledged row, and a region is never
   served by two datanodes at once.
3. A query returns each row once, with the value it was written with. For a table in append
   mode GreptimeDB keeps every row it took, so a row that the frontend wrote twice shows twice.
4. A flush or a compaction to object storage never loses a file that a manifest names, also
   when the object store fails or is slow.
5. A clock step does not delete or hide data.
6. DDL statements (`CREATE`, `ALTER`, `DROP`) take effect whole across the cluster.

The promises 2 to 6 are derived from the documentation and the code and are unconfirmed by the
owner.

## The project

`docker-compose.yaml` starts the cluster as GreptimeDB's own `cluster-with-etcd.yaml` does, with
the remote WAL and the object store that region failover needs. The configuration files in
`config/` set what differs from the examples of v1.3.1, each with its reason:

- The metasrv keeps its metadata in etcd, turns region failover on, and starts its failure
  detectors 10 s after it starts (the default is 10 minutes, for a cluster that the operator
  starts). Its WAL is Kafka with 4 topics instead of 64, and it prunes the WAL every 20 s.
- Each datanode writes its regions' WAL to Kafka and its data to the bucket `greptimedb` on S3.
  It turns on the write cache that the documentation recommends for object storage, with small
  caches and buffers, and needs a lease before it opens its regions.
- The frontends keep the defaults, among them their own retries of a failed write to a
  datanode (`[write_retry]`, new in v1.3.0).
- One Kafka 3.9 broker in KRaft mode syncs each record before it answers
  (`log.flush.interval.messages=1`). Production runs three brokers and replicates each record;
  one broker that syncs keeps an acknowledged WAL entry as three replicas would.

`hydo compose` imported the file. The changes from the import (the header of `hydo/hydo.yaml`):

- The S3 server is hydo's S3 emulator (`emulators:` with `use: s3`), so `H=s3.<route>` faults
  reach every request of the datanodes to the object store.
- The datanodes', Kafka's and etcd's data directories are file systems of their own
  (`storage:`), the targets of `L=` and `G=`. The groups are `datanodes`, `frontends` and `meta`
  (etcd and the metasrv).
- `/etc/hosts` names the guest's hostname, `(none)`. Kafka resolves the local hostname at its
  start and stopped with `UnknownHostException: (none)` without it. Docker gives every container
  a line for its own hostname.
- Kafka runs as root, because the image's user `appuser` cannot write the file system of
  `storage:`.
- The readiness checks ask each component's `/health` with the guest's busybox.
- The guest has 3,072 MiB. The services' peaks add up to about 1,360 MiB (`hydo run --memory`):
  Kafka 311, each datanode about 210, each frontend about 205, the metasrv 109, the emulator 73
  and etcd 35.

The memory of this example on the bare-metal machine, which other onboardings shared: hvx keeps
every disk of a VM in the host's memory beside its RAM, about 3.6 GB here, and an explorer
worker took about 8.5 GB of the host. The explorations ran one worker, and at most one
directed run beside it. With two workers the explorer process took 17 GB, and with three
`hydo run`s at once one VM could not get its disk (`HVX_SET_DISK: Out of memory`).

## The workload

The client `gtc` (`client/main.go`, Go's standard library only) speaks GreptimeDB's HTTP SQL API
(`POST /v1/sql`) to the two frontends, the even entries to `frontend0` and the odd ones to
`frontend1`. Its first command creates two tables, each partitioned on `k` into 3 regions, so
that every write reaches regions on both datanodes:

- `hydo_rows`, with GreptimeDB's default deduplication: the last row of a key and a timestamp
  wins.
- `hydo_log`, an append-mode table (`'append_mode' = 'true'`), which keeps every row it takes.
  A row written twice shows twice there.

| command | operation |
|---|---|
| `first_settle` | waits for both datanodes in `information_schema.cluster_info` and both frontends, creates the tables, and waits until both frontends read both |
| `parallel_driver_write` | one `INSERT` of 3 rows `(w, k, t0, v)`, `k` = 0 to 2, into `hydo_rows`, and the same rows into `hydo_log`; `w` is the entry number, `t0` the client's wall clock, and `v` a function of the row |
| `anytime_read` | reads both tables through one frontend; exit 100 when a read that succeeded misses a row acknowledged before it started, returns one twice, or returns another value |
| `anytime_admin` | one `ADMIN flush_table`, `ADMIN compact_table`, or `ADMIN migrate_region` of a region of either table to the other datanode |
| `serial_driver_ddl` | one `CREATE TABLE d<entry>` (two partitions), `ALTER TABLE ... ADD COLUMN`, or `DROP TABLE` of a table that an acknowledged `CREATE` made |

An insert answered with status 200 and 3 affected rows is acknowledged: the client writes
`<table>~<entry>~<t0>~<uptime>~<boot>` to `$HYDO_ACKED`. Any other answer, or none, is unknown,
because a frontend may have written some regions. Each insert is also a send of the messages
history `gt`, on the channel `r` (`hydo_rows`) or `l` (`hydo_log`). Before each insert, ADMIN
function and DDL statement the client writes a console event (`gt.write`, `gt.admin`,
`gt.ddl`), the event of the reactions. 60 rounds of 1 s give about 100 writes, 600 rows, a
dozen ADMIN functions and a dozen DDL statements a run.

## The rules

| rule | promise | where |
|---|---|---|
| `gt` (a `type: messages` check) | a full read of each table returns every acknowledged row (`no-loss`), once (`duplicates: forbidden`: a second copy carries another system id), and with its value (`no-phantom`) | `hydo.yaml`, the drain `gtc drain` |
| `query` | `count(*)` and `sum(v)` of each table, and `count(*)` of each region, agree with a full read | `checks/final_query` |
| `ddl` (eventually, 120 s) | every acknowledged `CREATE` is a table unless a `DROP` followed, every acknowledged `DROP` is gone, every acknowledged column exists, and every table `d<n>` answers a query on all its listed columns | `checks/eventually_ddl` |
| `leaders` (eventually, 120 s) | every region of the two tables has exactly one leader in `information_schema.region_peers` | `checks/eventually_leaders` |
| `compose.commands` | the anytime read: promises 1 to 3 during the faults | `workload/anytime_read` |

A DDL statement without an answer is unknown: the metasrv's procedure goes on after the
frontend gave up. The client records it as `u~<what>~<table>`, and the check allows a table
that such a `DROP` may have removed.

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint on the final image
(`sha256(explore-ob-greptimedb-disk.img)=3fb799a5…`): `2330e7fdc5c43f46` twice. A run takes
about 14 s in the engine and 2 minutes of wall time on hvx. No finding gave a regression, so
`hydo/regressions/` holds only its README.

## What was found

### 1. New: a short copy in the datanode's manifest cache stops the datanode at every start

**Label: a bug of the project, new as far as the search found. The trigger in the runs was
hydo's damage of one cached file; a power loss in 6 directed runs did not leave a short copy.**

A datanode keeps a copy of each region manifest file it reads or writes in its local manifest
cache (`cache/object/manifest/` under its data home; `manifest_cache_size` is 256 MB by default).
Seed 13 of the first exploration (e3) cut one such copy short while the datanode was stopped,
and the datanode never started again. The directed runs on the final image:

| schedule | result |
|---|---|
| `disk=none \| run G=datanode0@11+1:trunc,seed=71f5a076 W=6311ab23,p4,q2,r1000` | `ERROR datanode0 restart failed (its files were corrupted)`, `fp=f9ec66d7caa0cd80` twice |
| the same kill without the damage, `K=datanode0@11+1` | OK |

The damaged file was the cached copy of the first delta of a region of table `d12`. The copy
was cut to 686 bytes; the object on S3 has its 1,076 bytes. The datanode's log:

```text
ERROR datanode::region_server: Failed to open region: 4415226380289(1028, 1) err=0: Failed to open region, at src/mito2/src/worker/handle_open.rs:266:54
1: Failed to ser/de json object, at src/mito2/src/manifest/action.rs:618:36
2: Error("EOF while parsing an object", line: 1, column: 686)
Error: 0: Failed to start datanode, at src/cmd/src/datanode/builder.rs:137:14
1: Failed to open batch regions, at src/datanode/src/region_server.rs:1517:47
```

The cause, at the tag `v1.3.1` (the same code on `main` on 2026-11-23):

1. At its start the cache lists its files and records each one's size on the local disk as the
   entry's size (`src/mito2/src/cache/manifest_cache.rs` lines 190 and 193), so a short file
   becomes an ordinary entry.
2. `fetch_manifests` reads a delta from the cache first (`src/mito2/src/manifest/storage/delta.rs`
   line 211). It has the object's size from the listing of the object store at hand, but does
   not compare it with the cached copy.
3. `get_from_cache_decoded` (`src/mito2/src/manifest/storage/utils.rs` line 38) removes an entry
   and reads the object store again only when decoding fails. With `compress_manifest = false`,
   the default, decoding is the identity, so the short bytes go on to the JSON parser.
4. The parse fails, the region does not open, and one region that fails to open stops the whole
   datanode (`src/datanode/src/region_server.rs` line 1517, `src/cmd/src/datanode/builder.rs`
   line 137). The next start reads the same copy, so a restart policy makes a crash loop.

Pull request #10179 (merged 2026-11-05, in v1.3.1) added the fallback for a cached entry that
does not decode, and its test says why the default is left out: "Truncation only applies to
compression envelopes" (`src/mito2/src/manifest/storage.rs` line 845). A copy written without an
fsync can be short after a crash of the host, and so can a copy whose write a kill interrupted:
`put_file` writes the file in place (`manifest_cache.rs` line 263). Neither happened in the
runs: after `L=datanode0@25+2:strict` the lost copies were missing, not short, and the datanode
read them from S3. The consequence is that one damaged cache file, which the object store can
replace, takes a datanode out of the cluster until someone deletes the cache by hand.

No native run was made: the cause is plain in the code. The regression is
`hydo/regressions/manifest-cache-truncated.schedule`.

### Not bugs

| failure | label | where |
|---|---|---|
| a `DROP TABLE` that got no answer within the client's 30 s took effect later, and the first DDL check called the table missing | a mistake of the rules: the metasrv's procedure goes on after the frontend gives up, so the statement's outcome is unknown. The check now allows it (`u~drop~<table>`) | `H=s3.put_object@10-30:status=503`, with and without `effect=yes`; both pass now |
| `disk=none \| run C=30`: a query of the recovery boot fails with `RangeNotSatisfied` on an SST object | the environment: the S3 emulator loses objects that it acknowledged at a power cut of the guest (section "What hydo lacked") | the directed run `cut30` |
| a datanode does not start while Kafka is down or loses 84% of its packets: `Failed to build a Kafka client ... ConnectionRefused` | hydo's restart, which ignores the service's restart policy (open item 111); a datanode under Docker or Kubernetes is started again | e3 seeds 16 and 17 |
| a frontend does not start while the metasrv is down | the same | e4 seed 36 |
| Kafka does not start after a damaged log segment | the environment: detected corruption | e4 seeds 22 and 42 |

## The duplicate searches

The searches ran on 2026-11-23 over GreptimeTeam/greptimedb's issues and pull requests, open
and closed (`gh search issues --include-prs`), with the phrases below. The history of
`manifest/storage/utils.rs`, `delta.rs` and `cache/manifest_cache.rs` on `main` was read too.

| finding | phrases | what they found |
|---|---|---|
| 1, the short cached manifest | `manifest cache`, `manifest cache truncated`, `manifest cache corrupted`, `cached manifest`, `EOF while parsing a string`, `Failed to ser/de json object`, `Failed to open region manifest`, `manifest_cache_size`, `datanode fails to start manifest`, `datanode cannot start after power`, `power loss datanode`, `local cache corrupted`, `write cache corrupted`, `cache/object/manifest`, `get_from_cache_decoded`, `compress_manifest`, `datanode restart failed open region`, `Failed to open batch regions`, `crash datanode cache`, `fsync cache` | no report. #10168 (closed) is the nearest: a compressed checkpoint in the cache failed its checksum at a restart. Its fix, #10179 (merged 2026-11-05, in v1.3.1), refills an entry that does not decode, and leaves out the uncompressed default. #9132 (open) is another datanode that cannot start, from a Kafka record without a key |
| the write retries | `write retry duplicate`, `append mode duplicate rows`, `write_retry` | #9530 (closed) asked for the retries and named the risk: "Transport errors and timeouts can leave the outcome unknown". #9543 (merged, v1.3.0) added them; #10045 (open) proposes a retry queue |

## What held, and why

- **Failover waits out the old leader.** A region migration first tries to downgrade the old
  leader. When the old datanode's heartbeat stream is gone, the metasrv computes how much of the
  old leader's region lease (10 s) is left from the last heartbeat
  (`downgrade_leader_region.rs` lines 273 to 327) and waits for it before it upgrades the
  candidate.
- **The datanode counts its lease on the monotonic clock.** Each region has a leader deadline,
  renewed from the heartbeat replies, on `Instant` (`mito2/src/lease.rs`). A write is refused
  when the deadline has passed before it starts (`RegionLeaseExpired`), and failed when its WAL
  append ends after the deadline (`UnacknowledgedWrite`, `worker/handle_write.rs` line 601). A
  pause of a datanode past its lease therefore cannot write for a region that the metasrv moved.
- **A failed manifest write keeps its files when the write may have reached the store.**
  `may_have_persisted_manifest_update` (`mito2/src/error.rs` line 1444) keeps a flush's and a
  compaction's SSTs after an error of the object store, so an S3 error with `effect=yes` lost no
  file.
- **Two things that the runs did not show.** The frontend retries a write after `Unavailable`,
  `Cancelled` or `DeadlineExceeded` (`client/src/error.rs` line 216), and rskafka sends a produce
  again after its timeout. Each can apply a write twice, which shows in an append-mode table
  after a replay of the WAL. No run showed a duplicate (open item 170). The metasrv compares the
  last heartbeat with its wall clock (`downgrade_leader_region.rs` line 285), so a forward step
  during a failover could end the wait early; the steps of the runs met no failover.

## The explorations

All runs used hvx on the bare-metal machine with the published hydo 0.5.0 Linux release, one
worker of 3,072 MiB.

| exploration | image | options | schedules | failures |
|---|---|---|---|---|
| e1, e2 | earlier images, with `errors: true` | `--seeds 1-20 --strategy process --new-faults --cluster-faults --storage-faults` | 1 each | stopped at seed 2, which waited for its wall limit (section "What hydo lacked") |
| e3 | the final image | `--seeds 1-20 --strategy process --new-faults --cluster-faults --storage-faults --protocol-faults` | 20, in 11 minutes | 3 errors: seed 13 is finding 1; seeds 16 and 17 a datanode that did not start while Kafka was down (hydo's restart) |
| e4 | the final image | `--seeds 21-80 --strategy swarm --new-faults --cluster-faults --storage-faults --protocol-faults --reactive fault --kill-now --react-events gt.write,gt.admin,gt.ddl` | 60, in 26 minutes | 5 errors: seed 23 zeroed the start of another cached delta and stopped the datanode (finding 1); seeds 22, 42 and 67 a damaged Kafka segment; seed 36 a frontend that did not start while the metasrv was down |

The 80 schedules of e3 and e4 drew a clock step in 37, a reaction at a console event in 33 (24
of them a `kill-now-` of a service at the moment of a write, an ADMIN function or a DDL
statement), and faults of the S3 emulator, the groups, and the storage of one node. No schedule
lost, duplicated or changed an acknowledged row, and no DDL statement or region leader check
failed. Every failure was an ERROR of a service that did not start.

The directed runs, about 60 on the image of their time, aimed at the places where the code made
a bug likely:

| what | schedules | result |
|---|---|---|
| a datanode down for 40 s (failover), paused past the failure detector, partitioned from the metasrv | `K=datanode0@10+40`, `P=datanode0@10+25`, `N=datanode0-metasrv@10-40:loss,100%` | OK; the metasrv failed the regions over, and the paused datanode's regions turned to followers when it went on |
| a pause of a datanode for 11 s inside the WAL append of a write, with Kafka slowed by 300 ms, then its kill | `N=kafka@15-30:delay,300ms P=datanode0@20~150:11000 K=datanode0@32+2` and 12 variants | OK; rskafka logged `Request timed out request_name="produce"` and sent the produce again, and no row showed twice |
| lost answers of the metasrv while the leases ran out | `O=metasrv@10-22:resp:loss,100%`, with Kafka slowed | OK; writes failed and nothing was lost |
| Kafka paused, killed, or its power lost; the metasrv killed | `P=kafka@10+12 K=datanodes.all@30+2`, `K=kafka@10+5`, `L=kafka@30+2:strict`, `K=metasrv@10+5` | OK |
| clock steps | `T=10:60`, `T=10:31536000`, `T=10:-300`, `T=10:-2592000`, `P=datanode0@10+6 T=12:30` | OK; the forward step of 60 s started no failover |
| one datanode's power loss and damaged files | `L=datanode0@<15, 25, 45, 55>+2:strict`, `L=datanode0@25+2`, `G=` on SST files and cached manifests | OK, but for the cached manifests of finding 1 |
| S3 errors, drops and slow reads | `H=s3.put_object@10-30:status=503` (also `effect=yes`), `H=s3.put_object@10-20:drop`, `H=s3.read@10-30:status=500,p=50` | OK after the DDL check's fix |
| a power cut of the guest | `C=30` | the emulator's lost object (section "Not bugs") |

## What hydo lacked

1. **The S3 emulator's objects do not survive a power cut of the guest.** `disk=none | run C=30`
   ended in the recovery boot with `RangeNotSatisfied` on an SST object of `hydo_log`: the object
   was shorter than its manifest says. rclone writes an object's file and answers without an
   fsync: its local backend (`backend/local/local.go` of v1.76.0) calls no `Sync`. Real object
   storage keeps what it acknowledged. So `cut` is left out of `faults:`,
   and the strongest durability fault of hvx, the cut at the k-th flush (`cf=`), could not run.
   One service's power loss (`L=`) ran instead. Open item 170.
2. **The error-log scan stops a run of a verbose system on hvx.** With `errors: true`, seed 2 of
   the first exploration logged `hvx: agent lines full: the agent wrote more than 16777216 bytes
   of lines through the host-call port in this boot`, and the run waited for its wall limit. The
   scan reads every service's whole log through the agent and computes the error kinds on the
   host (`hydo-control/src/schedule/errlog.rs`), and Kafka's and GreptimeDB's logs passed the
   16 MiB that hvx keeps per boot. The explorer then ran no other schedule for 15 minutes. The
   example turns the scan off, and the GreptimeDB services log at `warn`. Open item 170.
3. **The clock audit found no place in GreptimeDB.** `hydo rules scan --kind clock` read 1,833
   source files and named 4 functions: two SQL strings of the PostgreSQL election, a SQL parser,
   and a script of the repository's bot. GreptimeDB reads the wall clock through its own
   `common_time::util::current_time_millis()` (`chrono::Utc::now()`), which the audit does not
   follow, and the metasrv's failure detectors, leases and region migration use it
   (`meta-srv/src/region/supervisor.rs` line 908, `discovery/utils.rs` line 112,
   `procedure/region_migration/downgrade_leader_region.rs` line 285). The audit also said that no
   service runs a program of these sources, because the services run the official image. Open
   item 152 has the gap of a project's own clock.
4. **A restarted service's earlier log is not in the run directory.** Kafka's first start failed,
   and the diagnosis quoted only the last start's one line, `All of the log directories are
   already formatted`. The cause, `UnknownHostException: (none)`, was in `/tmp/hydo/kafka.log.old`
   inside the guest, which a `hydo inspect` session found (open item 141).
5. **The guest's hostname is not in an imported `/etc/hosts`.** As for Temporal (open item 112),
   Kafka needed a line for `(none)`.

## Time

The onboarding started at 09:55 UTC on 2026-11-23. Another onboarding and another agent shared
the bare-metal machine, and its free memory fell to 3 GB at times.

| phase | wall time |
|---|---|
| 1. Scouting: the documentation, the configuration examples, and the code of failover, leases, the write retry and the manifest | 15 minutes, and more reading during phase 3 |
| 2. Setup to the first clean run: the import, the configuration, etcd's disk, Kafka's user and hostname | 30 minutes, about 15 of them in image builds |
| 3. Directed runs and explorations, with the two explorations that hung on the error-log scan | 2 hours 10 minutes |
| 4. Judging: the DDL check's mistake, the cached manifest, the retries | inside phase 3 |
| 5. The duplicate searches and the write-up | 40 minutes |
