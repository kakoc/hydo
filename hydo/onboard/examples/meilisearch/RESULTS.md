# Meilisearch under hydo: the task queue across crashes and power cuts

Meilisearch (github.com/meilisearch/meilisearch) is a search engine in Rust. Every write is an
asynchronous task: the server answers `202` with a `taskUid`, puts the task in a queue in LMDB,
and a scheduler processes the queue in batches. Each index is an LMDB environment of its own. This
example puts one Meilisearch server under hydo and hunts for bugs that nobody reported in what
the task queue promises across kills, power cuts, disk faults, and clock steps.

## The short answers

- **What was found?** Three new bugs and a fourth form of the first, none posted upstream:
  1. **An index swap runs twice after a crash.** The swap commits the swapped index mapping in
     one LMDB transaction and the task's `succeeded` status in a second one. A crash between the
     two leaves the task `enqueued`, and the restart swaps the indexes back. A power cut reaches
     it in hydo (`disk=none | run A=meili.add_acked@20+40:cut`), and so does, since Milestone
     305, a kill at the event's moment (`A=meili.swap_acked@20+14:kill-now-meilisearch`). A plain
     SIGKILL reaches it in Docker (`native/swap-twice.sh`, 4 of 48 rounds).
  2. **A power cut empties the payload of an enqueued document addition.** The route renames
     the request's payload file into place without an `fsync` and then commits the task. After a
     power cut the task succeeds with `indexedDocuments` 0, or fails with `No such file or
     directory (os error 2)` together with every task of its batch, also a deletion.
  3. **A succeeded dump is an empty file after a power cut.** The dump is written without an
     `fsync`, and its task is committed as `succeeded`. Every power cut of the runs came within
     30 s of a dump, before the guest kernel wrote it back, and left the dump at 0 bytes.
  4. **The same split runs an index's creation or deletion twice** (Milestone 346). After a
     kill between the two commits, a deletion that took effect ends `failed` with
     `index_not_found`, and a creation `failed` with `index_already_exists`. hydo's claim
     `acked_outcome` found both from the tasks' final states and effects.
  5. **Known and never reproduced upstream (#4584, #6202): a new database does not start again
     after a power cut in its first seconds** (Milestone 384). Meilisearch writes the database's
     `VERSION` file through a temporary file and a rename and syncs neither. After a power cut
     right after the first task's `202`, every start stops with `failed to infer the version of
     the database`. The variant `fresh/` shows it on hvx.
- **What held?** No acknowledged task was lost from the queue, and the tasks of each index ran in
  the order of their uids. Kills and pauses of the service and clock steps of 30 s to a year
  broke no check in 75 blind schedules and the directed ones. The settings were those of the
  last succeeded settings task, and the search agreed with the stored documents in every run.
  Each edit by function ran once in every run but one on a lying disk. Damaged files that
  Meilisearch detects stop it at the start. Every failure with disk I/O errors or a lying disk
  was the disk's own loss.
- **What did hydo lack?** Section "What hydo lacked": four gaps. Milestone 305 fixed the
  first.

## The version

Meilisearch v1.57.0 (2026-11-16) was the newest release when the onboarding started
(2026-11-17). Its tag is commit `1b782653`. It ran from Meilisearch's image
`getmeili/meilisearch:v1.57.0`. The image's index digest is `sha256:91bf0989…`; amd64 is
`sha256:540676d5…`, arm64 `sha256:ed87e30e…`. The sources were read at the tag, and the code of
each finding is the same on `main` at `d15a0866` (2026-11-17).

## The promises

Each check of the section "The rules" states one of these, from Meilisearch's documentation:

- The queue (learn/async/asynchronous_operations): tasks are processed "in the order they were
  received", and an `enqueued` task means that "Meilisearch is either processing them or will do
  so in the future". A `failed` task: "No changes were made to the database."
- Shutdowns (the same page): "Terminating a Meilisearch instance in the middle of an asynchronous
  operation is completely safe and will never adversely affect the database", and "any task that
  was `processing` is reset to `enqueued` on restart".
- Index swaps (learn/getting_started/indexes): "Swapping indexes is an atomic transaction: either
  all indexes are successfully swapped, or none are."
- Dumps: a succeeded dump task names its dump, the documented way to move data to a new version.

## The project

One Meilisearch server from the official image, as `docker-compose.yaml` starts it: development
mode without a master key, no telemetry, 256 MB of indexing memory, and 2 indexing threads.
`hydo onboard` imported the Compose file. The changes from the import:

- No bridge network: one service, and the client in the guest's own network namespace. The
  server listens on `127.0.0.1:7700`.
- The command names the database, the dump folder, and the snapshot folder under
  `/meili_data`. That folder is a file system of its own (`storage:` with 512 MiB), the target
  of `G=` and `L=`.
- The readiness check is the client's `mlc ready`.
- `guest.clock_after_cut: 60`, so the recovery boot's clock runs on from the cut, as a real
  machine's does; without it the tasks processed in the recovery boot start before they were
  enqueued. `guest.outcomes_after_cut: true`, so the checks know the unanswered requests after a
  power cut too.

## The workload

The client `mlc` (`client/`, Go with the standard library only) enqueues one task per command
and does not wait for it. Four commands run at once, four per round, in 60 steps:

| command | the task |
|---|---|
| `parallel_driver_add`, `parallel_driver_add2` | 1 to 20 new documents `c<cmd>_<i>` in index `docs`, each with a group, a value `v` of 0, and the word `w<cmd>` |
| `parallel_driver_upd` | a partial update that sets `v` on every document of an earlier acknowledged addition |
| `parallel_driver_del` | the deletion of one document of an earlier acknowledged addition |
| `parallel_driver_set` | new filterable attributes of `docs` |
| `parallel_driver_swap` | a swap of the indexes `sa` and `sb`, which hold one document each |
| `parallel_driver_edit` | an edit by function, an experimental feature, that adds 1 to the counter in index `cnt` |
| `parallel_driver_dump`, `parallel_driver_snap` | a dump or a snapshot in one call of four, else an addition |
| `parallel_driver_delidx` (Milestone 346) | the creation of a new index `x<cmd>` and its deletion, enqueued one after the other |

An answer `202` with a `taskUid` is acknowledged, and the client writes the task's uid and what
it asked for to `$HYDO_ACKED`. A request without an answer is unknown, and the checks do not
judge a document that such a request may have changed. The client writes the console events
`meili.add_acked`, `meili.swap_acked`, `meili.edit_acked`, `meili.dump_acked`,
`meili.snap_acked` and `meili.delete_index_acked`, the anchors of reactions (`A=`). Since
Milestone 346 it also records each request as a send of the history `tasks`, with the task's uid
as its system id.

## The rules

Every check first waits up to 60 s for the queue to hold no `enqueued` or `processing` task. A
queue that does not drain breaks the first promise.

| check | the promise |
|---|---|
| `final_tasks` | every acknowledged task is in the queue with its own type and ends `succeeded`, never `canceled`; a failed task prints its error and times. A succeeded addition or update received and indexed every document that the client sent. The tasks of one index ran in the order of their uids: their batch uids do not go back |
| `final_docs` | the documents of `docs` are what the succeeded tasks made of them, applied in uid order. The search agrees with the stored documents, by a filter on each group and by a phrase for a third of the additions |
| `final_settings` | the filterable attributes are those of the succeeded settings task with the highest uid |
| `final_swap` | `sa` and `sb` each hold their one document, at home after an even number of succeeded swaps |
| `final_edits` | the counter equals the number of succeeded edits: each edit ran once |
| `final_dumps` | the file of every succeeded dump task, and the snapshot, are whole archives with their metadata and index `docs` |
| `queue` (`hydo.yaml`, Milestone 346) | hydo's claim `acked_outcome`: every acknowledged task ends `succeeded`, or `failed` with `index_not_found`, and no final state contradicts the task's effect. `mlc readback` reads the effect of an addition (every document stored with its body) and of an index deletion whose creation succeeded (the index is gone) |

`errors: true` makes Meilisearch's `ERROR` lines findings too.

## Without faults, and the regressions

`disk=none | run` passes with `fp=eac607c09d945c94` in two runs (159 acknowledged tasks, all
7 checks held). The run takes 2 to 7 s in the engine. The saved regressions in
`hydo/regressions/`, each with `# expect: violation`, are:

| file | schedule | fingerprint |
|---|---|---|
| `swap-runs-twice-after-a-crash.schedule` | `disk=none \| run A=meili.add_acked@20+40:cut` | `44a749b10aeef55b` |
| `payload-lost-at-a-power-cut.schedule` | `disk=cut \| run A=meili.add_acked@20+2:cut` | `830da5344ced8d40` |
| `dump-empty-after-a-power-cut.schedule` | `disk=cut \| run C=30` | `e4469b79f388a979` |
| `swap-runs-twice-after-a-kill.schedule` (Milestone 305) | `disk=none \| run A=meili.swap_acked@20+14:kill-now-meilisearch` | `49a613db35d5f480` |

The values are from this commit's hydo on the Mac engine, at the fingerprint epoch 4, each the
same in two runs. The moment of a cut after an event moves with the image's bytes. The first
images of the onboarding, before the build parameter `dumps`, gave finding 1 with the cut at
`+2` and finding 2 at `+40` (hydo from `c1d78449`, `fp=5919be91fe02d8c2`, also from a fresh
clone).

**Since Milestone 346.** The client's history records, `mlc readback` and
`parallel_driver_delidx` give the image new bytes, and the image is built at the fingerprint
epoch 5. These are the values on the Mac engine, with the tree's hydo of Milestone 346. A value
without a note was the same in two runs.

| file | schedule | verdict | fingerprint |
|---|---|---|---|
| (no fault) | `disk=none \| run` | OK, 173 acknowledged tasks | `34d6b1999155c0a5` |
| `swap-runs-twice-after-a-crash.schedule` | `disk=none \| run A=meili.add_acked@20+40:cut` | `VIOLATION [dumps]`, findings 2 and 3, not the swap | `c4c67122170806f1` (one run) |
| `payload-lost-at-a-power-cut.schedule` | `disk=cut \| run A=meili.add_acked@20+2:cut` | `VIOLATION [dumps]`, and `queue.acked-outcome` | `3e5b31f5dd34a1fb` (one run) |
| `dump-empty-after-a-power-cut.schedule` | `disk=cut \| run C=55` (`C=30` passes now: no dump before step 30) | `VIOLATION [dumps]` | `547d6d5f16276222` |
| `swap-runs-twice-after-a-kill.schedule` | `disk=none \| run A=meili.swap_acked@20+14:kill-now-meilisearch` | `VIOLATION [swap]` | `ff2e49cab7d84f41` (one run) |
| `index-deletion-runs-twice-after-a-kill.schedule` | `disk=none \| run A=meili.delete_index_acked+13:kill-now-meilisearch` | `VIOLATION [tasks]`, and `queue.acked-outcome` | `b72d43f0a3a2d63c` |

## What was found

### 1. New: an index swap runs twice after a crash

**Label: a bug of the project, new as far as the search found.**

`disk=none | run A=meili.add_acked@20+40:cut` cuts the power 40 ms after the first document
addition acknowledged from step 20 on. The run ends with 21 succeeded swaps and with `sa` and
`sb` at home, as after an even number. The newest swap, task 164, was enqueued at 21.157 s of
the first boot and started at 81.046 s, in the recovery boot 60 s after the cut: it ran twice.
On the first image `disk=cut` gave the same. The verdict line names the document loss of
finding 2, and `hydo report --details <run dir>` shows the swap:

```text
swap: index sa holds [m] with home "sa" after 21 succeeded swaps; want one document with home "sb"
indexSwap task 164 succeeded: batch 136, enqueued 2026-11-17T00:00:21.157391968Z, started 2026-11-17T00:01:21.046101333Z, finished 2026-11-17T00:01:21.048093333Z
sa and sb are swapped otherwise than 21 succeeded swaps say
```

A user who swaps a newly built index into production, the documented way to reindex without
downtime, and loses the process at the wrong moment serves the old index again, with no failed
task.

The cause, the same at v1.57.0 and on `main`:

1. `IndexScheduler::process_batch` handles `Batch::IndexSwap` in
   `crates/index-scheduler/src/scheduler/process_batch.rs:369`. It opens a write transaction
   (`:372`), calls `apply_index_swap` (`:432`, the function at `:798`, which swaps the mapping
   at `:861`), and commits at `:434`. Then it sets the task's status to `Succeeded` in memory.
2. `IndexScheduler::tick` (`crates/index-scheduler/src/scheduler/mod.rs:168`) opens a second
   write transaction (`:279`), writes the task's status with `update_task` (`:322`), and commits
   at `:452`.
3. The `processing` status lives only in memory (`ProcessingTasks`,
   `crates/index-scheduler/src/processing.rs:12`). In LMDB the task stays `enqueued` until the
   second commit.
4. After a crash between the two commits the restart takes the task again, and
   `apply_index_swap` swaps the mapping back.

The same split holds for every batch that commits its effect before its status. It does no harm
to an addition, which replaces its documents again. Issue #1679 (2021), the design of the task
queue that replaced the update store, named the case ("processed task, but not marked as such,
and processed again on startup").

Kills in hydo did not reach it. Kill reactions at 18 delays after a swap, an edit, or an
addition passed (section "The explorations"): the kill goes through the guest agent, and the
server's threads finish the second commit first. Without hydo, `native/swap-twice.sh` kills the
server with `pkill -9` inside its container 0 to 45 ms after a swap and 16 additions, starts it
again, and compares the count of succeeded swaps with where `sa`'s document is. A swap ran twice
in 4 of 48 rounds (at 0, 12, 0 and 15 ms), and in round 4 of a first run. The script removes its
container and volume. Released hydo 0.3.4 (fingerprint epoch 3) builds and runs the example,
but cuts at other moments: with `+2` and `+40` it reached findings 2 and 3, not the swap, on
the first image and on this one.

Since Milestone 305 a kill in hydo reaches it too. `A=<event>+<ms>:kill-now-<svc>` kills the
server from a real-time relay in the guest, `ms` after the client's record, before any of the
server's threads runs on. One run per delay of 0 to 50 ms after a swap's `202`, on the Mac
engine:

| delay | verdict |
|---|---|
| 0, 6, 7 ms | `VIOLATION [edits]`: an edit by function ran twice, the same split of two commits |
| 13, 14 ms | `VIOLATION [swap]`: the swap ran twice: task 75, enqueued at 9.768 s, ran again at 21.239 s, after the restart |
| 1 to 5, 8 to 12, 15 to 50 ms | `OK` |

`+14` gave `fp=49a613db35d5f480` in three runs. `kill-meilisearch` at 13, 14, and 15 ms passed.
On hvx `A=meili.swap_acked@30+<ms>:kill-now-meilisearch` ran the swap twice at 34 to 40 ms
(`+36`: `fp=30ab8d845d6f258b` in three runs), and `kill-meilisearch` at 30 to 40 ms passed. The
same reactions armed at step 20 ran an edit twice at 14 to 22 ms on hvx, with either kill.

The regressions are `hydo/regressions/swap-runs-twice-after-a-crash.schedule` and
`swap-runs-twice-after-a-kill.schedule`.

### 2. New: a power cut empties the payload of an enqueued document addition

**Label: a bug of the project, new as far as the search found.**

With `disk=none | run A=meili.add_acked@20+40:cut`, the additions that were acknowledged and
not yet processed at the cut end `succeeded` with `receivedDocuments` 20 and 18 and
`indexedDocuments` 0, and their 38 documents are not in the index. With
`disk=cut | run A=meili.add_acked@20+2:cut` the restart fails tasks 165, 166 and 167 with `No
such file or directory (os error 2)` and the error type `internal`. Task 166 is a deletion with
no payload: it fails because an addition of its batch lost its file. Blind, seed 26 of the
second exploration lost a payload and shrank to `C=60 A=meili.add_acked:cut` with additions
only. Either way the client's documents were acknowledged and never read, and a failed
task's "no changes were made" covers documents that the server lost itself. A cut at a step
boundary (`C=30`) loses no document, because the scheduler processes an addition within a few
milliseconds and then deletes its payload.

The cause, at v1.57.0:

1. The route writes the request body into a temporary file in `data.ms/update_files/`
   and renames it to the task's uuid with `file_store::File::persist`
   (`crates/file-store/src/lib.rs:147`, called at
   `crates/meilisearch/src/routes/indexes/documents.rs:1598` and `:1624`). That is
   `tempfile::NamedTempFile::persist`, a `rename(2)`. Nothing syncs the file or its folder.
2. The route then registers the task. `register_with_custom_metadata` commits the queue's LMDB
   transaction (`crates/index-scheduler/src/lib.rs:1004`), which LMDB syncs, and the `202`
   follows.
3. Nothing makes the rename or the file's data durable. The file's data is allocated only at
   writeback, up to 30 s later. A cut 40 ms after the `202` left the file's name with 0 bytes.
   A cut 2 ms after it left neither name on the disk: the folder on the disk was older than both
   renames, and the index's commit of the running batch was lost too. So the tasks failed with a
   lost file, not because their batch ran a second time (checked on the v0.5.0 image,
   2026-11-23).
4. After a power cut in that window the queue holds the task, and its payload file is empty or
   missing. The scheduler maps it
   (`crates/index-scheduler/src/scheduler/process_index_operation.rs:96`): an empty file is an
   addition of no document, and a missing one fails the batch.

Native reproduction: none. The cause is plain in the code, and a power cut of a Docker host is
not a kill of a container (the playbook's rule). The regression is
`hydo/regressions/payload-lost-at-a-power-cut.schedule`.

### 3. New: a succeeded dump is an empty file after a power cut

**Label: a bug of the project, new as far as the search found.**

`disk=cut | run C=30` leaves all five dumps of the run at 0 bytes, and each dump task says
`succeeded` with its `dumpUid`:

```text
dump: task 57 succeeded with dump 20261117-000007890; the file has 0 bytes, 0 entries: EOF
```

The cause: `process_dump_creation` writes the archive with `File::create` and a `BufWriter`
(`crates/index-scheduler/src/scheduler/process_dump_creation.rs:281-282`), flushes the writer,
never syncs the file, and returns the task as `succeeded` (`:285`). The status is then committed
and synced. A snapshot is written through a temporary file renamed over `data.ms.snapshot`
(`process_snapshot_creation.rs:238`), also without a sync. ext4 writes a replacing rename's data
before the rename, so only a first snapshot could be lost the same way, and no run lost one.
Issue #6597 reports a dump missing after a succeeded task in Docker, with no power cut; its cause
is not known. Native reproduction: none, for the reason of finding 2. The regression is
`hydo/regressions/dump-empty-after-a-power-cut.schedule`.

### 4. The split of two commits in an index's creation and deletion (Milestone 346)

**Label: a bug of the project, the cause of finding 1 in two more tasks.**

Milestone 346 declared hydo's claim `acked_outcome` in the check `queue` and added
`parallel_driver_delidx`, which creates an index `x<cmd>` and enqueues its deletion. A kill of
the server at the moment of the first deletion's `202` (`kill-now-meilisearch`, one run per
delay of 4 to 14 ms on the Mac engine) found two new forms of finding 1:

| delay | what `queue.acked-outcome` said |
|---|---|
| 7 to 11 ms | `mkidx.4 (system id 13) was acknowledged and ended failed with index_already_exists, not an error the client causes`: the creation ran twice |
| 13 ms | `delidx.4 (system id 14) was acknowledged and ended failed with index_not_found, but its effect is there`: the deletion ran twice |
| 4 to 6, 12, 14 ms | held |

`+7` gave `fp=f3bf73fdaf6b40f3` and `+13` gave `fp=b72d43f0a3a2d63c`, each in three runs. In
`+13`'s run the creation, task 13, succeeded, and the index `x4` is gone. The deletion, task 14,
says `Index x4 not found`. So the deletion took effect in the first run, and the restart ran it
again. `index_not_found` is in the claim's `client_errors`, because a deletion of an index whose
creation failed fails so. A check that only excuses that error passes this run; the claim breaks
on the effect. Both tasks commit their effect before their status, as in finding 1. The regression is
`hydo/regressions/index-deletion-runs-twice-after-a-kill.schedule`.

The same reading of the effect changes what finding 2 means on this image. With
`disk=cut | run A=meili.add_acked@20+2:cut`, the two additions that ended `failed` with `No such
file or directory` have every document stored with their body. The first boot indexed them, and
the restart ran their batch again after the payload files were gone. The claim says
`add.155 (system id 178) was acknowledged and ended failed with internal, but its effect is
there`. With `disk=none | run A=meili.swap_acked@20+10:cut` and `+16` the payload was lost
before the first run: the additions ended `succeeded` with `indexedDocuments` 0, their documents
are missing, and the claim says `ended succeeded, but its effect is not there`. With `+14` and
`A=meili.add_acked@20+40:cut` the additions succeeded with `indexedDocuments` 0 and their
documents are stored, so the claim held there; only `final_tasks` counts that as broken.

### 5. Known, never reproduced upstream: a new database does not start again after a power cut

**Label: a known bug.** Issue [#4584](https://github.com/meilisearch/meilisearch/issues/4584)
(2024, closed in 2025 because nobody could reproduce it) reports a new database whose `VERSION`
file was empty, and guesses "a disk sync or something". Issue
[#6202](https://github.com/meilisearch/meilisearch/issues/6202) (closed) reports the same on
Kubernetes. Its fix, pull request #6275, changed only the error message. Nothing was posted.

**Why it ran.** The sync audit (`hydo rules scan --kind sync`, Milestone 381) lists
`create_version_file` (`crates/meilisearch-types/src/versioning.rs`, line 27): a write that no
sync follows, and a rename into place without a sync of the folder. hydo syncs the guest's disks
after provisioning, so the database that `hydo/hydo.yaml`'s service creates at its first start is
on the disk before every schedule. No run of the example met the place.

**The variant.** `fresh/hydo.yaml` runs the service of `hydo/hydo.yaml` with a workload of its
own. The schedule's `K=meilisearch@0+2` keeps Meilisearch down during steps 0 and 1. Step 1
removes the database and syncs, and the restart before step 2 creates a new database, as a new
deployment does. Step 3 enqueues one document addition and records its task's uid after the
`202`. The check `fresh_db` says that Meilisearch answers again and that every acknowledged task
is in its queue. With `--set fresh=no` step 1 keeps the database.

On hvx, with this tree's hydo and the image `999ebd681046eb44`:

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run K=meilisearch@0+2` | OK | `363a1da216c220fc` |
| `disk=cut \| run K=meilisearch@0+2 C=4`, right after the task's `202` | `VIOLATION service cannot recover (committed data unrecoverable) (meilisearch)` | `3a1aec7c61e5c7fc`, twice |
| `disk=cut \| run K=meilisearch@0+2 A=meili.fresh_acked@3+10:cut`, the audit's cut 10 ms after the answer | the same violation | `c65342bdd07daf8f` |
| `disk=cut \| run K=meilisearch@0+2 A=meili.fresh_acked@3+10:powerloss-meilisearch`, the audit's power loss of the service | `ERROR`: Meilisearch does not start again after its power loss, with the same log line | `97f757a448812aad` |
| `disk=cut \| run K=meilisearch@0+2 C=4` on the image built with `--set fresh=no` (`ef1a5a466e81d1f1`) | OK | `1d1aed836a03780a` |

In the recovery boot every start of Meilisearch exits with status 1 and the log line
`Meilisearch (v1.57.0) failed to infer the version of the database`. The schedule is saved as
`fresh/schedules/new-database-unstartable-after-power-cut.schedule`.

**The cause in the code** (the tag of v1.57.0, and `main` at `9fb6a904` on 2026-11-27):

- At every start `open_or_create_database_unchecked` (`crates/meilisearch/src/lib.rs`, line 474)
  calls `create_current_version_file`. It writes the version into a `NamedTempFile` in `data.ms`,
  calls `flush`, which syncs nothing, and `persist`, a rename.
- A new database's `VERSION` is a new name in the folder. The other files of the database are
  LMDB's, which syncs at each commit. On ext4 such a sync commits the journal, and with it the
  rename of `VERSION`. The data of `VERSION` is written only when the kernel writes it back,
  about 30 s later. So a power cut in the first 30 s leaves `VERSION` empty or missing.
- At the next start `is_empty_db` sees a folder that is not empty, so `check_version` reads
  `VERSION`. `get_version` returns `MissingVersionFile` for an empty or missing file, and
  Meilisearch exits. Every acknowledged task of the new database is out of reach until an
  operator writes the file by hand.
- An existing database rewrites `VERSION` at every start through the same rename over the old
  file. ext4 writes the new file's data before such a rename commits, so a later cut leaves the
  old or the new version.

## The duplicate searches

`gh search issues --include-prs` over meilisearch/meilisearch, open and closed, on 2026-11-17,
and the discussions of meilisearch/meilisearch and meilisearch/product:

| phrase | found |
|---|---|
| swap, swap-indexes, swapIndexes, indexSwap | #6730, #6059, #4926, #6188, #4040, #2969, #2970, #2975, #4103: none about a crash or a second run |
| swap crash, swap restart, swap atomic, swapped back, swap twice | nothing |
| processed twice, task processed again, reprocessed, task executed twice, twice, exactly once, idempotent | #1679 (the 2021 design note of finding 1), #6512 and #4586 (other causes) |
| crash restart task, task status commit, LMDB transaction, write transaction task, processing reset enqueued | nothing |
| fsync, sync_all, power loss, power failure, power outage, hard reboot, unexpected shutdown, durable | nothing on the cause; #6730 and #4793 on other topics |
| persist, NamedTempFile, file store, update file, update_files, payload | #6097 and #6048 (orphan payload files after an error), #2472 (update files in snapshots), #3528, #5148, #3164: none about a sync |
| ENOENT, os error 2, No such file or directory, Can't access update file | #2085 (closed 2022, a missing update folder at start), #3256, #1701: other causes |
| indexedDocuments 0, receivedDocuments, empty payload | #5251, #5190 (internal errors with other causes), #5192, #1987 |
| dump, dumps, dump empty, dump file corrupted, dump 0 bytes, snapshot empty, snapshot corrupted | #6597 (a dump missing in Docker, no power cut, cause unknown), #4158, #4156, #4596, #5266, #5317: none about a sync |
| crash, corrupted, restart lost, documents disappeared, MDB_CORRUPTED | #5396, #672, #4437, #2672, #6575: other causes |
| discussions: swap, crash; product: swap indexes | product#834, #814, #79, #607, #585: no crash case |

`crates/file-store/src/lib.rs` and `process_dump_creation.rs` have no sync on `main` either.

The phrases of finding 5 were searched on 2026-11-27 with the same command:

| phrase | found |
|---|---|
| `VERSION file`, `version file` | #4584 (closed 2025, not reproduced): a new database with an empty `VERSION`, "We could have missed a disk sync or something". #6202 (closed): an empty `VERSION` on Kubernetes, fixed by #6275, which changed only the error. #6249 (closed): another fix of the message. #5280, #3069, #2166: other causes |
| `failed to infer the version of the database` | #2503, #4917, #2389, #2292: upgrades and dumps; #6258: a panic on virtiofs |
| `MissingVersionFile`, `data.ms VERSION` | #6202, #6249, #6275 |
| `VERSION file empty`, `version file power`, `VERSION file missing`, `version file corrupted`, `startup failure VERSION`, `Kubernetes VERSION empty`, `new database power`, `first start crash`, `fsync VERSION`, `NamedTempFile persist sync`, `power outage` | nothing |
| `power loss` | #6749 (open): findings 2 and 3 of this page |
| `create_version_file` | #1860 (2021): the design of the file |

## The explorations

All on the Mac engine with one worker, guest RAM 1,536 MiB, and the reactions
`--reactive fault --react-events meili.add_acked,meili.swap_acked,meili.edit_acked` (A also
`meili.dump_acked`). Every exploration had `--new-faults --storage-faults`.

| exploration | image | strategy, seeds | failures by cause |
|---|---|---|---|
| A | the default, with dumps | process, 1-25 | 9 finding 3 (seed 1 also finding 2 in its output); 4 damage that Meilisearch detects at the start (`MDB_INVALID: File is not an LMDB file`, a corrupted version file), class `corrupted-files` |
| B | `--set dumps=no` | process, 26-50 | 1 finding 2 (seed 26) |
| C | `--set dumps=no` | swarm, 1-25 | 14, each with a disk fault or a damaged file: 6 with a lying disk (3 committed data that cannot be recovered, 2 answers `500` to the checks, 1 counter of edits above its succeeded edits), 7 with disk I/O errors (restarts that fail, answers `500`, a storage that does not mount after the power loss), 1 detected damage. Stopped after the 25 schedules, before the shrink |

Exploration A ran its 25 schedules in 200 s (450 runs an hour) and shrank its two signatures
of finding 3 in 92 and 93 runs. The minimal schedule of the first is one power loss of the
service, `L=meilisearch@42+1`. B shrank seed 26 in 91 runs to `C=60 A=meili.add_acked:cut`
with only `parallel_driver_add`. No minimal schedule kept a clock step, a kill, or a pause.
The blind explorations did not reach finding 1: the explorer draws a reaction's delay from
fixed values, and none of them came between the swap's two commits.

Since Milestone 309 a blind exploration reaches it with the kill at the event's moment. With
`--strategy process --reactive fault --react-events
meili.swap_acked,meili.edit_acked,meili.add_acked --kill-now`, without `--new-faults` and
`--storage-faults`, on the default image:

- 101 of the seeds 1 to 360 drew a `kill-now-meilisearch` reaction.
- The swap ran twice at 6 seeds, the first at seed 101, and an edit ran twice at 5 seeds.
- Seed 101 shrank in 88 runs to one reaction, `A=meili.edit_acked:kill-now-meilisearch`, with
  no other fault and three of the client's commands left out of the composition
  (`fp=e8098215bf80d4e0`). `docs/HISTORY.md` Milestone 309 has the whole schedule.

Directed runs, before the explorations:

| schedules | result |
|---|---|
| `disk=none \| run`, 4 runs | `OK`, one fingerprint per image (one first run broke `final_docs` by a mistake of the rule: the search `w3` also found the prefix `w30`; fixed with a phrase) |
| `A=meili.add_acked@20+2:cut` and `+40`, `disk=cut` and `disk=none` | findings 1 and 2 (and 3 with dumps) |
| `C=30`, `A=meili.dump_acked@5+3000:cut` | finding 3 |
| `K=meilisearch@20+3`, `T=20:-3600` | `OK` |
| `A=meili.swap_acked@20+<ms>:kill-meilisearch`, 1 to 25 ms (9 delays); `A=meili.add_acked@20+<ms>:kill-meilisearch`, 1 to 6 ms (5 delays, one twice); `A=meili.edit_acked@20+<ms>:kill-meilisearch` (4 delays) | `OK` |
| `A=meili.swap_acked@20+3:cut` | findings 2 and 3, not 1 |

## What hydo lacked

None of these is fixed here; open item 148 in AGENTS.md lists them.

1. **A kill reaction does not come between two commits that a power cut finds.** Finding 1 needs
   the server stopped between two LMDB commits. A plain SIGKILL does it in Docker, and in hydo
   only a power cut did. `kill-<svc>` reaches the server through the guest agent, so the
   server's threads run on until the agent is scheduled. 18 kill delays passed. Fixed in
   Milestone 305: `kill-now-<svc>` broke the swap at 13 and 14 ms. Since Milestone 309 `hydo
   explore --kill-now` draws such kills on this one service, and it found the swap blind.
2. **One schedule's first violation hides the others.** Every power cut within 30 s of a dump
   breaks `final_dumps`, so most failing schedules of the explorations carry finding 3, and
   the verdict line of finding 1's schedule names finding 2. The swap shows only in
   `hydo report --details`. A swap-only regression would need a workload without dumps and
   additions, which then does not open the window.
3. **The moment of a cut after an event moves.** Released 0.3.4 builds an epoch-3 image whose
   timing differs, so its cuts at `+2` and `+40` miss finding 1; a reader needs this commit's
   hydo. The example's own change moved it too: the build parameter `dumps` changed the client,
   and finding 1 moved from `+2` to `+40` ms.
4. **The shared Mac VM lock.** The onboarding waited about 1 hour 40 minutes for the lock,
   while other agents' explorations held it, against about 50 minutes of its own builds, runs,
   and explorations.

## Time

The onboarding started at 17:01 CET on 2026-11-17 and used this tree's `hydo` over the 0.3.4 Mac
release, on this Mac's engine only.

| phase | wall time | of it waiting for the Mac VM lock |
|---|---|---|
| 1. Scouting: the documentation, the code of the file store, the queue and the dumps, the clock audit | 15 minutes | none |
| 2. Setup to the first clean run, twice | 32 minutes | about 28 minutes |
| 3 and 4. Directed runs, the kill scans, the native reproduction, the fact sheet of finding 1 | 85 minutes | about 70 minutes |
| 3. The three explorations | 27 minutes | none |
| 5. The duplicate searches and the write-up | 60 minutes | none |
