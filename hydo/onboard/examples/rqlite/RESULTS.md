# rqlite under hydo: a cluster of three Raft nodes

rqlite (github.com/rqlite/rqlite) is a distributed relational database. Each node keeps a SQLite
database and a Raft log, with hashicorp/raft: a write goes to the leader, any node forwards it
there, and each node applies the committed log entries to its own SQLite file. This example runs
such a cluster from the official image in one guest of hvx on the bare-metal machine, and looks
for bugs that nobody reported.

## The short answers

- **What was found?** One known bug and two new candidates. Nothing was posted.
  1. **Known: a queued write with `wait` is answered 200 although the queue dropped it.** A
     follower forwards the queue's batch to the leader, tries twice a second apart, and then runs
     the success path. A kill of the leader is enough
     (`regressions/queued-write-with-wait-acked-and-lost-when-the-leader-dies.schedule`), and so
     is a delay of 500 ms on the leader's links
     (`regressions/queued-write-with-wait-acked-and-lost-behind-a-slow-leader.schedule`). The
     open pull request [rqlite/rqlite#3027](https://github.com/rqlite/rqlite/pull/3027) reports
     the same cause.
  2. **New: a graceful stop of a node loses the queued writes that it has answered.** At SIGTERM
     the node closes its write queue, and the queue's goroutine returns without writing the batch
     that it holds. Three queued writes and a SIGTERM to that node lost all three, in a run
     without any fault (`regressions/queued-writes-lost-at-a-graceful-stop.schedule`, with the
     build parameter `qstop=yes`). rqlite's page on queued writes names only a crash as the risk.
  3. **New: a node removed while it was down never joins again after its restart.** At its start
     the node hears the leader, which still replicates the removal entry to it, and the
     bootstrapper takes that as a finished boot and sends no join. The node stays out of the
     cluster for good (`regressions/node-removed-while-down-never-joins-again.schedule`). A
     power loss of a follower 10 ms after an acknowledged write and a removal while it was down
     were enough.
- **What held?** Every plain write and every transaction that rqlite acknowledged was on the
  leader at the end, unless a later write replaced it, and no refused write took effect. Every
  history of list appends and reads at level `linearizable` or `strong` was strict serializable.
  Every read of a node at level `none` was a prefix of the leader's later lists, every backup held
  the appends acknowledged before it, and every node held the leader's rows after the heal. That
  held under kills, pauses, partitions and one-way loss, one node's power loss, power cuts of the
  whole guest, clock steps, a removal of a running follower and its join after a restart, and
  power cuts, power losses and kills at the moments of acknowledgements.
- **What is the environment?** A node whose files `G=` damaged stopped at its next start, or,
  once, went on without an acknowledged delete after its Raft log was damaged (section "Not
  bugs").
- **What did hydo lack?** The section "What hydo lacked": a list-append check in a file was
  dropped without a word, no fault stops a service gracefully, no fault removes a node from a
  cluster, and a pause by steps lasted as long as a serial driver.

## The version

- rqlite 10.8.0, the newest release (2026-11-26), the tag `v10.8.0` at `bc6efce1`. The official
  image `rqlite/rqlite:10.8.0`: index
  `sha256:71b122313e060f915918225d07f802825547d4fc9cc1f7be007d6aab0b34c9ca`, linux/amd64
  `sha256:ac2e29dda37f80063076d951867f689d987f3c5584b3cfb298626d268a0775d1`. It pins
  hashicorp/raft v1.8.0 and SQLite 3.54.1.
- hydo: `main` at `e2058be0` (0.6.0 and Milestones 362 to 367), built on the bare-metal machine,
  with the 0.6.0 release's guest files. The engine is hvx, with one VM of 1,024 MiB at a time.

## The promises

The promises come from rqlite's documentation (the repository `rqlite/rqlite.io`). No owner of
rqlite confirmed them; the ones marked "unconfirmed by the owner" are read from the code.

| promise | source |
|---|---|
| a write is answered after it is committed to a majority of the nodes | the FAQ: "Normally rqlite waits until a write request has been safely committed to a majority of SQLite databases before returning a response" (Queued Writes page) |
| a request with `transaction` applies all of its statements or none | the FAQ, "a form of transactions" |
| a read at level `linearizable` or `strong` sees every write committed before it began | the page "Read Consistency" |
| a queued write with `wait` is answered when the queue has committed it; without `wait` a crash of the node may lose it | the page "Queued Writes", "Waiting for a queue to flush" and "Trade-offs" |
| a backup through any node is the leader's database | the page "Backups": the node "will transparently forward the request to the Leader" |
| every node applies the same log, so every node ends with the same database | the page "Design" |
| a removed node can join again, and a join of a node is idempotent | the page "General guidelines"; unconfirmed by the owner for a node that keeps its data |
| a cluster of three serves with one node down | the page "General guidelines": "It can tolerate the failure of any node, even the Leader" |

## The project

`docker-compose.yaml` runs three nodes, `rq1`, `rq2` and `rq3`, at the fixed addresses
10.89.0.11 to 10.89.0.13, as rqlite's guide on automatic clustering starts them. Each node has a
volume for `/rqlite/file`, its own `-node-id`, and `-bootstrap-expect 3` with the three Raft
addresses in `-join`. `hydo compose` imported the file (`hydo/hydo.yaml`), with these changes:

- each node's `/rqlite/file` is a file system of its own (`storage:`), the target of `L=` and
  `G=`, and `setup:` gives it to uid 1000, the image's user;
- the readiness check asks `/status` with the test client and takes any answer;
- the nodes start together (`start_after: started`), because each waits for the other two before
  it bootstraps;
- `-raft-snap` is the build parameter `snap`, 64 log entries instead of 8,192, so that a run of a
  few minutes takes snapshots;
- the test client `rqc` is in the image.

The first image stopped every node at its start with `failed to create extension store: mkdir
/rqlite/file/data: permission denied`: a new file system's root belongs to root, and the image
runs as uid 1000.

## The workload

`client/` is the Go client `rqc` on `net/http`, which sends each request once (`sh build.sh`
builds it). Each request goes to a node that the seed picks, so most writes and reads take the
forwarding path to the leader. The commands, one file each in `hydo/workload/`:

- `insert`, `upsert`, `update`, `delete`: one write of the table `kv(id, v)` through
  `/db/execute`;
- `tx`: 2 or 3 of those writes of other rows in one request with `transaction`;
- `queue`: an `INSERT` through `/db/execute?queue&wait`, a queued write that waits for the queue
  to commit it;
- `txn` and `append`: a transaction of 1 to 3 list appends and reads of the table `la(k, e)`
  through `/db/request?transaction`, at level `linearizable`, or `strong` in one of four;
- `read` (an anytime command): every node's lists at level `none`, then the leader's at level
  `strong`;
- `backup` (an anytime command): `/db/backup?fmt=sql` through a node;
- `remove` (a serial driver, at most once a run): `DELETE /remove` of a follower, then the end of
  its process, so that it starts again with its `-join` flags;
- `qstop` (a serial driver, only with the build parameter `qstop=yes`): 2 or 3 queued writes
  without `wait` through a node, then SIGTERM to that node;
- `leader` (an anytime command): every node's Raft state from `/status` once a second for 12 s.

A row is a register: its value is the tag of the write that last set it. One command at a time
works on a row, and a row whose write got no certain answer is never written again. Each write
is recorded in `$HYDO_ACKED`, `$HYDO_OUTCOMES` and the history `w`. An answer 200 without an
error acknowledges. A statement's error (the transaction rolled back), 400, and 503 `leader not
found` mean that rqlite applied nothing, so the write failed. Every other answer, an error in
the body of a 200, a timeout, and no answer give an unknown outcome. The list appends and reads
are recorded in the history `la`. After each acknowledged write and each acknowledged
transaction the client writes the events `rq.write_acked` and `rq.txn_acked`, the targets of
`--reactive-at`.

`hydo rules scan --kind writes` found no route, and the clock audit found no place (section "What
hydo lacked"). rqlite's HTTP routes are a `switch` on `r.URL.Path` in `http/service.go`. The
workload covers `/db/execute`, `/db/request`, `/db/query`, `/db/backup`, `/remove`, `/nodes` and
`/status`. `/db/load`, `/boot`, `/snapshot`, `/leader` (a stepdown), CDC and the automatic
backups have no command.

## The rules

| check | promise | file |
|---|---|---|
| `writes` (`type: messages`, `acked_outcome` with `effect: recorded`) | every acknowledged write of `kv` is on the leader at the end, unless a later write of its row replaced it; no refused write took effect | `hydo.yaml`, `rqc readback` |
| `la` (`type: list-append`, strict serializable) | transactions of appends and of reads at level `linearizable` or `strong` are strict serializable; no acknowledged append is lost | `hydo.yaml`, `rqc lafinal` |
| `eventually_converge` | within 120 s of the heal the leader lists every node as a reachable voter, and every node holds the leader's rows and lists | `checks/eventually_converge` |
| `eventually_serves` | within 120 s of the heal a write through each node is acknowledged, and then readable on every node within 15 s | `checks/eventually_serves` |
| `final_available` | while two or more nodes answered, the cluster was never without a leader for more than 30 s | `checks/final_available` |
| a node's lists are prefixes of the leader's | a read at level `none` of any node returns a prefix of each list that the leader holds later | `anytime_read` (exit 100) |
| a backup holds what came before it | a backup through any node holds every append acknowledged before the request | `anytime_backup` (exit 100) |
| a removal takes effect | a follower removed with `/remove` is no voter within 20 s; started again, it joins again (the check `eventually_converge`) | `serial_driver_remove` (exit 100) |

The read-back and `eventually_converge` read the leader at level `strong`, after a barrier write
through it.

## Without faults, and the regressions

The final image `explore-ob-rqlite-disk.img` (`c4d29886…`, the fingerprint epoch 6) gives one
fingerprint, `943d6c299867d55d`, on two runs of `disk=none | run`, which ran 116 commands without
a failure. The image of finding 2, a copy of `hydo.yaml` named `rqlite-qstop` and built with
`--set qstop=yes` (`a3078a27…`), gives `7d26ebb2631121b3` on two runs of the same schedule, each
with finding 2's violation. Each regression gave one fingerprint over two fresh runs:

| file | finding | image | fingerprint |
|---|---|---|---|
| `queued-write-with-wait-acked-and-lost-when-the-leader-dies.schedule` | 1 | default | `b99061a1e2c70785` |
| `queued-write-with-wait-acked-and-lost-behind-a-slow-leader.schedule` | 1 | default | `11279983f35ccb8b` |
| `queued-writes-lost-at-a-graceful-stop.schedule` | 2 | `qstop=yes` | `7d26ebb2631121b3` |
| `node-removed-while-down-never-joins-again.schedule` | 3 | default | `c6b10c11a878e130` |

`hydo repro --regressions` on the default image gave `3 of 3 regressions as expected, 1 left out: they need other build parameters`, and printed the build command of
the left-out one. On the image built with `qstop=yes` it gave `3 of 3 regressions as expected`,
before finding 3's file existed.

The runs that separate the causes, one run each unless two are named:

| schedule | image | verdict | fingerprint |
|---|---|---|---|
| seed 28 of e1, shrunk: `O=rq2@4-6:resp:loss,74% W=ddd2fe8b,p3,q3,r1000 G=rq2@33+2:flip,n=4,seed=32b61fcc` | e1's | `writes`: `q.k80` lost | `e778b8adb3f888be` (two runs) |
| the same without `G=` | e1's | OK | `173ba02d53467c1c` |
| the same without `O=` | e1's | OK | `5fba2024ed03e57b` |
| the composition alone | e1's | OK | `8d4447cccfd347e0` |
| `K=nodes.primary@12+3`, `@18+3` and `@28+3` | an earlier image | OK | |
| `K=nodes.primary@35+3` | final | `writes`: `q.k83` lost | `b99061a1e2c70785` (two runs) |
| seed 46 of e2b, shrunk: `N=nodes.primary@17-20:delay,500ms W=2da25f11,p3,q1,r2000,oanytime_backup` | final | `writes`: `u.k23` (its row's queued `INSERT` lost) | `11279983f35ccb8b` (two runs) |
| the clean schedule | `qstop=yes` | `writes`: 3 queued writes lost | `7d26ebb2631121b3` (two runs) |
| `A=rq.write_acked@20+10:powerloss-rq2` | final | `converge`: `rq2` no voter, without the barrier write | `c6b10c11a878e130` (two runs) |
| seed 65 of e2b: `K=nodes.majority@39+1 N=rq1-rq3@37-50:loss,94% P=rq3@34+1 T=17:-300 W=92b587c9,p2,q2,r2000 L=rq1@43+1` | final | OK | `18769a9cf02accd7` |

In seed 28 the `G=` killed the leader `rq2` before step 33 to damage its file, and a queued write
through `rq1` in that step tried `rq2` twice: `dial tcp 10.89.0.12:4002: connect: connection
refused`. Without the one-way loss the run passed too; why it needs the loss was not traced. The
kills at steps 12, 18 and 28 passed; why each one passed was not traced either.

## What was found

### 1. Known: a queued write with `wait` is answered 200 although it was never committed

- **The label.** A known bug, reported in the open pull request
  [rqlite/rqlite#3027](https://github.com/rqlite/rqlite/pull/3027) (2026-11-08, not merged
  in 10.8.0 or on `master`).
- **The schedule.** `disk=none | run K=nodes.primary@35+3`: a kill of the leader before step 35,
  which starts again before step 38.
- **What breaks.** The messages check `writes`: `q.k83.1.w83 was acknowledged and ended
  succeeded, but its effect is not there`. The client sent `INSERT` of the row `k83` to a
  follower with `/db/execute?queue&wait&timeout=20s` and got 200 after 1.05 s. At the end no node
  has the row. The follower's log has the cause:

  ```text
  execute queue write failed for sequence number 1795651205904106005 on node [::]:4001: dial tcp 10.89.0.12:4002: connect: connection refused
  execute queue write failed for sequence number 1795651205904106005 on node [::]:4001: dial tcp 10.89.0.12:4002: connect: connection refused
  execute queue write retry limit (1) limit reached for sequence number 1795651205904106005 on node [::]:4001
  ```

- **How it was found.** Blind, at seed 28 of the first exploration. The shrinker kept a one-way
  loss and a `G=` of the leader `rq2`, whose kill before step 33 made the same window. On a
  copy of that image the minimal schedule failed twice with one fingerprint
  (`e778b8adb3f888be`). Without the `G=`, and so without the kill of the leader, it passed. The
  saved schedule kills the leader at the step of a queued write. The second exploration met it
  again in seeds 46, 62 and 77, and seed 46 shrank to a delay of 500 ms on the leader's links,
  without a kill (the second regression).
- **The cause.** `runQueue` (`http/service.go` lines 1949 to 2019) sends a batch through the
  proxy and retries it once after a second (`-write-queue-retry`, default 1). When the retries
  run out, it leaves the loop and runs the success path. It stores the batch's sequence number
  as written (line 2013), and `req.Close()` closes the flush channel of every waiting caller
  (line 2014). `queuedExecute` then answers 200 (lines 1514 to 1527). The page "Queued Writes"
  says that a request with `wait` returns when "the queue has committed all pending requests",
  and with an error after its timeout.
- **The consequence.** A client that waits for the queue to learn that its write is durable
  is told so when the batch was dropped. A failure of the leader for more than a second is
  enough.
- **Duplicates.** Pull request #3027 describes the same path and a fix: a waiting caller gets
  `503 queued write failed`. It has no comments yet.
- **No native run.** The cause is plain in the code, and the pull request has a reproduction.
- **The regression.** `regressions/queued-write-with-wait-acked-and-lost-when-the-leader-dies.schedule`.

### 2. New: a graceful stop of a node loses the queued writes that it has acknowledged

- **The label.** A bug of rqlite, new as far as the search found. rqlite documents the queued
  writes' risk at a crash; this is a clean stop.
- **The schedule.** `disk=none | run` on an image built with `--set qstop=yes`. The serial driver
  `qstop` sends 3 queued writes without `wait` (`/db/execute?queue`) through `rq3`, each
  answered 200 with a sequence number, and then sends SIGTERM to `rq3`, as `docker stop`, a
  rolling restart or an upgrade does. The restart policy starts `rq3` again.
- **What breaks.** The messages check `writes`: `s.k101q0.1.w101`, `s.k101q1.1.w101` and
  `s.k101q2.1.w101` were acknowledged, and no node has them at the end. That happens in a run
  without any fault. `rq3`'s log shows a clean stop, with the snapshot that rqlite takes on close:

  ```text
  [rqlited] 2026/11/26 00:00:56 received signal "terminated"
  [http] 2026/11/26 00:00:56 closing HTTP service on [::]:4001
  [store] 2026/11/26 00:00:56 snapshot-on-close took  6.713ms
  [store] 2026/11/26 00:00:56 store closed with node ID rq3, listening on 10.89.0.13:4002
  [rqlited] 2026/11/26 00:00:56 rqlite server stopped
  ```

  It has no line `execute queue write failed`: the batch was never sent.
- **The cause.** At SIGTERM `rqlited` closes the HTTP service first (`cmd/rqlited/main.go`
  line 284). `Service.Close` (`http/service.go` line 421) closes the queue, and `Queue.Close`
  (`queue/queue.go` line 166) stops the queue's goroutine. The goroutine returns at `case
  <-q.done` (line 236) with the statements it holds, without a last write, until the batch timer
  (`-write-queue-timeout`, 50 ms) or the batch size (128) sends them. `Service.Close` then closes
  `closeCh`, so `runQueue` also ends without the batch that it may have received. Nothing flushes
  the queue before the store closes.
- **The consequence.** A planned restart of a node can lose the queued writes of its last 50 ms,
  whose sequence numbers the client already has. The page "Queued Writes" names
  only a crash: "there is a small risk of data loss in the event that the node crashes before
  queued data is persisted". A client that queues writes cannot tell a clean stop from a crash.
- **Duplicates.** No issue or pull request names a queue that a shutdown drops. Pull request
  #2978 (open) changes `Service.Close` so that it cannot hang behind a blocked queue; it does
  not flush the queue either. The section "The duplicate searches" has the phrases.
- **No native run.** The cause is plain in the code: nothing between the signal and the store's
  close writes the queue's batch.
- **The regression.** `regressions/queued-writes-lost-at-a-graceful-stop.schedule`, with the
  line `# params: qstop=yes`: `hydo repro --regressions` replays it on an image built with that
  value only, and names the build command on the default image.

### 3. New: a node removed while it was down never joins again after its restart

- **The label.** A bug of rqlite, new as far as the search found. It is a case that the fix of
  [#1290](https://github.com/rqlite/rqlite/issues/1290) (a removed node with `-bootstrap-expect`
  that did not join again, fixed in 7.20.1) does not cover.
- **The schedule.** `disk=none | run A=rq.write_acked@20+10:powerloss-rq2`: 10 ms after the first
  acknowledged write of step 20, the follower `rq2` loses its power and stays down until the
  heal. While it is down, the removal driver removes `rq2` through `rq3` (`DELETE /remove`, 200),
  as the clustering guide says to do for a failed node. At the heal hydo starts `rq2` again with
  its own flags, `-bootstrap-expect 3` and the three Raft addresses in `-join`.
- **What breaks.** `eventually_converge` for the whole 120 s: `the leader rq3 does not list rq2
  as a reachable voter`, and `rq2 has not applied the barrier write`. `eventually_serves`: `a
  write through rq2: HTTP 503 leader not found`. `rq2`'s log:

  ```text
  [rqlited] 2026/11/26 00:01:03 preexisting node state detected in /rqlite/file/data
  [store] 2026/11/26 00:01:03 node rq3 at 10.89.0.13:4002 is now Leader
  [cluster-bootstrap] 2026/11/26 00:01:03 boot operation marked done
  [raft] 2026/11/26 00:01:13 [WARN]  not part of stable configuration, aborting election
  [store] 2026/11/26 00:01:13 Leader is now unknown
  [raft] 2026/11/26 00:01:50 [ERROR] failed to take snapshot: error="cannot take snapshot now, wait until the configuration entry at 64 has been applied (have applied 63)"
  ```

  The last line repeats every 6 to 9 s to the end. `rq2` sends no join request. The leader's log
  has `node rq2 removed successfully` at 00:00:44, and then `failed to appendEntries to:
  peer="{Voter rq2 10.89.0.12:4002}"` until 00:01:01.
- **How it was found.** The aimed grid e3, `--reactive-at rq.write_acked@20 --react-delays
  0..40/10 --react-actions cut,powerloss,kill-now`, at cell 10 of 35. It is the grid's only
  failure, and the shrinker kept the whole schedule, the reaction alone.
- **The cause.** hashicorp/raft's leader keeps replicating to a removed server until that
  server holds the configuration entry that removes it. So the leader of this run sends
  AppendEntries to `rq2` again as soon as `rq2` listens. At its start `rqlited` runs the
  bootstrapper with `-bootstrap-expect` and `-join` (`cmd/rqlited/main.go` lines 673 to 677), and
  its done check is `leader != ""` (lines 643 to 646). The bootstrapper checks it before every
  join (`cluster/bootstrap.go` lines 151 to 156), and the leader that `rq2` has just heard from
  passes it. The boot ends as done, without a join. Then `rq2` takes the entry that removes it,
  stops all elections, and has no leader. Nothing tries the join again.
- **The consequence.** A node that an operator removed while it was down, and that comes back
  with its data and its start flags, stays out of the cluster for good. It answers reads at
  level `none` with old data, refuses writes with `leader not found`, and logs a failed snapshot
  every few seconds. The guide's text on joins says that they are "basically idempotent". The
  same restart after the removal had taken effect, as the removal driver does in a run without
  faults, joined again within seconds.
- **Duplicates.** #1290 (closed 2023-06-02) is the nearest: with `-bootstrap-expect`, a node that
  the cluster had removed did not join again, because it saw its old configuration; the fix
  makes the bootstrapper try a join first. No issue names a join that the done check skips. The
  section "The duplicate searches" has the phrases.
- **No native run.** The cause is plain in the code and in the two nodes' logs.
- **The regression.** `regressions/node-removed-while-down-never-joins-again.schedule`.

### Not bugs

- **Detected damage.** In 5 schedules a `G=` damaged a file of one node, the SQLite database or
  the Raft log `raft.db`, and that node stopped at its next start (`ERROR rq1 restart failed (its
  files were corrupted)`). The other two nodes went on serving and kept every acknowledged write.
- **Undetected damage of the Raft log.** Seed 51 of e2b flipped 32 bits of `rq1`'s `raft.db`
  while `rq1` ran, and later cut `rq1`'s power. After the heal `rq1` still held the row `k23`,
  which a transaction had deleted and the leader had acknowledged: `rq1 holds k23 = "w23", and
  the leader rq2 ""`, for the whole 120 s of `eventually_converge`. `rq1` logged no error. The
  shrinker kept `T=1:-30 W=6f607a33,p4,q3,r500,oanytime_read G=rq1@18:flip,n=32,seed=1cf26ef3
  L=rq1@31+1`, which failed twice in the same way. hydo damaged the file, so the label is the
  environment. rqlite keeps a checksum only of its snapshot files (the package
  `snapshot/sidecar`), not of the Raft log. A damaged entry whose command still decodes is
  applied on that node alone, and a statement that the damage made fail is a result, not a fatal
  error (`store/command_processor.go` lines 62 to 71). The page "Data Integrity" says that
  "rqlite computes a CRC checksum over every on-disk data file" at a start. That reads wider than
  the code.
- **Mistakes of the rules.** The first client's transactions sent a `DELETE` for every write of a
  transaction, so a clean run reported 5 acknowledged writes without their effect. The first
  removal driver waited 90 s for the rejoin inside a serial driver (seed 57 of e2; section "What
  hydo lacked", item 2). The second one did not restart a follower whose removal got the answer
  `500 leadership lost while committing log`, an unknown outcome. The removal committed later,
  the follower ran on outside the cluster, and `eventually_converge` failed (seeds 65 and 75 of
  e2b). The driver now restarts the follower after such an answer too, and seed 65's schedule
  passes on the final image.
- **An update of a row that a dropped queued write never created.** In seed 46 of e2b the queued
  `INSERT` of `k23` was answered 200 after 3 s under a 500 ms delay of the leader's links and
  never committed (finding 1). The later `UPDATE` of `k23` was acknowledged with 0 rows changed,
  and the messages check named the update. The shrinker reduced it to
  `N=nodes.primary@17-20:delay,500ms W=2da25f11,p3,q1,r2000,oanytime_backup`: finding 1 without
  any kill.

## The duplicate searches

`gh search issues --include-prs` and `gh issue list --search`, open and closed, on 2026-11-26.
The CHANGELOG from v10.0.0 to v10.8.0 was read too, and `master` differs from `v10.8.0` only in
`CHANGELOG.md`.

| repository | phrase | what it found |
|---|---|---|
| rqlite/rqlite | `queue wait`, `write-queue-retry`, `retry limit`, `wait queue 200` | #3027 (open: queued writes that hit the retry limit are answered as written), #3021 and #1887 (the `wait` flag's input and its timeout), #2827 (the retries made configurable), #3000 (closed: forwarded writes applied twice, fixed in v10.7.0) |
| rqlite/rqlite | `queue in:title`, issues and pull requests | 43 results; #2978 (open: `Service.Close` can hang behind a blocked queue), #2185 (open: "Queue flush reset", no description); none about a stop that drops the queue |
| rqlite/rqlite | `queued writes lost`, `queue lost`, `queued writes dropped`, `queue shutdown`, `queued writes shutdown`, `graceful shutdown queue`, `SIGTERM queue`, `queue close` | #1886 (timeouts in a cluster), #1627 (WAL snapshots), #1262 (queries on Windows); none about a stop |
| rqlite/rqlite | `shutdown`, `SIGTERM`, `graceful` | #1132 (closed: graceful restarts without client impact, which added `-raft-shutdown-stepdown`), #1695, #2620, #1746; none about queued writes |
| rqlite/rqlite | `sequence_number`, `sequence number restart` | #3000, #1648, #1048; none about the queue's numbers after a restart |
| rqlite/rqlite | `rejoin`, `removed node`, `join removed`, `node removed cannot join`, issues and pull requests | #1290 (closed: with `-bootstrap-expect`, a removed node did not join again because of its old configuration; fixed in 7.20.1), #1293, #818, #2778 (a rejoin with a changed node ID), #3072, #3073 (open: linearizable reads time out after a membership change); none about a join that the boot's done check skips |
| rqlite/rqlite | `not part of stable configuration`, `boot operation marked done`, `bootstrap-expect restart` | #1290, #1293, #1627, #2687; none about a removed node that hears the leader at its start |


## The explorations

| name | image and options | seeds | schedules | failures |
|---|---|---|---|---|
| e1 | the first image (the list-append check dropped, a removal driver and the stop driver not yet there); `--strategy process --new-faults --cluster-faults --storage-faults`, 1 worker | 1-40 | 40 | 3: finding 1 (seed 28, shrunk to a one-way loss and the kill of the leader that a `G=` makes), detected damage (2) |
| e2 | the image with the removal driver that waited; `--strategy process --more-faults --cluster-faults --storage-faults` | 41-110 | 19, then stopped | 1: seed 57, the waiting removal driver (a mistake of the rules) |
| e2b | the image with every check; the options of e2 | 41-100 | 60 | 9: finding 1 (3: seeds 46, 62, 77; seed 46 shrank to a delay of the leader's links), detected damage (3), the damaged Raft log (seed 51), a removal of unknown outcome that the driver did not follow with a restart (2, a mistake of the rules) |
| e3 | the final image; `--reactive-at rq.write_acked@20 --react-delays 0..40/10 --react-actions cut,powerloss,kill-now` | the grid once | 35: the delays 0, 10, 20, 30 and 40 ms, each with a power cut of the guest, a power loss of each node and a `kill-now` of each node | 1: finding 3 (cell 10, `powerloss-rq2` at 10 ms) |

e1 ran 40 schedules, the reruns and the shrink of one failure in 581 s, at 250 runs an hour. e2b
ran 60 schedules in 1,827 s, at 122 runs an hour, beside two other onboardings and a release
pass on the same machine. e3 ran 35 schedules in 413 s. The list-append check judged every run
of e2b and e3, 95 schedules, and it held in all of them. e2b's image is the final image without the last change of the removal driver: since
then a removal of unknown outcome is followed by a restart of the node.

## What hydo lacked

In the order of the time each cost:

1. **A list-append check in a file is dropped when hydo.yaml has `checks:`.** The first images
   had `checks/list_append_la` with `# hydo: history=la consistency=strict-serializable`, beside
   the messages check `writes` in `hydo.yaml`. `hydo build` said `+ 2 history checks`, and the
   image's `etc/hydo/manifest.sh` has both (`HYDO_CHECK_c0` and `HYDO_CHECK_c1`). The runs
   declared and judged only `writes`: no `HYDO-CHECK` line, no read-back, and no verdict for `la`.
   The host controller takes the history checks from the embedded `hydo.yaml`, and reads
   `manifest.sh` only when that list is empty (`hydo-control/src/agent.rs`, lines 245 to 262).
   With a messages check there it replaces the messages check's JSON and adds nothing. The run
   gives no warning. The example declares `la` in `hydo.yaml` instead.
2. **A pause by steps lasts as long as a serial driver holds the round.** The first removal
   driver waited up to 90 s for the removed node to join again. The composer starts no round
   while a serial driver runs (`hydo guide composer`, "Command kinds"), so the step after it came
   90 s later. In seed 57 of the second exploration, `P=rq3@10+1` began while `rq3` restarted
   and lasted those 90 s, so `rq3` could not join, and the driver reported a violation. The page
   of faults says that `+<n>` counts steps, and does not say that a step can last a serial
   driver's whole run. The driver now returns after the restart, and `eventually_converge`
   checks the membership.
3. **No fault stops a service gracefully.** `K=` kills with SIGKILL. Finding 2 needs SIGTERM and
   a restart. The client sends the signal itself to the process whose command line names the
   node, in `/proc`, so the explorer cannot draw it or move it, and the shrinker cannot drop it.
4. **No fault changes a cluster's members.** Finding 3 needs a removal while a node is down.
   hydo has no token that removes a node or adds one, so the client's serial driver calls
   `/remove` itself, and only the grid's power loss put the removal inside a node's down time.
   The explorer cannot draw a removal at a chosen step, and the shrinker cannot drop or move it
   (the idea "Repeated fault windows and membership changes" of `docs/USE-CASES.md`).
5. **Two images in one folder.** The image of finding 2 is a copy of `hydo.yaml` built with
   `--set qstop=yes` (`explore-qstop.yaml`, name `rqlite-qstop`). After that, `hydo run` without
   `--image` stopped with `no image given, and there are 2 images`, and the scripts had to name
   the image (as in the Typesense example).
6. **A file system of `storage:` belongs to root.** The image runs as uid 1000, and every node
   stopped at its start with `mkdir /rqlite/file/data: permission denied`. `hydo compose` gives a
   volume the image's owner (`copy_up`), but a service's own file system starts empty and owned
   by root, and `hydo build --check` does not say so. Each node's `setup:` now runs `chown
   1000:1000` on it.
7. **The scan of write routes found none.** `hydo rules scan --kind writes` read 163 files and
   listed 0 routes. rqlite's routes are a `switch` on `r.URL.Path` with `strings.HasPrefix` in
   `ServeHTTP` (`http/service.go` lines 479 to 540), and the handlers check `r.Method`
   themselves. The routes were listed by hand.
8. **The clock audit found no place.** It read 160 files and found no duration on the wall
   clock, which matches the code: rqlite's timeouts use Go's monotonic clock. It also listed no
   reading kept as an order: the staged WAL files of an incremental snapshot are named by
   `time.Now().UnixNano()` and applied in the order of their names (`snapshot/staging.go` line
   147), and the queue's sequence numbers start at `time.Now().UnixNano()` (`queue/queue.go`
   line 113). No run here made two staged WAL files, so whether a step back reorders them was
   not tested.
9. **The release tree needs its store unpacked.** `hydo build` of an image with `network:
   bridge` stopped with `HYDO_IPROUTE2 not found at /nix/store/...` until `guest/store.tar` was
   unpacked, and the message does not name `hydo setup` (open item 98).
10. **A Compose file's fixed address reads as ignored.** `hydo compose` printed `service rq1:
   ipv4_address 10.89.0.11 ignored; the service is at 10.89.0.11` for each node, as in the
   Typesense example.

## Time

| phase | wall time |
|---|---|
| scouting (the docs, the code of the queue and the reads, the issues, the target's fit) | 20 minutes |
| setup to the first clean run (the client, the import, the owner of the file systems, the release's store) | 30 minutes, with the client written while the first builds ran |
| exploration, shrinking, and judging (three images rebuilt for the rules' mistakes and for the drivers) | 2 hours 30 minutes |
| duplicate search, write-up, and the tests | 1 hour 30 minutes, with the tests on a loaded machine |
