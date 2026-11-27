# Typesense under hydo: a cluster of three Raft nodes

Typesense (github.com/typesense/typesense) is a search engine that keeps its documents in
RocksDB and its search index in memory. A cluster of three nodes replicates every write through
Raft, with the braft library: a write goes to the leader, any node forwards it there, and each
node applies the committed writes to its own store and index. This example runs such a cluster
from the official image in one guest of hvx on the bare-metal machine, and looks for bugs that
nobody reported.

## The short answers

- **What was found?** One new bug that needs no clock step, and two new clock effects. Nothing
  was posted.
  1. **New: a node that replays its Raft log applies a delete by a filter in several batches out
     of order with the writes after it, and it then lacks documents for good.** A delete by a
     filter runs its first batch in the write's turn and its later batches from a timer on
     another thread. A node that replays its log after a kill or a power cut does not wait for
     them, applies the next writes, and the later batches then delete the new versions. The node
     answers searches and reads without those acknowledged documents, while the other nodes have
     them. A kill of one node is enough, the leader or a follower
     (`regressions/follower-lacks-documents-after-a-restart.schedule`, whose `ts3` is the leader
     when it is killed; the section below has the follower kills).
  2. **New, a clock effect: after a backward step of the wall clock, every answer that Typesense
     makes in several parts stops after its first part, for the step's length.** An export with
     its default batch of 100 documents ends after the first batch, and a delete by a filter with
     more matches than its batch size deletes one batch and then nothing, on every node. The
     HTTP server makes each later part from a timer of its event loop, and that loop fires no
     timer while the wall clock is behind its last run (h2o's open issue h2o/h2o#2321)
     (`regressions/export-and-batched-delete-stop-after-a-backward-clock-step.schedule`).
  3. **New, a clock effect: after a backward step of the wall clock, a failure of the leader is
     followed by no election for the step's length.** braft's election timer runs at wall-clock
     times, so its next run comes as late as the clock went back. A pause of the leader 10 s
     after a step back of 120 s left the two other nodes without a leader for 68 s
     (`regressions/no-election-after-a-backward-clock-step.schedule`).
- **What held?** Every write that Typesense acknowledged was on the leader at the end, unless a
  later write of the same document replaced it, and no refused write took effect. That held for
  creates, upserts, partial updates, deletes, deletes and updates by a filter, imports of 2 to 100
  documents and of request bodies in several chunks, and schema changes. It held under kills,
  pauses, partitions and one-way loss, one node's power loss or damaged files, power cuts of the
  whole guest, and clock steps. No read during the run returned a version that no write sent.
- **What is the environment?** A node whose files `G=` damaged stopped at its start, or stayed
  up and never became healthy. The other two nodes went on serving.
- **What did hydo lack?** The section "What hydo lacked": the console of hvx kept 64 KiB without
  a warning, the scan of write routes found none of Typesense's 57, and a group's `primary:`
  failed during an election.

## The version

- Typesense 30.2, the newest release (2026-04-19), the tag `v30.2` at `d45d46ba`. The official
  image `typesense/typesense:30.2`: index
  `sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110`, linux/amd64
  `sha256:b405d4443bdb9254fab4792b7fd9ee95d969f3b5485975917a3a3f84274eba27`.
- The causes were read at `v30.2` and on the default branch `v31` at `319f73ec` (2026-11-23),
  which pins the same h2o (`1491a703`), braft (`bc527db9`) and brpc (`df31bf51`) and has the
  same code of each cause.
- hydo: `main` at `8a60fcb2` (0.6.0 and Milestones 362 to 365), built on the bare-metal machine,
  with the 0.6.0 release's guest files. The engine is hvx, with up to 4 VMs of 2,048 MiB at once.

## The promises

The promises come from Typesense's guide on high availability and its API reference. No owner of
Typesense confirmed them; the ones marked "unconfirmed by the owner" are read from the code.

| promise | source |
|---|---|
| a cluster of 3 nodes keeps serving reads and writes with 1 node down | the guide: "A Typesense cluster with N nodes can tolerate a failure of at most `(N-1)/2` nodes without affecting reads or writes" |
| any node takes a write and forwards it to the leader | the guide: "write API calls are automatically forwarded to the leader of the cluster internally" |
| an acknowledged write survives a failure of a minority of nodes | Raft; unconfirmed by the owner |
| every node ends with the same documents | Raft; unconfirmed by the owner |
| a search finds a stored document with its stored version | unconfirmed by the owner |
| an export returns every document of the collection | the API reference: "Export documents in a collection in JSONL format" |
| a delete by a filter deletes every match, `batch_size` at a time | the API reference, "Delete by query" |

## The project

`docker-compose.yaml` runs three nodes, `ts1`, `ts2` and `ts3`, at the fixed addresses
10.89.0.11 to 10.89.0.13. Each node has a volume for `/var/lib/typesense` and reads the peers from
`config/nodes`: `10.89.0.11:8107:8108,10.89.0.12:8107:8108,10.89.0.13:8107:8108`, the peering
port and the API port of each. `hydo compose` imported the file (`hydo/hydo.yaml`), with these
changes:

- each node's `/var/lib/typesense` is a file system of its own (`storage:`), the target of `L=`
  and `G=`;
- the readiness check asks `/health` with the test client and takes any answer, because a node
  alone has no quorum and is never healthy;
- the nodes start together (`start_after: started`), as Compose starts them;
- `--snapshot-interval-seconds` is the build parameter `snap`, 30 s instead of an hour, so that
  each node takes about one snapshot in a run; a node checks the interval once a minute;
- the test client `tsc` is in the image.

The data directory is the volume's root. The first import named `/var/lib/typesense/data`, and
Typesense stopped at its start with `Data directory /var/lib/typesense/data does not exist`.

## The workload

`client/` is the Go client `tsc` on `net/http`, which sends each request once (`sh build.sh`
builds it). Each write goes to a node that the seed picks, so most writes take the forwarding
path. Its writes, one command file each in `hydo/workload/`, all to one collection `docs`:

- `create`: `POST /collections/docs/documents` of a new document;
- `upsert`: `POST ...?action=upsert` of a known document, a deleted one too;
- `update`: `PATCH /collections/docs/documents/<id>`;
- `delete`: `DELETE /collections/docs/documents/<id>`;
- `fdelete` and `fupdate`: a delete and a `PATCH` by a filter on the document's id and its last
  acknowledged version (`filter_by=id:=<id> && tag:=<version>`), which act as a compare-and-set;
- `bdelete`: a delete by a filter on the tag of an earlier import, with `batch_size=2` (the
  build parameter `batch`), of at least 3 documents;
- `import`: `POST .../import?action=create` of 2 to 12 new documents; one import in eight carries
  100 documents, an answer of more than one batch of 40 lines, and one in eight 20 documents of
  12 KB, a request body of more than one chunk of 196,605 bytes;
- `alter` (a serial driver): `PATCH /collections/docs` that adds an optional field.

A document is a register: its value is the tag of the write that last set it. One command at a
time works on a document, and a document whose write got no certain answer is never written
again. Each write is recorded in `$HYDO_ACKED`, `$HYDO_OUTCOMES` and the history `writes`. A 2xx
answer acknowledges. 400, 404, 409, 422, and 503 `Not Ready or Lagging`, mean that Typesense did
nothing, so the write failed. Every other answer, a timeout, and no answer give an unknown outcome. The
command `anytime_leader` asks every node's `/debug` once a second for 12 s and records which node
is the leader.

`hydo rules scan --kind writes` found no route (section "What hydo lacked"). Typesense registers
57 routes that write in `src/main/typesense_server.cpp`. The workload covers the documents and
the schema. Aliases, synonyms, curations, API keys, presets, stopwords, analytics rules, and the
conversation and embedding models have no command.

## The rules

| check | promise | file |
|---|---|---|
| `writes` (`type: messages`, `acked_outcome` with `effect: recorded`) | every acknowledged write is on the leader at the end, unless a later write of its document replaced it; no refused write took effect | `hydo.yaml`, `tsc readback` |
| `eventually_converge` | within 120 s of the heal every node is healthy and holds the leader's documents, each with the leader's version | `checks/eventually_converge` |
| `eventually_serves` | within 120 s of the heal a write through each node is acknowledged, and then readable on every node within 15 s without another write | `checks/eventually_serves` |
| `eventually_export` | within 120 s of the heal an export with the default batch size ends on every node with every stored document | `checks/eventually_export` |
| `eventually_bdelete` | within 120 s of the heal every delete by a filter whose answer the client did not get has deleted all of its documents or none | `checks/eventually_bdelete` |
| `final_search` | on every node a search for everything finds exactly the stored documents with their stored versions, and a search for a document's tag finds it | `checks/final_search` |
| `final_schema` | every field that an acknowledged schema change added is in every node's schema | `checks/final_schema` |
| `final_available` | while two or more nodes answered, the cluster was never without a leader for more than 30 s | `checks/final_available` |
| no phantom read | a read of any node returns only versions that some write sent | `anytime_read` |

The read-back and `eventually_converge` export each node in one batch
(`batch_size=1000000`), so that finding 2 breaks the export check and not the read-back. The
read-back first upserts a barrier document through the leader. Typesense applies the writes of
one collection in log order, so the barrier's answer comes after every earlier write is applied
there. The later batches of a delete by a filter are the exception (finding 1).

## Without faults, and the regressions

The final image `explore-ob-typesense-disk.img` (`02024a5f…`, the fingerprint epoch 6) gives one
fingerprint, `097b8fcffac8545b`, on two runs of `disk=none | run`, which ran 134 commands without
a failure. A run has a few hundred writes. Each regression gave one fingerprint over two fresh runs,
and `hydo repro --regressions` gave 3 of 3 as expected, with the same values:

| file | finding | fingerprint |
|---|---|---|
| `follower-lacks-documents-after-a-restart.schedule` | 1 | `2d653b8a28652520` |
| `export-and-batched-delete-stop-after-a-backward-clock-step.schedule` | 2 | `c3653bc7d18e8f42` |
| `no-election-after-a-backward-clock-step.schedule` | 3 | `8675611411642625` |

**With the released hydo 0.6.0, from a fresh clone.** On hvx, a clone of this repository at
`0fe1e269` and the published 0.6.0 Linux release (`12fe3299…`, checked by `SHA256SUMS`) built the
image `de22e524…` with the example's own steps. Its client, built from the committed
`client/main.go` (`tsc-amd64` `5109fe4d…`), differs from the onboarding's (`3dc4d230…`) in the
line numbers stored in the program, so its fingerprints differ from the table above:

| schedule | image | verdict | fingerprint |
|---|---|---|---|
| `K=ts3@40+20 W=5479b87f,p6,q1,r500` | default | `converge`: `ts3` lacks `i4x15` and `i4x39` | `881d375553ca3742` (two runs) |
| the same | batch 1e9 | OK | `6853dac15089c3f5` (two runs) |
| `W=5479b87f,p6,q1,r500` without `K=` | default | OK | `7dd9c5fa34b400d5` (two runs) |
| `K=ts1@40+20 W=5479b87f,p6,q1,r500` | default | `converge`: `ts1` lacks `i4x15` and `i4x39` | `f7e81ca6d9185872` (two runs) |
| `K=ts2@40+20 W=5479b87f,p6,q1,r500` | default | `converge`: `ts2` lacks the same two | `fadaff36e14c4c9d` |
| `K=ts1@40+20 W=5479b87f,p6,q1,r500` | batch 1e9 | OK | `754d4b64dd9c7592` |

The client's probes show `ts3` as the leader (braft state 1) and `ts1` and `ts2` as followers
until `ts3` goes down, so the saved regression kills the leader, and the `ts1` and `ts2` rows
kill a follower. At the run's end the restarted node answers `/health` with `{"ok":true}` and a
read of either document with 404, while the other two nodes answer 200 with the acknowledged
version.

The runs that separate the causes, one run each unless two are named. The image "batch 1e9" is
a copy of `hydo.yaml` built with `--set batch=1000000000`, so that every delete by a filter runs
in one batch. The image "earlier" is the one before the parameter, whose client always sent
`batch_size=2`.

| schedule | image | verdict | fingerprint |
|---|---|---|---|
| `K=ts3@40+20 W=5479b87f,p6,q1,r500` | final | `converge`: `ts3` lacks 2 documents | `2d653b8a28652520` (two runs) |
| the same | batch 1e9 | OK | `e56f2f1ad3e6bd3e` |
| `K=ts3@50+3 W=5479b87f,p6,q1,r500` | earlier | `converge`: `ts3` lacks 1 document | `46f666ec7979a7dc` |
| the same | batch 1e9 | OK | `dd4d9271f71694dc` |
| `W=5479b87f,p6,q1,r500`, no fault | earlier | OK | `45ee8cc0ee9258a0` |
| `K=ts3@40+20`, the default composition | final | OK: the composition has no delete in batches before the kill | `6f1f1bfc72678aae` |
| seed 49 of e2 with `disk=none`: `K=nodes.minority.1@15+1 C=60 T=49:-30 W=757a66d9,p4,q1,r500` | batch 1e9 | OK | `804e8ce9dc1183f2` |
| seed 53 of e2: `K=ts1@12+12 N=nodes.halves@20-21:delay,500ms P=ts3@40+19 W=bacf015b,p4,q3,r500` | batch 1e9 | OK | `4fd6abfdb1938fcd` |
| `T=20:-3600` | final | `bdelete`, and `export` | `c3653bc7d18e8f42` (two runs) |
| `T=20:3600` | final | OK | `ea0a703cac10fe5b` |
| `T=20:-120 P=nodes.primary@30+25` | final | `available`: no leader for 68 s | `8675611411642625` (two runs) |
| `P=nodes.primary@30+25` | final | OK | `4ca5a7a3bc371ef9` |
| `T=20:-120 K=nodes.primary@30+25` | final | `bdelete`, `available` (no leader for 36 s), `converge`, `export` | `f15de817eee56880` |

## What was found

### 1. New: a node that replays its Raft log applies a delete by a filter in batches out of order, and lacks documents for good

- **Where.** Any node that replays its Raft log: a node after a kill, a power loss of the node, or
  a power cut of the guest. A node without a snapshot replays its whole log; one with a snapshot
  replays the entries after it.
- **The schedule.** `disk=none | run K=ts3@40+20 W=5479b87f,p6,q1,r500`: a kill of the follower
  `ts3` before step 40, which starts again before step 60. The composition (`W=`) deletes earlier
  imports by a filter. After that, it upserts and updates the deleted documents.
- **What breaks.** After the heal `ts3` lacks documents that the leader and the other follower
  hold, each with an acknowledged version. `eventually_converge` fails for the 120 s after the
  heal: `ts3 lacks i4x15, which the leader ts1 holds as w12` and `ts3 lacks i4x39, which the
  leader ts1 holds as w21`. `ts3` is healthy, so it answers reads and searches without them. The
  writes of `i4x39` were all acknowledged: the import (`w4`), the delete by a filter of its
  import's tag, an upsert that created it again (`w8`), and an update by a filter (`w21`).
- **The other two faults of the blind schedule are not needed.** The explorer found it at seed
  43, `K=ts3@40+20 N=nodes.halves.2@34-44:loss,100% P=nodes.primary@20+1 W=5479b87f,p6,q1,r500
  L=ts3@56+3`. The kill alone fails as described, and so do `K=ts3@40+20 L=ts3@56+3` and the shorter
  `K=ts3@50+3`. The composition without a fault passes.
- **The delete in batches is needed.** The same four failing schedules, on an image built with
  `--set batch=1000000000` (one batch for every delete, Typesense's default `batch_size`), pass.
  That covers the kill of `ts3`, `K=ts3@50+3`, the blind seed 49 with a power cut (run with
  `disk=none`) and the blind seed 53 with a kill of `ts1`.
- **The cause.** A delete by a filter is a streaming handler. `del_remove_documents` deletes one
  batch (`src/core_api.cpp` line 2166) and, when matches are left, calls `defer_processing(req,
  res, 0)` (line 2196). That posts the request to the HTTP thread, which links a timer of its
  event loop (`src/http_server.cpp` line 892). The timer's callback runs the handler again for
  the next batch on a thread pool (line 867). The batched indexer, the thread that applies the
  collection's writes in log order, called the handler once (`src/batched_indexer.cpp` line
  299), and goes on to the next write at once (lines 338 to 343). It does not wait for the later
  batches. A live delete on the leader hides this, because the client's next write of those
  documents waits for the delete's answer, and the answer comes after the last batch. A replay
  applies the next entries at once: the upsert that created `i4x39` again updates the old
  document in place, and then a later batch, which holds the old document's internal id from the
  filter, deletes it. Typesense deletes at most 1,000 documents a batch whatever `batch_size`
  says (`INTERNAL_REMOVE_BATCH_MAX`, `src/core_api_utils.cpp` line 6). So a delete by a filter
  of more than 1,000 documents runs in several batches with the default `batch_size` too.
- **The consequence.** A node that restarted after a delete by a filter of more than one batch
  can lack acknowledged documents, or hold a version that a later write replaced, for good. Reads
  and searches that a client sends to that node, as Typesense's clients send reads to every node,
  miss them. A later leader election can make that node the leader. In the blind seed 49 the
  leader after the power cut lacked a document that both followers held, and in seed 53 a node's
  search index named an older version of a document than its store (`ts1 stores i0x6 = w93, and
  a search finds it as w9`).
- **Duplicates.** No issue names a delete by a filter during a replay. #3181 (open, 2026-11-17)
  reports `num_documents` that differs between the nodes of a cluster at one committed index,
  under deletes by a filter and restarts, with `async_reference` fields; it names no cause. The
  section "The duplicate searches" has the phrases.
- **No native run.** The cause is plain in the code, and the A/B runs with one batch
  separate it.

### 2. New, a clock effect: after a backward step of the wall clock, an answer in several parts stops after its first part

- **The schedule.** `disk=none | run T=20:-3600`: the wall clock goes back by an hour before step
  20. The run passes without `T=` and with `T=20:3600`, a step forward.
- **What breaks.** Two checks, for the step's length:
  - `eventually_export`: an export with the default batch size does not end on any node. A
    `wget` of `/collections/docs/documents/export` stopped after the first batch of 100 documents,
    and an export with `batch_size=100000`, one batch, ended at once.
  - `eventually_bdelete`: every delete by a filter that the client sent after the step deleted
    its first batch of 2 documents and then nothing (`the delete of the documents with w46
    removed 2 of 15`). The client got no answer within 10 s. A delete with `batch_size=1` sent
    by hand at the end of the run deleted 1 of 8 documents and answered nothing for 15 s; the
    same delete without the step deleted all 8 at once.
- **More schedules.** Every backward step longer than about 75 s broke `eventually_export` in the
  first exploration, and the shrinker reduced 5 failures of seeds 3, 10, 14, 24, and 29 to one
  step each, the shortest `T=57:-150`. `T=57:-75` passed: the export ended 75 s after the step,
  inside the check's 120 s. In the second exploration the batched deletes stopped part way on
  each node at another point, so after a step back the nodes held other documents
  (`eventually_converge` failed in seeds 42, 45, 48, 59, 65, 66, and 72).
- **The cause.** Typesense's HTTP server is h2o at `1491a703`. h2o's event loop reads the time
  with `gettimeofday` (`lib/common/socket/evloop.c.h` line 792), the wall clock. Its timer wheel
  returns no expired timer while the time is before its last run (`lib/common/timerwheel.c`
  lines 304 to 308, which print `detected rewind`). Typesense makes each later part of a
  streaming answer from such a timer:
  - an export: when h2o has sent a part, `response_proceed` calls `defer_processing(...,
    1)` (`src/http_server.cpp` line 945), a timer of 1 ms that calls the handler for the next
    batch;
  - a delete by a filter: the handler calls `defer_processing(req, res, 0)` for the next batch
    (`src/core_api.cpp` line 2196).

  After a step back of an hour, no timer fires for an hour. Each node's log has thousands of
  lines `h2o_timerwheel_get_expired:detected rewind; last_run=1795651230720,
  now=1795647631006`. Answers in one part, the writes, the searches, the reads, and imports of
  100 documents, were not affected.
- **The consequence.** For the length of a step back, a backup by export ends after its first
  100 documents and a client waits on it until its own timeout, and a large delete by a filter
  stops part way on every node, each at its own point, and answers nothing.
- **Duplicates.** h2o's open issue h2o/h2o#2321 (2020) describes the timers of h2o's event loop
  on `gettimeofday` and proposes a monotonic clock; the pull requests h2o/h2o#2339 and #2822 that
  add one are open. No Typesense issue names a clock step, `detected rewind`, or an export that
  stops; #830, an export that stopped after 25,000 documents, was a load balancer's timeout.
- **A clock effect.** NTP or an operator can step a real server's clock back, for example after
  the server ran with a wrong time. The promise broken is the export's, every document, and the
  delete's, every match.

### 3. New, a clock effect: after a backward step of the wall clock, a failure of the leader is followed by no election

- **The schedule.** `disk=none | run T=20:-120 P=nodes.primary@30+25`: the wall clock goes back
  by 120 s before step 20, and the leader (`ts3` in this run) is paused with SIGSTOP from step 30 to step 55.
- **What breaks.** `final_available`: `from 43.8 s to 112.7 s of uptime no node was the leader
  while two or more answered (ts1=4,ts2=4,ts3=-1)`, so `no leader for 68 s`. The two other nodes answered `/debug` as followers (`state 4`) for the whole pause, and
  writes through them got no answer. Without `T=` the same pause gave a new leader, and
  `final_available` held. On an earlier image, with `T=20:-3600`, the two nodes stayed followers
  for 289 s, until the pause ended. A kill instead of the pause, `T=20:-120
  K=nodes.primary@30+25`, left the cluster without a leader for 36 s.
- **The cause.** braft at `bc527db9` runs the election timer, the vote timer and the step-down
  timer as `RepeatedTimerTask`s. Each run schedules the next at
  `butil::milliseconds_from_now(timeout)`, a wall-clock time (`src/braft/repeated_timer_task.cpp`
  lines 128 to 130). brpc's timer thread at `df31bf51` compares the due times with
  `butil::gettimeofday_us()` (`src/bthread/timer_thread.cpp` lines 372 and 427). So the run that
  was due when the clock went back comes as late as the clock went back. A follower then starts
  no election however long its leader is silent. The election timeout is 5,000 ms
  (`src/typesense_server_utils.cpp` line 416).
- **The consequence.** For the length of a step back, a failure of the leader makes the cluster
  refuse writes, where the guide promises that a cluster of three serves with one node down. A
  killed leader that starts again has new timers and ends the wait, so the outage lasts as long
  as the leader is down or the step, whichever is shorter.
- **Duplicates.** No issue of Typesense, braft or brpc names a clock step and the election timer.
  braft's issue baidu/braft#50 is about the step-down timer's log lines, not about the clock.
- **A clock effect.** As finding 2, a step back by NTP or an operator is enough.

### Not bugs

- **Detected damage.** In the two explorations, 14 schedules damaged a file of one node with
  `G=`: RocksDB's `MANIFEST`, `OPTIONS` or SST files, or braft's `raft_meta`. That node stopped at
  its next start (`ERROR ts3 restart failed (its files were corrupted)`), or it stayed up and
  never became healthy (`/debug` state 0, 5 or no answer). The other two nodes went on serving
  and kept every acknowledged write.
- **The primary of a group during an election.** In the first exploration, seed 22 killed the
  leader `ts3` and aimed a partition at `nodes.primary` at the same step. hydo asked the group's
  `primary:` command 10 times a second apart, the election took longer, and the run ended
  `ERROR nodes.primary: the group's primary: command named no member in 10 tries`. The command
  `tsc primary` now waits up to 15 s for a leader itself.
- **Forward steps.** Steps forward of 300 s to a year broke nothing. The h2o timers and braft's
  timers fire early after a forward step, which ends a wait early and nothing else here.

## The duplicate searches

`gh search issues --include-prs`, open and closed, on 2026-11-26:

| repository | phrase | what it found |
|---|---|---|
| typesense/typesense | `delete by filter replica`, `delete by filter follower`, `replay delete`, `defer_processing`, `batch_size delete`, `delete by filter hangs` | nothing |
| typesense/typesense | `documents missing on follower`, `follower missing documents`, `nodes out of sync`, `different document count nodes`, `num_documents differ` | nothing |
| typesense/typesense | `Document fetch error`, `Could not locate the JSON document` | #3028 (concurrent upserts and searches miss hits), #2789 (closed, the same symptom), #3166 (a test of stale filter matches), #3181 (`num_documents` that differs between nodes, with `async_reference` fields and restarts; no cause named) |
| typesense/typesense | `detected rewind`, `timerwheel`, `clock`, `system clock`, `NTP`, `time change` | nothing about a clock step |
| typesense/typesense | `export hangs`, `export stuck`, `export incomplete`, `export timeout`, `export stops`, `export` | #830 (closed: an export that stopped at 25,000 documents behind a load balancer's timeout of 30 s), #743, #1067, #2240 (slow or large exports) |
| typesense/typesense | `Could not find a leader`, `no leader`, `leader election`, `election timeout`, `heartbeat` | #2155 and #2238 (no leader after the nodes' addresses change), #3014 (a stale node list in a configuration change), #412, #904; none about the clock |
| h2o/h2o | `rewind`, `clock_gettime`, `monotonic clock`, `gettimeofday monotonic` | #2321 (open since 2020: the event loop's timers on `gettimeofday`), #2339 and #2822 (open pull requests that add a monotonic clock) |
| baidu/braft | `clock`, `time jump`, `gettimeofday`, `system time`, `ntp`, and the Chinese words for the time, a clock step back and a change of the system time | #50 (the step-down timer's log lines), #532 (heartbeats under load); none about the clock |
| apache/brpc | `timer gettimeofday`, `system time change timer`, `clock jump`, `monotonic timer_thread`, and the Chinese words for the system time and a clock step back | nothing about the timer thread and a clock step |

## What held, and why

- braft syncs its log before a write counts (`raft_sync`, on by default), and Typesense answers
  a write after the leader applied it, so no kill, power loss or power cut lost a write that the
  leader acknowledged.
- A follower forwards a write to the leader and returns the leader's answer, so a forwarded
  write had the leader's outcome. A forward that the leader never answered was an unknown
  outcome, never a false acknowledgement.
- Imports of 100 documents and request bodies of 240 KB in two chunks applied whole or not at all
  on every node, also across kills and power cuts.
- Every acknowledged schema change was in every node's schema after the heal.

## The explorations

| name | options | seeds | schedules | failures |
|---|---|---|---|---|
| e1 | the first image (no batched delete, no leader probe, a converge check that exported in batches of 100); process strategy, `--new-faults --cluster-faults --storage-faults`, 3 workers | 1-30 | 30 | 10: finding 2's export (5, each shrunk to one step back), detected damage (4), the group's primary during an election (1) |
| e2 | the image with every check; the same options | 31-90 | 60 | 22 first runs: finding 1 (3 without a step back: seeds 43, 53 and 61), finding 2 and the nodes it leaves apart (8, all with a step back), detected damage (10), and seed 87, a forward step with an export that held an error line, not judged |

e1 ran 30 schedules, 10 reruns and the shrinks of 5 failures in 593 s, at 254 runs an hour. e2
was stopped after its first runs, during its shrinks, so it wrote no report. The directed runs of
the section "Without faults, and the regressions" separated its failures instead.

## What hydo lacked

In the order of the time each cost:

1. **The console of hvx kept 64 KiB without a word.** The first client wrote one history record
   per document and one assertion record per write. The records stopped at 15 s of uptime, after
   about 230 writes, and the messages check said that no acknowledged write was read back,
   although the read-back had written 1,813 status items. `hydo guide hazards` says that `hydo
   run` warns when a run writes more, and no run warned; `engine.log` said nothing either. An
   inspect session showed the read-back's output. The client now writes short ids and one record
   per import (open item 181 has the same gap of the Pulsar example). Since Milestone 368 a cut
   console warns in `hydo run`, `result.json` and `hydo report`, and a messages claim that the
   missing records break is an ERROR that names the cut.
2. **One history record per process at a time.** An import recorded one `send` per document
   from one process. The check counted 337 of 352 sends as unknown, because a process has one
   operation in flight. `hydo guide messages` says so for consumers; it does not say that a batch
   is one `send` with several items. The client now writes one record per import. Since Milestone
   368 a process token `<process>#<op>` gives each operation of one process its own completion.
3. **A group's primary during an election.** hydo asks the `primary:` command 10 times, a second
   apart (`PROBE_TRIES`), and ends the run in an ERROR when no member is named. A Raft election
   after a kill of the leader took longer. The example's command waits 15 s itself. Since
   Milestone 368 hydo asks 30 times, and the ERROR names the likely cause.
4. **The shrinker kept a damage that the failure did not need.** Seed 3's minimal schedule was
   `T=35:-21600 W=... G=ts3@8+3:zero,len=512,seed=951ce4e8`, and its class `corrupted-files`. The
   shrinker tried to leave out `T=`, which passed, and never tried to leave out `G=`. Without it
   the step alone fails (the regression of finding 2). Since Milestone 368 the shrinker tries to
   leave out each storage fault after its rounds.
5. **The scan of write routes found none of Typesense's 57.** `hydo rules scan --kind writes`
   read 24 files and listed 0 routes. Typesense registers its routes in C++ with
   `server->post("/collections/:collection/documents", post_add_document)` and its kin
   `get`, `put`, `patch` and `del` (`src/main/typesense_server.cpp`). The routes were listed by
   hand. Since Milestone 368 the scan reads them: 54 routes that write, and 3 `POST` routes whose
   names say that they read.
6. **The clock audit named none of the findings' places.** It named 14 functions of Typesense,
   search cutoffs and rate limits. The two clock effects are in h2o and braft, built from their
   sources by Bazel, outside the project's folder.
7. **Two images in one folder.** A copy of `hydo.yaml` built with `--set batch=1000000000` wrote
   a second image beside the first, and `hydo run` and `hydo repro --regressions` then stopped:
   `no image given, and there are 2 images ...; pass one with --image`. A build parameter has no
   image name of its own. Since Milestone 368 the commands take the image built from `hydo.yaml`,
   else the newest, and say which.
8. **A Compose file's fixed address reads as ignored.** `hydo compose` printed `service ts1:
   ipv4_address 10.89.0.11 ignored; the service is at 10.89.0.11` for each node. The services got
   the same addresses, from hydo's own order of `addr:`, so the warning names a difference that
   is not there. Since Milestone 368 a kept address gives no warning.

## Time

| phase | wall time |
|---|---|
| scouting (the clustering code, the issues, the target's fit) | 30 minutes |
| setup to the first clean run (the client, the import, the console's limit) | 1 hour 10 minutes |
| exploration, shrinking, and judging | 4 hours 30 minutes |
| duplicate search and write-up | 1 hour 30 minutes |
