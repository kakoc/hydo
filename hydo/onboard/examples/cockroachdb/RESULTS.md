# CockroachDB under hydo: a cluster of three nodes

CockroachDB (github.com/cockroachdb/cockroach) is a distributed SQL database. It splits the data
into ranges, replicates each range with Raft, and orders transactions by hybrid logical clock
(HLC) timestamps that start from each node's wall clock. This example runs a cluster of three
nodes from the official image in one guest of hvx on the bare-metal machine, and looks for bugs
that nobody reported.

## The short answers

- **What was found?** One new candidate, one known clock effect, and one candidate whose cause
  was not traced. Nothing was posted.
  1. **New, a clock effect past the documented bound: one node's clock that is off stops the two
     healthy nodes too.** With roach3's own clock 1 s ahead or behind (`clock: own`,
     `T=roach3@5:1`), all three nodes stop themselves again and again with `clock synchronization
     error ... (1 of 2 are within the offset)`, and the cluster acknowledged 5 of 23 writes.
     A healthy node of three counts two clocks, and the rule stops a node when at most half of
     them are within the offset (`skew/regressions/a-node-clock-*-stops-the-healthy-nodes.schedule`).
  2. **Known, a clock effect by design: a step back of the clock and a restart of every node.**
     After `T=10:-300` and a kill of all three nodes, no node serves for longer than the check's
     180 s, because each new process starts its clock 5 minutes before the cluster's data
     (`hydo/regressions/no-node-serves-after-a-step-back-and-a-restart-of-all.schedule`).
     [#25109](https://github.com/cockroachdb/cockroach/issues/25109) and the setting
     `server.clock.persist_upper_bound_interval` cover it.
  3. **Not traced: the first node fills its store with WAL files that it never flushes.** In one
     blind schedule roach1 kept nine WAL files of about 70 MiB each and wrote no SST, filled its
     file system of 640 MiB, and did not start again after the power cut
     (`skew/regressions/the-first-node-fills-its-store-with-unflushed-wal-files.schedule`).
- **What held?** Every write that CockroachDB acknowledged stayed as acknowledged, and no refused
  write took effect. Every history of list appends and reads was serializable, every read at one
  past timestamp gave every node the same bank and lists with the total of 500, every follower read
  kept the total, and CockroachDB's own replica check found no difference. That held under the
  clock runs above, with roach3's clock 300 ms ahead or behind, under steps of the whole guest's
  clock with and without kills of every node, and in the schedules of the blind explorations whose
  checks ran, which also drew partitions, one-way losses, pauses, power cuts, one node's power loss and
  damaged files.
- **What did hydo lack?** The section "What hydo lacked": a diagnosis replay killed by the host's
  out-of-memory killer, the nodes' own log files outside the run directory, and a node that stops
  itself and starts again invisible to the verdict.

## The version

- CockroachDB v26.4.1, the newest release (2026-11-18). The official image
  `cockroachdb/cockroach:v26.4.1`: index
  `sha256:13331054ac7b815bcf7d4396e3416d3ef53522be308b5c4a8155079f35fe58a1`, linux/amd64
  `sha256:de1c2ef57bc2b5b7e553ab68e0cd4217a948ab26eae471b63ffea00127f8e0f6`.
- The public repository has no tag and no branch of v26.3 or v26.4. Its `master` ends at
  `d30c905f` (2026-09-23), a commit that forwards the project's automation "to the code
  repository's workflow". So the code of v26.4.1 is not public. The causes below cite `master`
  at `d30c905f`, the newest public code; the issue tracker is still the public repository's.
- hydo: `main` at `bdb403f4` (0.6.0 and Milestones 362 to 374), with the explorer's
  `--service-clocks` of Milestone 375's branch (`6f65876a`), built on the bare-metal machine,
  with the 0.6.0 release's guest files. The engine is hvx, with one VM of 3,072 MiB at a time.

## The promises

The promises come from CockroachDB's documentation. No owner of CockroachDB confirmed them.

| promise | source |
|---|---|
| a committed transaction is durable, on a majority of the range's replicas | "Architecture: Replication Layer" and "Transaction Layer" |
| every transaction runs at SERIALIZABLE isolation by default | "Transactions": "CockroachDB executes all transactions at the strongest ANSI transaction isolation level: SERIALIZABLE" |
| a read `AS OF SYSTEM TIME` reads one consistent snapshot at that timestamp, on any node | "AS OF SYSTEM TIME" |
| a follower read returns the data at `follower_read_timestamp()` from the nearest replica, consistent at that time | "Follower reads" |
| the replicas of a range hold the same data, and the consistency checker finds any difference | "Architecture: Replication Layer"; `crdb_internal.check_consistency` |
| a node whose clock is more than 80 % of `--max-offset` (500 ms) away from at least half of the other nodes stops itself | "Clock synchronization" in "Operational FAQs" and the flag `--max-offset` |
| a cluster of three survives the loss of any one node | "Fault tolerance" |

## The project

`docker-compose.yaml` runs three nodes, `roach1`, `roach2` and `roach3`, at the fixed addresses
10.89.0.11 to 10.89.0.13, as CockroachDB's guide "Start a cluster in Docker" does, insecure, each
with a volume for `/cockroach/cockroach-data`, and a one-shot service `init` that runs `cockroach
init`. The cache is 64 MiB and the SQL memory 128 MiB per node. `hydo compose` imported the file
(`hydo/hydo.yaml`), with these changes:

- each node's `/cockroach/cockroach-data` is a file system of its own of 640 MiB (`storage:`), the
  target of `L=` and `G=`;
- `--store=path=/cockroach/cockroach-data,ballast-size=0` (also in the Compose file): the first
  clean run filled roach1's file system, the node stopped with exit status 10, CockroachDB's code
  for a full disk, and the emergency ballast file was turned off for it. The comment in
  `hydo.yaml` names the ballast as the cause. It was not: its default is 1 % of the disk, and the
  store held the nine unflushed WAL files of candidate 3. The comment stays, because it is in the
  image's bytes and its recorded fingerprints;
- `guest.cpu: v3`: the image's glibc stops at once with `Fatal glibc error: CPU does not support
  x86-64-v3` on hvx's default x86-64-v2 CPU;
- the one-shot service `init` is gone; the workload's `first_init` runs `cockroach init` once with
  roach1's image (`hydo-oci roach1 /cockroach/cockroach init ...`);
- the readiness check is a connection to the RPC port 26357, which a node opens before the
  cluster is initialized, and the three nodes start together (`start_after: started`);
- the group `nodes` restarts its killed members together and puts a link between every pair
  (`restart: together`, `links: all`, Milestone 374);
- the test client `crc` is in the image.

`skew/hydo.yaml` is the same manifest with `clock: own` on roach3 (Milestone 372), so that
`T=roach3@<at>:<change>` moves roach3's wall clock alone.

## The workload

`client/` is the Go client `crc` on pgx v5.12.0, which sends each statement once (`sh build.sh`
builds it). Each command connects to a node that the seed picks; CockroachDB sends the statement
on to the range's leaseholder. The commands, one file each in `hydo/workload/`:

- `insert`, `upsert`, `update`, `delete`: one write of the table `kv(id, v)`, an implicit
  transaction;
- `tx`: 2 or 3 of those writes of other rows in one explicit transaction;
- `txn`: a transaction of 1 to 3 list appends and reads of the table `la(k, v)`, at
  CockroachDB's default isolation, SERIALIZABLE;
- `transfer`: a transfer of 1 to 5 between two of the five accounts of `acct`, read, and written
  in one transaction;
- `read` (an anytime command): the HLC timestamp from one node minus 4 s, then a read of the bank
  and of every list at that timestamp through each node in one read-only transaction, and the
  current lists;
- `follower` (an anytime command): the bank at `follower_read_timestamp()` through a node.

A row of `kv` is a register: its value is the tag of the write that last set it. One command at a
time works on a row, and a row whose write got no certain answer is never written again. Each
write is recorded in `$HYDO_ACKED`, `$HYDO_OUTCOMES` and the history `w`. A commit without an
error acknowledges. An error before the commit, a retry error (40001), a unique violation, and the
other errors of a statement's own classes mean that nothing was applied, so the write failed.
"Result is ambiguous" (40003), every other error of the commit, a timeout, and a lost connection
give an unknown outcome. The list appends and reads are recorded in the history `la`.

## The rules

| rule | promise | file |
|---|---|---|
| `writes.acked-outcome` | every acknowledged write of `kv` ends as acknowledged; no refused write took effect | `hydo.yaml` (`type: messages`, `crc readback`) |
| `la` | the transactions of list appends and reads are serializable, and no acknowledged append is lost | `hydo.yaml` (`type: list-append`, `consistency: serializable`) |
| `compose.anytime_read.no_violation` | every node reads the same bank and lists at one past timestamp, the bank sums to 500, and each list is a prefix of the current one | `workload/anytime_read` |
| `compose.anytime_follower.no_violation` | a follower read of the bank sums to 500 | `workload/anytime_follower` |
| `bank` | every node that answers reads the bank as 500 at the end | `checks/final_bank` |
| `replicas` | CockroachDB's own consistency check, `crdb_internal.check_consistency`, finds no range whose replicas differ | `checks/final_replicas` |
| `serves` | within 180 s of the heal, a write and a read through at least two nodes succeed | `checks/eventually_serves` |
| `clockstops` | at most one of the three nodes stopped itself for clock synchronization: one node's clock that is off stops that node alone | `checks/final_clockstops` |

The check `clockstops` reads the nodes' own log files (`/data/vol/roach<n>/logs/cockroach.*.log`)
for the fatal line `clock synchronization error`. It was written after the runs of finding 1,
because hydo starts a stopped node again (`restart: unless-stopped`), so every rule over the data
held in those runs.

## Without faults, and the regressions

The final default image `explore-ob-cockroachdb-disk.img` is `5bb6c565ad9ec130…`, built at the
fingerprint epoch 6; the final skew image is `f49d407904c55fb4…`. `disk=none | run` passed twice
on the default image with `2ed1cc731ea7b53f`: 66 writes acknowledged and read back, and every
check held. On the skew image the clean run passed too (`68e14bc8d949e4de` on an earlier build).

| regression | image | verdict | fingerprint, twice |
|---|---|---|---|
| `skew/regressions/a-node-clock-ahead-stops-the-healthy-nodes.schedule` | skew | VIOLATION `[clockstops]` | `57bca9568e26a82d` |
| `hydo/regressions/no-node-serves-after-a-step-back-and-a-restart-of-all.schedule` | default | VIOLATION `[serves]` | `052cb64318f15b3f` |

| `skew/regressions/the-first-node-fills-its-store-with-unflushed-wal-files.schedule` | skew | VIOLATION `service cannot recover` | `09ec5ed750333c34` |

`hydo repro --regressions` on both final images gave every saved schedule its verdict and the
fingerprint above: `1 of 1 regressions as expected` on the default image and, on the skew image,
the two schedules above. That replay also held the schedule with roach3's clock behind, which an
earlier copy of the folder had saved; it gave OK (`5e0dcb9487f09a44`), as in its two runs, and is
no longer saved. hydo warned that the third schedule's `W=` is in the seed form without its
command list, because it was copied from the exploration by hand rather than with `hydo save`.

## Finding 1: one node's clock out of bounds stops the two healthy nodes too

**A bug of the project, new as far as the search found; a clock effect, past the documented
bound.**

With roach3's clock 1 s ahead or 1 s behind the other two nodes, roach3 stops itself, as
CockroachDB documents. But roach1 and roach2, whose clocks agree, stop themselves too, again and
again, each with the same fatal line:

```text
F261126 00:00:39.749523 2749 1@rpc/peer.go:535 ⋮ [T1,Vsystem,n1,rnode=1,raddr=‹10.89.0.11:26357›,class=system,rpc] 261  clock synchronization error: this node is more than 400ms away from at least half of the known nodes (1 of 2 are within the offset)
```

So a three-node cluster with one bad clock loses every node, not one. While hydo started the
stopped nodes again, the cluster acknowledged 5 of the 23 writes the workload sent with roach3
ahead (`T=roach3@5:1`), and 22 of 65 with it behind (`T=roach3@5:-1`); a clean run sends 66 and
every one is acknowledged. A
node that the operator's supervisor starts again runs into the same check while roach3 runs.

The runs on the skew image before the final one (`bebabf7f…`, the same manifest without
`clockstops` and the `logs:` lines). A node's exits are the lines `<node> exited (status 7)` of
`one.log`; exit status 7 is CockroachDB's code for a fatal error.

| schedule | verdict | exits of roach1, roach2, roach3 | writes acknowledged of those sent |
|---|---|---|---|
| `disk=none \| run` | OK `68e14bc8d949e4de` (an earlier image) | 0, 0, 0 | 66 of 66 |
| `disk=none \| run T=roach3@5:1` | OK `ddc9580760a8dbfd` | 4, 3, 10 | 5 of 23 |
| `disk=none \| run T=roach3@5:-1` | OK `4e8cc5ee0ed874b4` | 7, 10, 8 | 22 of 65 |
| `disk=none \| run T=roach3@0:50000ppm` | OK `9713525754313c6a` | 1, 3, 3 | 6 of 28 |
| `disk=none \| run T=roach3@5:300ms` | OK `5346e311a6fd4e71` | 0, 0, 0 | 66 of 66 |
| `disk=none \| run T=roach3@5:-300ms` | OK `6c66b8e79c6f4073` | 0, 0, 0 | 66 of 66 |

On the final skew image `T=roach3@5:1` gave 1, 1 and 7 exits and 3 of 22 writes acknowledged,
twice.

**The cause.** Each node keeps the offset of every node it heartbeats with
(`RemoteClockMonitor`, `pkg/rpc/clock_offset.go`). After every heartbeat,
`VerifyClockOffset` counts the offsets within the tolerated offset (80 % of `--max-offset`,
400 ms) and returns an error when `healthyOffsetCount <= numClocks/2` (line 467 on `master` at
`d30c905f`). The heartbeat then ends the process with `log.Ops.Fatalf` when
`FatalOnOffsetViolation` is set (`pkg/rpc/peer.go` line 523). A healthy node of three counts two
clocks: the other healthy node's and the bad one's. One of two is not more than half, so the
healthy node stops. The documentation says the rule the same way ("at least half of the other
nodes"), but the rule exists to stop the node whose clock is off, and in a cluster of three it
stops all of them. The investigation of the roachtest failure
[#173248](https://github.com/cockroachdb/cockroach/issues/173248) shows a count of three
(`1 of 3 are within the offset`) and says that the two healthy nodes survived there. It calls that
"exactly the intended behavior."

**Within the bound nothing broke.** With roach3's clock 300 ms ahead or behind, no node stopped,
and every rule held (the runs `t-ahead300` and `t-behind300` below).

**The duplicate search.** The message `(1 of 2 are within the offset)` is in the logs of four
roachtest failures on three-node clusters: #145889, #156834, #137898 and #173765. Each was closed
as a clock problem of the test machines. No issue names the count of a healthy node.

A drift does the same: with roach3's clock 5 % fast from the start (`T=roach3@0:50000ppm`), its
offset passed 400 ms after about 8 s. roach1 stopped once, roach2 and roach3 3 times each, and
the cluster acknowledged 6 of 28 writes.

**No native run.** The rule is plain in the code, and the log lines name the count.

**The regression.** `skew/regressions/a-node-clock-ahead-stops-the-healthy-nodes.schedule`
breaks `clockstops`: `VIOLATION [clockstops] 3 of 3 nodes stopped themselves for clock
synchronization (roach1, roach2, roach3)`, twice with `57bca9568e26a82d` on the final skew image
(`f49d4079…`). The schedule with the clock behind is no regression: on the final image no node
stopped in it, roach3 included, and the cluster acknowledged 46 of 62 writes; why the offset
check did not fire there was not traced.

## Finding 2: no node serves after a step back of the clock and a restart of every node

**A clock effect, known ([#25109](https://github.com/cockroachdb/cockroach/issues/25109)); by
design.**

The guest's wall clock steps back 300 s before step 10, and all three nodes are killed before step
15 and start again together before step 18 (`restart: together`). No node serves a write or a
read within 180 s of the heal: `VIOLATION [serves]`, and every SQL connection fails with `failed
to receive message`. The restarted nodes' logs say `first range unavailable` and `resolved system
to 1 but found no descriptor with id 1`: a node's new process starts its hybrid logical clock
from its wall clock, here 5 minutes before the timestamps of everything the cluster wrote (and
before its own `cockroach init`). The outage follows the step's size: the same kills after a
step of 120 s passed within the deadline of 180 s, and so did a step of 20 s before a
kill of every node or of roach1 alone, and a step of 20 s without a kill.

On the final default image (`5bb6c565…`):

| schedule | verdict | fingerprint |
|---|---|---|
| `disk=none \| run T=10:-300 K=nodes.all@15+3` | VIOLATION `[serves]`, twice | `052cb64318f15b3f` |
| `disk=none \| run T=10:-120 K=nodes.all@15+3` | OK | `8d5d093333a78c8a` |
| `disk=none \| run T=10:-60 K=nodes.all@15+3` | OK | `cefa1a86b54f13b4` |
| `disk=none \| run T=30:-20 K=nodes.all@32+3` | OK, twice | `c87f47c9852d52be` |
| `disk=none \| run T=30:-20 K=roach1@32+3` | OK | `8050fd03d8a5bb1d` |
| `disk=none \| run T=30:-20` | OK | `76878daa990c1b11` |

CockroachDB says so: the cluster setting `server.clock.persist_upper_bound_interval` exists
because "after a restart HLC loses its state" (#25109, open since 2018), and its description says
that with it "when cockroach starts, it waits for the wall time to catch-up till this persisted
timestamp" (`pkg/server/clock_monotonicity.go`). It is off by default. With it set to 100 ms
(`hydo build --set persist=100ms`) the same schedule fails the same way (`79bcab6e99881a9d`), and
the step of 20 s passes (`7c64f641b76d9c2c`). So the outage is the
documented price of a step back across a restart of every node, with the setting and without it.
No acknowledged write, transaction, transfer or past-timestamp read was lost in any of these
runs.

**The regression.** `hydo/regressions/no-node-serves-after-a-step-back-and-a-restart-of-all.schedule`
fails `[serves]`, twice with `052cb64318f15b3f`.

## Candidate 3: the first node fills its store with WAL files that it never flushes

**A candidate whose cause was not traced.**

Seed 1 of the exploration e2 on the skew image failed with `VIOLATION service cannot recover
(committed data unrecoverable) (roach1)`: in the recovery boot after the power cut `C=40`, roach1
stopped at every start with exit status 6, CockroachDB's code for a log file it cannot write.
Its own file system was full: 601 of 614 MiB; roach2 and roach3 used 147 MiB. The store
held nine Pebble WAL files from the first two minutes (`000003.log` to `000011.log`, 155 KB to
29 MB each), each with 72,092 KiB allocated, and no SST: `MANIFEST-000001` was still 33 bytes, so
roach1 had never finished a flush. The first flush, `000012.sst`, was empty. roach2's store at
the same moment had two WAL files and four SSTs. The first clean run of the first image showed
the same nine files on roach1 and stopped it with exit status 10 (a full disk), in a run without
faults.

So two things hold: Pebble preallocates each WAL file to about 70 MiB, and roach1's memtables
were not flushed for two minutes while their WAL files stayed. The first is by design; with a
real disk it costs memory and replay time, not space. Why roach1 did not flush was not traced;
the schedule's other faults (a kill of roach2, a pause of roach1, a forward step of 30 s, losses
on roach2's links, and its composition) were not separated. The duplicate search found no issue
that names a store that never flushes.

**The regression.** `skew/regressions/the-first-node-fills-its-store-with-unflushed-wal-files.schedule`,
twice with `09ec5ed750333c34` (the exploration's run and `hydo run`).

## Not bugs

**A cluster of 40 s with ranges of two replicas.** The first image started the workload as soon
as every node served SQL. With roach3's clock 1 s off from step 5, the cluster then served
nothing for good: after the heal no node answered a write within 180 s (`[serves]`). The logs of
roach1 and roach2 named ranges whose descriptor held two replicas, `[(n1,s1):1, (n2,s2):2,
next=3]`, one of them on roach3; 29 ranges were unavailable and 62 under-replicated 40 s after
`cockroach init`. CockroachDB adds the third replica of each range in the background, and a
range of two replicas has no quorum without one of them. That is the replication factor's
promise, not a bug, so `crc init` now waits until every range has three replicas before the
workload starts.

## The duplicate searches

`gh search issues --repo cockroachdb/cockroach --include-prs "<phrase>"` on 2026-11-27, and
`gh issue view` of the hits:

| phrase | what it found |
|---|---|
| `clock synchronization error at least half of the known nodes` | nothing |
| `1 of 2 are within the offset` | roachtest failures #145889, #156834, #137898 and #173765 with the message on three-node clusters, each closed as the test machines' clock |
| `are within the offset` | #173248 (`1 of 3`, healthy nodes survived), #102316, #147479 |
| `healthyOffsetCount` | #173248 and #16943 (a clearer message, 2017) |
| `VerifyClockOffset` | #159601 and #147825 (its cost), #85983 (several max offsets), #103582 |
| `clock offset kills healthy nodes`, `clock skew one node kills cluster`, `clock synchronization error healthy nodes`, `self offset RemoteClockMonitor`, `offset own node VerifyClockOffset`, `max-offset three nodes`, `clock offset three node cluster` | nothing |
| `clock jump backwards` | #25109 (open since 2018): "Consider enabling extra clock protections by default." |
| `persist upper bound` | #25109, #137331 and #24820 (the roachtest `clock/monotonic`), #139120 (its setup fixed) |
| `remote wall time is too far ahead` | #102316 (open), #152247, #146754 |
| `clock backward restart stale read`, `stale read after restart clock`, `clock monotonic restart`, `wall time upper bound` | nothing |
| `WAL files disk full flush`, `memtable never flushed`, `WAL preallocate disk space`, `store full small disk wal`, `exit status 6 log file unavailable`; in cockroachdb/pebble `WAL preallocation disk full` | nothing |

## The explorations

Both explorations ran on hvx with one worker and without shrinks (`--no-shrink`), on the
shared bare-metal machine.

| exploration | options | seeds | first runs | failures, by cause |
|---|---|---|---|---|
| e2, the skew image | `--strategy process --new-faults --service-clocks` | 1-4 | 4, each run again once with its fingerprint | seed 1: candidate 3 (`C=40` and roach1's full store); seed 2: finding 1 (`T=roach3@34:1500ms`, `[clockstops]`); seed 3: finding 1 at a restart (`T=roach3@28:-1500ms`, then `K=roach1@37+2`: `ERROR roach1 restart failed`); seed 4: a clock effect, not separated (`T=16:-2592000`, a step back of 30 days, and `K=roach1@31+9`: no node served within 180 s) |
| e1, the default image | `--strategy process --new-faults --cluster-faults --storage-faults` | 1-6 | 6, the failures run again once with their fingerprints | seeds 3 and 4: the clock effect of seed 4 of e2 (`T=28:-86400` and `T=16:-2592000`, each with a kill of one node and damaged files of a node: `[serves]`); seed 5: the environment (`G=roach1@16+1:zero,len=65536`, `ERROR roach1 restart failed (its files were corrupted)`); seeds 1, 2 and 6 passed |

Seed 1 of e1 is seed 1 of e2 with a power loss of roach1 (`L=roach1@11+3`) on the default image,
and it passed: roach1's store did not fill there.

The drawn clock of seed 2 was 1.5 s, three times CockroachDB's bound, and found finding 1 blind.
Seed 4 looks like the class of finding 2 on a running cluster: the node that was killed starts
again 30 days behind the cluster's timestamps. Its cause was not traced and its faults were not
separated; a step of 20 s and a kill of roach1 alone passed (finding 2's table).

## What hydo lacked

In the order of the time each cost:

1. **A run's diagnosis was killed by the host's out-of-memory killer.** `hydo run` maps the
   image's disks, and the kernel counted 12 GB of file pages against it while another session's
   engine held 7 GB. The run printed its verdict, but the diagnosis replay never wrote `logs/`,
   and `run.sh` got status 137. The cause of the first beyond-bound run took a second run and a
   `hydo inspect` session to read.
2. **The services' own log files were not in the run directory.** CockroachDB writes its log to
   files in the store, and `logs/<svc>.log` held only its start banner. Each cause in this page
   needed a `hydo inspect` session that replays the run (5 to 10 minutes on the loaded machine).
   `services[].logs` names the files now; it came late, because the guide's onboarding path does
   not name it. Also, `cockroach.log` is a link to the newest process's file, so the fatal lines
   of earlier processes were in other files of the same folder.
3. **A node that stops itself and is started again leaves no trace in the verdict.** hydo's
   restart policy started roach1 and roach2 again after each fatal stop, so every rule over the
   data held in the runs of finding 1. The finding showed only in `one.log`'s `exited (status 7)`
   lines. The check `clockstops` reads the logs; a generic count of a service's exits in the
   verdict or the report would have shown it at once.
4. **The image's glibc needs x86-64-v3.** The first run stopped every node with `Fatal glibc
   error: CPU does not support x86-64-v3`. The diagnosis said `exit status 127: a program it runs
   was not found`. `hydo compose` proposes no `guest.cpu: v3` (as open item 157 says for
   ClickHouse).
5. **A full file system was named by no diagnosis.** `hydo build` warned of the disk well, but
   the run's diagnosis did not name exit status 10, CockroachDB's code for a full disk, and the
   verdict was a check that timed out. In candidate 3 it said `exit status 6`. A line with each
   `storage:` file system's use at the end of a run, or at a service's exit, would have named
   the cause at once; here it took two `hydo inspect` sessions.
6. **The import.** `hydo compose` took the port 8080 from the image's `EXPOSE` for roach2 and
   roach3, and kept the one-shot service `init` as a long-running service with `restart:
   on-failure`; a service that runs `cockroach init` once is not a `job:`, because Compose's short
   `depends_on` does not say so. The import's own `storage:` sizes are on top of the data disk, so
   the first `data.size_mib` of 2,048 MiB and three file systems of 512 MiB passed hvx's 4,096 MiB.
7. **A cluster's age.** A three-node cluster 40 s after `cockroach init` had ranges of two
   replicas. Nothing in hydo says when a system's replication is complete; the client waits for it
   (section "Not bugs").
8. **A regression from an exploration without shrinks.** `hydo save` saves minimal schedules, and
   with `--no-shrink` there were none, so candidate 3's schedule was copied by hand and its `W=`
   kept the seed form; `hydo repro` warned that it composes from the current command list.
9. **The shared machine.** Runs took 2 to 17 minutes of wall time for 35 to 110 s of guest time
   on the loaded bare-metal machine, and each waited for 10 GB of free memory first.

## Time

| phase | wall time |
|---|---|
| scouting and the fit (the image, the memory, hvx's disk) | 30 minutes |
| setup to a first clean run: the glibc, the full store, `crdb_internal`'s access | 2 hours |
| the clock runs of the skew image and the whole guest, and their logs | 4 hours |
| the final images, the regressions twice, `hydo repro --regressions` | 3 hours |
| the explorations e2 and e1 | 1 hour 25 minutes |
| the goldens and the tests, beside the runs | 3 hours |
| the duplicate searches and the write-up | 1 hour |

Most of the wall time was the shared machine: a run of 35 to 150 s of guest time took 2 to 17
minutes, and many waited for 10 GB of free memory first.
