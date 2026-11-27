# Garage under hydo: three nodes, replication factor 3, and what a listing says

Garage (git.deuxfleurs.fr/Deuxfleurs/garage) is an S3-compatible distributed object store
written in Rust. Its nodes keep the metadata in an embedded database and the data blocks in
files, replicate both on `replication_factor` nodes, and read and write with quorums. Garage has
no consensus algorithm: each table is a set of CRDTs, and the versions of an object are ordered
by their wall-clock timestamps. This example runs the shape of a production cluster, three nodes
with replication factor 3 in the default consistent mode, in one guest of the Mac engine and
looks for bugs that nobody reported.

## The short answers

- **What was found?** Two new candidates and one bug fixed on Garage's `main-v2` branch but in
  no release. Nothing was posted.
  1. **Fixed on `main-v2`, not released: after a backward step of the wall clock, CopyObject
     onto an existing or a deleted key is acknowledged and has no effect.** The copy's version
     takes the node's clock as its timestamp, which is older than the key's current version.
     Commit `4dde841d` (2026-11-22, "add missing quota check in copyobject") changed it to
     `next_timestamp` (`regressions/copy-lost-after-a-backward-clock-step.schedule`).
  2. **New: after a node comes back, ListObjectsV2 lists the keys deleted while it was down, for
     up to 10 minutes, while GET answers 404 on every node.** Each node filters its own copy of
     the object table before the coordinator merges the copies, so the nodes that hold the
     deletion return nothing and the merge sees only the stale copy
     (`regressions/deleted-key-listed-after-a-node-returns.schedule`). ListMultipartUploads
     lists an aborted upload the same way
     (`regressions/aborted-upload-listed-after-a-node-returns.schedule`).
  3. **New, minor: a cluster formed with `garage node connect` does not form again after a power
     loss of every node in its first minute.** Each node saves the addresses of its peers once a
     minute, by truncating and writing a file without a sync, and the documented deployment
     sets no `bootstrap_peers` (the build `--set peerwait=no`, section "What was found").
  4. **Known, with the SQLite engine (issue #1571, open): a power cut of every node loses
     acknowledged writes, also with `metadata_fsync = true`.** Garage then runs SQLite in WAL mode
     with `synchronous = NORMAL`. `hydo/hydo-sqlite.yaml` shows it on hvx (Milestone 384); the
     LMDB image passes the same schedules.
- **What held?** With `metadata_fsync` and `data_fsync` on, every acknowledged PutObject,
  DeleteObject, DeleteObjects, multipart upload and its abort, and every CopyObject without a
  clock step, stayed as acknowledged. That held through kills, pauses, partitions and one-way
  loss of any node or of the group, one node's power loss, power cuts of the whole guest aimed
  at 0 to 30 ms after an acknowledgement, and damaged files of one node. Every read through
  another node after the acknowledgement saw the write, but for the copies of finding 1. No body
  read back was torn.
- **What is documented?** With Garage's defaults, `metadata_fsync = false` and
  `data_fsync = false`, a power cut of the guest lost acknowledged writes; `hydo explore
  --configs` labeled it the documented trade-off. A damaged LMDB file stops its node at every
  start, which the page "Known issues" names.
- **What hydo lacked?** Section "What hydo lacked": `hydo rules scan --kind writes` found no
  route in Garage, the clock audit does not see versions ordered by wall-clock timestamps, and
  an `acked_outcome` check without deliveries makes every passing run count as vacuous.

## The version

Garage v2.5.0 (2026-10-24, tag commit `586726ed`), the newest release when the onboarding
started (2026-11-23). The images on Docker Hub that are newer are builds of commits. The sources
were read at the tag, and `main-v2` at `0ec59975` (2026-11-22) for the fixes after it.

| image | index digest | amd64 | arm64 |
|---|---|---|---|
| `dxflrs/garage:v2.5.0` | `sha256:677e5b75…` | `sha256:23b16d34…` | `sha256:32b5de84…` |

`hydo/fetch-images.sh` fetches the image by digest. The image holds one static program,
`/garage`.

## The promises

From the configuration reference (`consistency_mode`, `replication_factor`), the page "Known
issues", and the code:

1. An acknowledged write is readable after it, through any node: the consistent mode with
   replication factor 3 writes and reads with quorums of 2, and the reference says
   "read-after-write consistency is guaranteed".
2. An acknowledged write survives the loss of one node, and with `metadata_fsync` and
   `data_fsync` on, a power failure of the nodes.
3. A completed multipart upload is whole, and an aborted one never becomes the object.
4. A deleted object stays deleted, and ListObjects lists what exists.
5. CopyObject is a write like PutObject: the later write of a key wins.

Garage does not promise conditional writes: `If-None-Match` and `If-Match` on PutObject are
"structurally impossible to implement in Garage due to the lack of a consensus algorithm"
(page "Known issues", issue #1052). The workload does not test them. The promises 3 to 5 are
read from the code and the S3 semantics that Garage implements, and are unconfirmed by the owner.

## The project

`docker-compose.yaml` runs three nodes of the image with one configuration file each, as the
page "Deployment on a cluster" deploys them, with volumes for the metadata and the data.
`config/garage-fsync-true.toml` and `config/garage-fsync-false.toml` are that file:

- `replication_factor = 3`, `consistency_mode = "consistent"`, `db_engine = "lmdb"` (the
  default engine), and a map size of 4 GiB instead of the default 1 TiB.
- `metadata_fsync` and `data_fsync`, both `true` or both `false`. The build parameter `fsync`
  chooses the file, `true` by default. Garage's own default is `false` for both.
- `rpc_public_addr_subnet`, so that each node finds its address in the guest's service network.

`hydo compose` imported the file. The changes from the import (the header of `hydo/hydo.yaml`):

- Each node's `/var/lib/garage`, with `meta/` and `data/`, is a file system of its own
  (`storage:`), the target of `L=` and `G=`. Production puts the metadata on an SSD and the
  data on disks; here both share the node's file system, because `storage:` takes one
  directory per service.
- The readiness checks connect to the admin port. A node is not healthy before the layout
  exists, so the cluster's health is the first command's wait.
- The group `nodes` holds the three nodes, for the cluster faults.
- The guest has 1,024 MiB. The three nodes peak at 41 to 44 MiB each (`hydo run --memory`).

## The workload

The client `gac` (`client/main.go`, the AWS SDK for Go v2 1.41.5 with its S3 client 1.97.3, and
Go's standard library for Garage's admin API) speaks S3 with path-style addresses, no retries,
and a time limit of 30 s per request. Each operation goes to a node that the command's seed
picks.

| command | operation |
|---|---|
| `first_init` | connects the three nodes, applies a layout of one zone with the three nodes, imports the S3 key, makes the bucket `hydo` and allows the key on it, through the admin API v2; waits until every node is healthy, answers a PutObject, and has saved its peers (finding 3) |
| `parallel_driver_put` | PutObject of a new key |
| `parallel_driver_over` | PutObject over a known key |
| `parallel_driver_del` | DeleteObject of a known key |
| `parallel_driver_mdel` | DeleteObjects of two known keys in one request |
| `parallel_driver_mpu` | a multipart upload of 2 to 4 parts to a new or a known key, and its completion |
| `parallel_driver_abort` | a multipart upload of one part to a new key, and its abort |
| `parallel_driver_copy` | CopyObject of a known key to a new or a known key |
| `anytime_read` | GetObject of up to 5 known keys through any node: each body must be whole |

The objects are 64 bytes to 2 KiB (Garage keeps them inline in its metadata), 8 to 128 KiB (one
data block), or 1.1 to 1.4 MiB (two blocks of Garage's 1 MiB). Each body starts with a line that
names its tag, its length and the sha256 of the rest, so a read knows the write it returns and
whether it is whole.

One command at a time works on a key. An answer 2xx is acknowledged, a 4xx answer but 408 and
429 failed, and any other answer, or none, unknown. A key whose operation has an unknown outcome
takes no other operation. After each acknowledged write the command reads the key through
another node and exits 100 when a certain answer is not the write (read-after-write). Each
acknowledgement writes a console event, `garage.<kind>_acked`, the event of the reactions.
40 rounds of 1 s give about 100 operations a run.

## The rules

| rule | promise | where |
|---|---|---|
| `objects` (a `type: messages` check with `acked_outcome` and `effect: recorded`) | every acknowledged write is the key's value at the end unless a later write of the key replaced it; no refused write took effect | `hydo.yaml`, the read-back `gac readback` |
| `compose.commands` | read-after-write through another node; no body read is torn | the drivers, `anytime_read` |
| `list` | ListObjectsV2 lists every key that GET finds, and no key that GET does not | `checks/final_list` |
| `uploads` | no upload whose abort Garage acknowledged is still listed | `checks/final_uploads` |
| `available` (eventually, 120 s) | every node answers a listing after the heal and in the recovery boot | `checks/eventually_available` |

Each operation is a send of the history `objects`, with its id `<kind>.<key>.<seq>.<value>`.
The read-back reads every key until each has a certain answer, then writes the final state of
each operation: `s=ok` for acknowledged, `s=fail` for refused, `s=info` for unknown. It writes
`f=1` or `f=0` for the last acknowledged write of each key, and for every write of unknown
outcome, by whether the key holds the write's value now (hydo guide messages, "Read back the
final states").

## Without faults, and the regressions

`disk=none | run` passes with one fingerprint on the final image
(`sha256(explore-ob-garage-arm64.cpio.gz)=d25c3a50…`): `e80b6137f1d1a827` twice. A run takes about 4 s
in the engine and 12 s of wall time.

| regression | finding | verdict |
|---|---|---|
| `copy-lost-after-a-backward-clock-step.schedule` | 1 | `VIOLATION [compose.commands]`: the copy's read through another node answers the key's old value or 404 |
| `deleted-key-listed-after-a-node-returns.schedule` | 2 | `VIOLATION [list]`: three deleted keys are listed |
| `aborted-upload-listed-after-a-node-returns.schedule` | 2, the uploads | `VIOLATION [uploads]`: an aborted upload is listed |

Finding 3 needs a build without the first command's wait, so it has no file in `regressions/`:

```sh
hydo build hydo.yaml --set peerwait=no
hydo run "disk=cut | run C=3"     # VIOLATION [available] not satisfied 120.56s after heal
```

## What was found

### 1. Fixed on `main-v2`, not released: CopyObject after a backward clock step

**What happens.** After the wall clock steps back, a CopyObject onto a key that exists, or that
was deleted, answers 200 and changes nothing. A GET through any node returns the key's old
value, or 404 after a deletion. With a step back of an hour before step 17,
`disk=none | run T=17:-3600` fails on every run:

```text
VIOLATION [compose.commands] command parallel_driver_copy (entry 45) reported a violation (exit 100): read-after-write: copy.k0.4.v29x0 was acknowledged by garage1, and then garage3 answered - for k0
```

The blind explorations ended with it in 23 of their 200 schedules, the first at seed 3
(section "The explorations"), twice as a copy lost in a run with a power cut: the read-back
found the key without the copy's value. The shrinker reduced seed 7 to the clock step alone, `T=17:-162000`.

**The cause.** Every write of an object version takes a timestamp, and the object table keeps
the version with the newest one (`src/model/s3/object_table.rs`, `strip_obsolete` and
`Object::merge`). PutObject, DeleteObject and CreateMultipartUpload take
`next_timestamp(existing_object)`, the larger of the clock and the newest version's timestamp
plus 1 (`src/api/s3/put.rs` line 720), so a later write wins after a clock step. CopyObject
without a change of encryption or checksum takes the clock alone:

1. `handle_copy` calls `handle_copy_metaonly` (`src/api/s3/copy.rs` line 183).
2. `handle_copy_metaonly` sets `let new_timestamp = now_msec();` (line 293), without reading the
   destination object.
3. After a step back, `new_timestamp` is older than the destination's current version or its
   delete marker, so every node keeps that one, and the copy's version is dropped.

The same happens without a clock step when the node that serves the copy has a clock behind
the node that wrote the key's last version, by more than the time between the two writes.

**Status.** Commit `4dde841d` on `main-v2` ("s3: add missing quota check in copyobject (fix
#1621)", 2026-11-22) reads the destination object and uses `next_timestamp` for the quota check,
which fixes this too. No release has it yet; v2.5.0 has the bug. No issue describes the lost copy
(section "The duplicate searches"). Not posted.

### 2. New: ListObjectsV2 lists deleted keys after a node comes back

**What happens.** A node that was down while keys were deleted keeps their old versions until
the object table's anti-entropy sync repairs it. Until then, a ListObjectsV2 lists the deleted
keys, through any node, whenever the stale node is one of the two that answer the read. GET of
such a key answers 404 on every node. `regressions/deleted-key-listed-after-a-node-returns.schedule`
kills `garage1` before step 1 and starts it again before step 2, and fails on every run:

```text
VIOLATION [list] the listing differs from the objects in 3 keys
```

`hydo inspect` held the end of a run with `K=garage3@2+3`, where `k1` was deleted while
`garage3` was down, and listed through each node every minute of guest time, with no GET in
between: `k1` was listed through every node for 2 minutes, through one or two of them until
minute 9, and through none from minute 10 on. One node's power loss (`L=garage3@2+3`), a pause
of 7 steps (`P=garage1@33+7`), and a kill at the moment of an acknowledgement
(`A=garage.put_acked:kill-now-garage1`) give it too. The blind explorations ended with it in 52 of
their 200 schedules, the first at seed 2, and `hydo explore --configs` found it with both values of
`fsync`.

**The cause.**

1. `handle_list` (`src/api/s3/list.rs` line 63) asks the object table for a range of keys with
   the filter `ObjectFilter::IsData`: an object with a version that has data.
2. `get_range_internal` (`src/table/table.rs` line 480) sends the range read to the nodes of the
   partition and waits for the read quorum, 2 of 3.
3. Each node applies the filter to its own copy before it answers: `read_range_aux`
   (`src/table/data.rs` lines 152 to 161) keeps an entry only when
   `matches_filter` holds (`src/model/s3/object_table.rs` line 882). A node that holds the
   deletion has a delete marker as the newest version, so its copy does not match and it
   returns nothing for the key.
4. The coordinator merges the entries it received per key (`table.rs` lines 527 to 545). For
   the deleted key it received only the stale node's entry, which has data, so the key is listed
   with its old size and ETag. The merge cannot see the deletion it was never sent.
5. `get_range` does no read repair (`monotonic_read` is false), and the object table's full
   anti-entropy sync runs every 10 minutes (`ANTI_ENTROPY_INTERVAL`,
   `src/table/replication/sharded.rs` line 27).

A read of one key (`get`) merges the nodes' entries without a filter, so GET is right. The
consistent mode's quorums intersect, so a filter applied after the merge would see the deletion.

**The uploads.** ListMultipartUploads reads the same table with the filter
`ObjectFilter::IsUploading` (`list.rs` line 181), so an upload that a stale node still holds as
uploading is listed after its abort. Seed 72 of e3 showed it beside finding 1, and
`regressions/aborted-upload-listed-after-a-node-returns.schedule` separates it: slow links of
`garage3` (500 ms) from step 21 to 29 and a power loss of `garage1` before step 23 end with

```text
VIOLATION [uploads] 1 aborted uploads are still listed
    the upload 4a2248ff… of k35a is listed after its abort was acknowledged
```

The slow links alone, and the pause of seed 72 alone, pass. A client that lists the uploads to
clean them up gets an upload that no longer exists.

**The consequence.** After any restart or partition of one node, a client that lists a bucket,
such as a sync or backup tool or a garbage collector of its own, sees objects that were deleted
and cannot read them, for up to 10 minutes. The listing breaks the read-after-write promise of
the consistent mode.

**Status.** New as far as the search found. Unchanged on `main-v2`. Not posted. No native run:
the cause is plain in the code, and hydo's replay shows it.

### 3. New, minor: the peers are lost at a power loss in the cluster's first minute

**What happens.** The documented deployment ("Deployment on a cluster") forms the cluster with
`garage node connect` and sets no `bootstrap_peers`. Each node writes its known peers to
`<metadata_dir>/peer_list` once a minute. When every node loses power within the minute after
`garage node connect`, each node starts again with an empty list, connects to nobody, and the
cluster never answers again. With the build `--set peerwait=no` the first command does not wait
for the lists, and `disk=cut | run C=3` ends:

```text
VIOLATION [available] not satisfied 120.56s after heal, deadline 120s (last check rc=1: a node does not answer a listing)
```

The nodes log `Could not reach quorum of 2 ... Network error: Not connected` until the end.
`hydo inspect` showed `peer_list` of 1 byte (no peer) on every node at the end of a clean run,
43 s after the start, and of 97 bytes (two peers) 20 s later. The default build waits for the
lists in `first_init`, so that the explorations' power cuts test the data.

**The cause.**

1. `discovery_loop` (`src/rpc/system.rs` line 681) runs every `DISCOVERY_INTERVAL`, 60 s
   (line 43). It calls `save_peer_list` (line 792) once per round.
2. `save_peer_list` writes through `Persister::save_async` (`src/util/persister.rs` line 79):
   `File::create`, which truncates the file, then `write_all`, with no `sync_all` and no
   rename. The same Persister writes `cluster_layout`.
3. At its start a node has no cached list, so its first round writes the file again, with the
   peers it loaded from it. A power loss of every node within the kernel's writeback delay after
   that write can leave an empty file too. That case comes from the code; no run showed it.

**The consequence.** Small: it needs a power loss of every node at once, in the first minute of
a new cluster or in the seconds after every node started. Then an operator must run `garage node
connect` again, or set `bootstrap_peers`.

**Status.** New as far as the search found. Issues #1457 and pull request #1488 (2026-07) changed
how often the file is written, not how. Not posted.

### 4. Known (#1571): with the SQLite engine, a power cut of every node loses acknowledged writes

This finding comes from Milestone 384, on hvx. The sync audit (`hydo rules scan --kind sync`,
Milestone 381) ranks Garage's SQLite adapter first. With `metadata_fsync = true` Garage opens
SQLite in WAL mode with `synchronous = NORMAL` (`SqliteDb::open`, `src/db/sqlite_adapter.rs`, line
79). SQLite then syncs the WAL only at a checkpoint, so a commit returns before it is on the disk.
The example's `hydo.yaml` runs LMDB, so no earlier run met it.

**The variant.** `hydo/hydo-sqlite.yaml` is `hydo.yaml` with `db_engine = "sqlite"` in the nodes'
configuration (`config/garage-sqlite-fsync-<fsync>.toml`). The workload, the checks and the
build parameter `fsync = true` are the same. On hvx, with this tree's hydo, the image
`explore-ob-garage-sqlite-disk.img` (`ce8592a991f3bf6a`) against the image of `hydo.yaml`
(`0e42f5bed445cad9`), both built with `--set arch=amd64`:

| schedule | SQLite | LMDB |
|---|---|---|
| `disk=none \| run` | OK, `c02cf3d733856138` | not run |
| `disk=cut \| run A=garage.put_acked@10+10:cut`, the audit's cut 10 ms after an acknowledged PutObject | `VIOLATION [objects.acked-outcome]`: the multipart upload `mpu.k93` was acknowledged and is gone, `fcd596d9e0a8c850`, twice | OK, 93 operations judged |
| `disk=cut \| run C=20`, the audit's cut after step 20 | `VIOLATION`: 5 of 47 acknowledged operations are gone (two CopyObjects, two deletions of DeleteObjects, one PutObject), and an upload whose abort Garage acknowledged is listed again, `baf58af8f496c189` | OK, 47 operations judged |

A first build of the same file, with another comment at its top (`fe6b6f0873c14210`), gave the
same two violations. It also ran one node's power loss: `disk=cut | run
A=garage.put_acked@10+10:powerloss-garage1` passed, and `disk=cut | run L=garage1@20+3` broke
only the listing (`VIOLATION [list]`, two deleted keys listed, finding 2) and lost no
acknowledged operation.

A power loss of one node loses no acknowledged write, because the two other nodes hold it. The
whole guest's power cut takes the last commits of every node at once. The schedule is saved as
`schedules/sqlite-acked-writes-lost-at-a-power-cut.schedule`; it is not in `hydo/regressions/`,
because the image of `hydo.yaml` would replay it too.

**Status.** Known: issue
[#1571](https://git.deuxfleurs.fr/Deuxfleurs/garage/issues/1571) (open, 2026-10-09) reports the
same cause on a single node and asks for a setting with `synchronous = FULL`. The configuration
reference lists `PRAGMA synchronous = NORMAL` for `metadata_fsync = true`. This run adds that a
cluster of three nodes loses the writes too when every node loses its power. `main-v2` on
2026-11-27 still sets `NORMAL` (it added `journal_size_limit`). Not posted.

### Not bugs

| failure | label | where |
|---|---|---|
| with `fsync=false`, a power cut of the guest at step 1 lost an acknowledged PutObject | documented trade-off: the reference says that without `metadata_fsync` and `data_fsync` a power failure can lose data; `hydo explore --configs` labeled it so | e5, `disk=cut | run C=1` |
| a node did not start again after a damaged LMDB file (`G=` on `db.lmdb`) | documented: "Known issues", "LMDB metadata corruption"; the other two nodes kept the data | e1 seeds 6 and 13 |
| the uploads check timed out in the recovery boot after a power cut | a mistake of the example: the first command did not wait for the peer lists, so the nodes did not form again (finding 3); the checks' own deadlines also passed the 60 s of `command_timeout` | e2 and the first directed cuts |
| `garage1` never answered again after bits of its `node_key` flipped (`available` failed in e7 seeds 121 and 167) | the environment: a damaged key file. Garage reads the 64 bytes as the key pair without comparing the secret half with the public half (`gen_node_key`, `src/rpc/system.rs` line 210), so the node starts and cannot join. The other two nodes kept serving | e7 |
| the listing check named the keys that the first command writes | a mistake of the rules, fixed | the first clean run |

## The duplicate searches

The searches ran on 2026-11-23 over Garage's issues and pull requests, open and closed, with the
tracker's API (`/api/v1/repos/Deuxfleurs/garage/issues?state=all&type=all&q=...`). The history of
`copy.rs`, `table/table.rs`, `table/data.rs`, `util/persister.rs` and `rpc/system.rs` from v2.5.0
to `main-v2` was read too.

| finding | phrases | what they found |
|---|---|---|
| 1, the copy | `copy timestamp`, `CopyObject`, `copy object`, `clock`, `timestamp`, `last writer wins`, `overwrite lost`, `1621` | #1621 (closed): "CopyObject doesn't check quotas", fixed by #1627 (`4dde841d`), which also takes `next_timestamp`. #543 (2023) introduced the increasing timestamps for the other writes. No report of a lost copy |
| 2, the listing | `list deleted`, `ListObjects deleted`, `deleted object listed`, `listing`, `ListObjectsV2`, `filter merge`, `read_range filter`, `matches_filter`, `get_range`, `IsData`, `tombstone`, `delete marker`, `deleted file still`, `ghost`, `list consistency` | no report. #544 (2023) ran Jepsen's set test on PutObject and ListObjects, without deletions. #1136 (open) revives entries from a restored snapshot, another cause |
| 3, the peers | `peer_list`, `peer list`, `Persister`, `cluster_layout corrupted`, `node connect reboot`, `nodes do not reconnect`, `fsync` | #1457 and #1488 (how often the list is written), #1150 (a panic when the layout cannot be saved), #1571 (open: a single node with SQLite loses an acknowledged PUT at a power loss with `metadata_fsync = true`, and names LMDB's `MDB_NOMETASYNC`). No report of lost peers |
| 4, SQLite (2026-11-27) | `sqlite`, `synchronous`, `synchronous NORMAL`, `metadata_fsync`, `WAL`, `journal_mode`, `sqlite power`, `sqlite fsync`, `sqlite durability`, `sqlite corruption`, `db_engine sqlite`, `power failure`, `power loss`, `fsync`, `lost write` | #1571 (open), the same cause on one node. Other causes: #1594 (closed: the WAL grows, fixed by `journal_size_limit`), #1200 (a malformed SQLite file), #1224 (open: lost data on a single node, no cause named) |

## What held, and why

- **Every other write orders itself after the key's newest version.** `next_timestamp` makes a
  PutObject, a DeleteObject and a multipart upload win over the version it read, whatever the
  clock says. Clock steps of 30 s to 30 days back lost no such write.
- **The quorums intersect.** Reads of one key went to any two nodes and always saw the
  acknowledged write of the two others, through kills, pauses, partitions and one-way loss.
- **LMDB with `metadata_fsync = true` kept every acknowledged write at a power cut.** LMDB runs
  with `MDB_NOMETASYNC` even then (`src/db/lmdb_adapter.rs` line 42), which may undo the last
  transaction at a crash (issue #1571 names it). 48 power cuts aimed at 0 to 30 ms after an
  acknowledged PutObject, overwrite or DeleteObject (e4) lost nothing: Garage commits other
  transactions within milliseconds, and each commit syncs the one before it.
- **Blocks are checked when read.** A damaged block file of one node (`G=` on `data/`) did not
  reach a client: the node fetched the block from another node.

## The explorations

All runs used the Mac engine on this Mac, with the hydo of `main` at `45aa2be3` (0.5.0 and
Milestones 344 to 347), one worker of 1,024 MiB: the explorer sized its pool from the free
memory, which other sessions shared.

| exploration | options | schedules | failures |
|---|---|---|---|
| e1 | `--seeds 1-20 --strategy process --new-faults --cluster-faults --storage-faults` | 20, in 9 minutes with the shrinks | 11: finding 1 in 5, finding 2 in 3, a damaged LMDB file of one node in 2, the uploads check past its time limit after a damaged file in 1 |
| e2 | `--reactive-at garage.put_acked@10,garage.del_acked@10 --react-delays 0..40/2 --react-actions cut,kill-now-garage1`, on the image before the peer wait | 84, in 12 minutes | 79: the 42 power cuts left the nodes without their peers (finding 3), and 37 of the 42 kills showed finding 2 |
| e3 | `--seeds 21-80`, the options of e1 | 60, in 7 minutes | 27: finding 2 in 16, finding 1 in 10, a damaged LMDB file in 1; seed 72 also listed an aborted upload |
| e4 | `--reactive-at garage.put_acked@10,garage.over_acked@20,garage.del_acked@30 --react-delays 0..30/2 --react-actions cut` | 48, in 3 minutes | none |
| e5 | `--configs --seeds 1-20 --durable-only`, `fsync` `true` and `false` | 20 each | `true`: finding 2 in 6. `false`: finding 2, and a power cut that lost an acknowledged PutObject, the documented trade-off |
| e6 | `--seeds 81-120 --strategy swarm --new-faults --cluster-faults --storage-faults --reactive fault --kill-now --react-events garage.put_acked,garage.del_acked,garage.mpu_acked,garage.copy_acked` | 40, in 4 minutes | 16: finding 2 in 13, finding 1 in 2, a damaged LMDB file in 1 |
| e7 | `--seeds 121-200 --strategy process --new-faults --cluster-faults --storage-faults --corrupt-several --reactive fault --kill-now --react-events garage.put_acked,garage.over_acked,garage.mdel_acked,garage.abort_acked` | 80, in 10 minutes | 29: finding 2 in 20, finding 1 in 6, a damaged `node_key` in 2, a damaged LMDB file in 1 |

`hydo stats` of e3, e6 and e7 still counted a new property outcome near the end of each round.
The two findings fail different checks in different runs, so the checks' outcomes keep
changing. The last 80 seeds added no new class of failure but the damaged key, and the rounds
stopped there.

The directed runs:

| what | schedules | result |
|---|---|---|
| a backward clock step | `T=10:-600`, `T=17:-3600`, `T=17:-1` | finding 1 with 600 s and 3,600 s; 1 s passes |
| one node down while deletions run | `K=garage3@2+3`, `L=garage3@2+3`, `K=garage1@13+1` | finding 2 |
| power cuts of the guest | `C=3`, `C=38`, the reactions of e4 | OK with the peer wait; finding 3 without it |
| pauses, the longest past the failure detector (4 missed pings 15 s apart) | `P=garage2@3+36`, `P=garage1@2+37`, `P=nodes.majority@5+20`, `P=garage2@2~0:80000`, `P=garage1@5~0:75000 K=garage3@30+2` | OK; the writes through a paused node failed or got no answer, and a paused node applied the queued writes when it went on, so it listed no deleted key |
| damaged data blocks of one or two nodes | `G=garage1@10:flip,n=8,file=data/`, `G=garage2@10+3:zero,len=65536,file=data/`, `G=garage3@10:trunc,file=data/`, `G=nodes.majority@10:zero,file=data/` | OK; no damaged body reached the client |
| the faults of the uploads finding, one at a time | seed 72 of e3 without one fault each | the slow links with `L=garage1@23+1` list the aborted upload; each alone does not |
| a damaged `node_key` | `G=garage1@1+2:flip,n=32,seed=cb902ca2` | `garage1` never answers again; the other two serve (section "Not bugs") |

## What hydo lacked

1. **`hydo rules scan --kind writes` found no route.** It read 205 files of Garage and printed
   `0 routes that write`. Garage routes S3 requests through its own `Endpoint` enumeration
   (`src/api/s3/router.rs`, `api_server.rs`), not a web framework's route table. The write
   operations of the workload came from `api_server.rs` by hand.
2. **The clock audit does not see versions ordered by wall-clock timestamps.** `hydo rules scan
   --kind clock` named 4 functions, the request signatures' date checks, and none of the places
   of finding 1. Garage orders an object's versions by `now_msec()` (`copy.rs`, `put.rs`,
   `multipart.rs`), a last-writer-wins order, not a duration. The audit also explained each
   place with "10 minutes by default (src/block/manager.rs:49)", a constant of another file.
3. **An `acked_outcome` check without deliveries makes every passing run vacuous.** The check
   has no `recv`, so its implicit claim `no-phantom` judges nothing, and `hydo explore` counted
   all 48 passing runs of e4 as `vacuous_passes` with the warning "may have checked nothing",
   although `acked-outcome` judged about 80 operations a run.
4. **One worker.** The explorer sized its pool to one worker of 1,024 MiB from this Mac's free
   memory, which other sessions shared, so the explorations ran at 160 to 860 runs an hour.

Milestone 350 fixed the first three. `hydo rules scan --kind writes` lists 42 write routes of
Garage v2.5.0 with their handlers, every operation of the workload among them. The clock audit
names the copy's `now_msec()` (`src/api/s3/copy.rs` line 293) as a wall-clock reading kept as a
version. A check whose `no-phantom` judges nothing beside `acked-outcome` no longer counts as a
vacuous pass.

## Time

The onboarding started at 21:40 local time on 2026-11-23. Other agents shared this Mac, and its
free disk fell below 12 GB twice; the onboarding waited about 20 minutes for it.

| phase | wall time |
|---|---|
| 1. Scouting: the documentation, the known issues, the code of the writes, the tables and the LMDB adapter | 25 minutes |
| 2. Setup to the first clean run: the import, the client, the admin API | 50 minutes, 20 of them waiting for disk |
| 3. Explorations and directed runs | 1 hour 30 minutes, the explorations about 50 minutes of it |
| 4. Judging: the copy, the listing, the peer lists | inside phase 3 |
| 5. The duplicate searches and the write-up | 50 minutes |
