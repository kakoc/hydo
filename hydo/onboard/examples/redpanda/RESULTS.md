# Redpanda: results (Milestone 279, tentative number)

Redpanda is a streaming platform that speaks Kafka's protocol, written in C++ on the Seastar
framework. This example runs a three-node Redpanda cluster in one hvx guest and checks what
Redpanda promises its producers and consumer groups, under hydo's faults. The cluster is the
latest release, v26.2.7 (2026-11-12, `redpandadata/redpanda`). The newest release tag of the
public repository is v26.2.2 (August 2026), so this file reads the code at v26.2.2, and it
checks each cited place again on the `dev` branch of 2026-11-12.

## What Redpanda promises

The documentation's page "Configure write caching" (Develop, Topic configuration) states the
durability of an acknowledged write:

- "When write caching is disabled, Redpanda fsyncs each acks=all batch to disk on a majority of
  replicas before it acknowledges the write." Write caching is off by default in production
  mode and on by default in development mode.
- With write caching on, a write is acknowledged once a majority holds it in memory. Only enable
  it "on workloads that can tolerate some data loss in the case of multiple, simultaneous broker
  failures".
- In either mode, "data written in the context of a transaction and consumer offset commits is
  always written to disk and fsynced before being acknowledged to the client."

The page "Transactions" adds: "either all messages are committed or none", a `read_committed`
consumer "can only process successfully committed transactions", and "Redpanda's default
configuration supports exactly-once processing". The idempotent producer
(`enable_idempotence`) and transactions (`enable_transactions`) are on by default.

The example's check states these promises as claims of one `type: messages` check (`hydo guide
messages`):

| claim | what it says here |
|---|---|
| `no-loss` | every record whose produce or transaction commit was acknowledged is read back at the end |
| `no-phantom` | no record of an aborted transaction is ever read with `read_committed` (an "aborted delivery") |
| `duplicates: forbidden` | every delivery of a record carries one offset: the idempotent producer stores it once |
| `order: {kind: log, scope: key}` | one record per offset of a partition, one offset per record, and each reader reads a partition upwards without gaps |
| `fan_out: every-group` | both readers get every record: the full read of every partition and the consumer group `g` |
| `after_ack: no-redelivery` | after an offset commit of group `g` was acknowledged, no later member of `g` receives those records again |

## What Jepsen reported in 2022

Jepsen's analysis of Redpanda 21.10.1 (https://jepsen.io/analyses/redpanda-21.10.1, April 2022)
lists these issues. A finding of this example that matches one of them is not new.

| issue | what it was | status in the analysis |
|---|---|---|
| (Jepsen 1) | duplicated writes with the default client settings: idempotence was off | default changed in 22.1.1 |
| 3039 | duplicated writes with idempotence on: no deduplication on the server | fixed in 21.10.3 |
| 3335 | an assertion failure while deallocating partitions, after membership changes | open then |
| 3336 | an assertion failure in `response.partition_index` after a crash | fixed in 22.1.1 |
| 3003 | inconsistent offsets: uncommitted Raft entries applied, records reordered | fixed in 21.10.3 |
| (Jepsen 6) | acknowledged records at the end of a partition never read, after pauses, crashes or partitions | under investigation then |
| KAFKA-13574 | `NotLeaderOrFollower` is not a definite failure, so a "failed" send may succeed | documentation |
| (Jepsen 8) | write cycles (G0) between transactions on one partition | by design (Kafka does the same) |
| 3036 | aborted reads and circular information flow: the visible offset went past uncommitted writes | fixed in 21.10.2 |
| (Jepsen 10) | non-monotonic polls inside a transaction after a consumer rebalance | documentation |
| 3616 | aborted reads after `InvalidTxnState`, and lost transactional writes after a lost and regained leadership | fixed in 21.11.15 |

Jepsen also tested clock jumps and found nothing. It did not test power loss of a node or
corrupted files.

## The example

| file | what it is |
|---|---|
| `docker-compose.yaml` | the same three nodes under Docker, for runs without hydo |
| `conf/n1.yaml` … `conf/n3.yaml` | each node's configuration: node id, the three seed servers, the listeners |
| `conf/bootstrap.yaml` | the cluster's properties at its first start, write caching off |
| `conf/bootstrap-write-caching.yaml` | the same with write caching on |
| `client/` | `rpclient`, the test client (Go, franz-go v1.22.2, static) |
| `hydo/hydo.yaml` | the three nodes, the group `nodes`, the workload and the messages check |
| `hydo/workload/` | `parallel_driver_produce`, `parallel_driver_txn`, `serial_driver_consume`, and the drain |
| `hydo/regressions/` | the saved schedule of finding 1 |
| `native/` | the scripts that run Docker: findings 1, 3 and 4 without hydo, and the write-caching configuration |

Each node runs the image's `redpanda` program directly, as `rpk redpanda start --mode
dev-container` would run it, with `--smp 1 --memory 1G --reserve-memory 0M --overprovisioned`.
The development mode also sets two options that change durability, and the example leaves both
out. It does not pass `--unsafe-bypass-fsync`, and it keeps write caching off, which is the
production default. The cluster takes the mode's other properties, such as three partitions of
`__consumer_offsets` and no rebalance delay. Two more properties make the cluster fit one
guest: 3 transaction-coordinator partitions (default 50), and segments that grow in steps of
1 MiB (default 32 MiB). Each node's data directory is a file system of 384 MiB of its own
(`services[].storage`), so that `G=` and `L=` can hit one node.

The client's commands record a `HYDO-HIST` history named `rp`:

- `produce`: one record to the partition the seed picks, with franz-go's defaults: the
  idempotent producer and `acks=all`. An acknowledged record is `ok` with its offset, and a
  produce without an answer is `info`.
- `txn`: two records to two partitions in one transaction. The transactional id comes from a
  pool of 4, so concurrent commands fence each other. The transaction commits 3 times in 4. A
  completed commit is `ok`, a completed abort is `fail`, and anything else is `info`.
- `consume`: one member of group `g` at a time (a serial driver), reading with
  `read_committed` from the group's committed offsets. It polls 10 to 49 records and commits
  their offsets. A commit is the messages history's `commit p<k>@<offset>`.
- The drain (the check's `final:`) reads every partition from the start with
  `read_committed`, then group `g` from its committed offsets. It waits until every leader
  answers, no transaction is open (the last stable offset equals the high watermark), and nothing
  arrives for 8 seconds. It gives up after 150 and 230 seconds, and writes its reason to the
  console.
- `lso`: the invariant `tx.no_hanging` (see finding 2) runs it after the heal; it fails while a
  partition's last stable offset is below its high watermark.
- `dump <partition> <from>`: one raw `read_committed` fetch from the partition's leader, with
  the aborted transactions it reports and each batch's producer, epoch and control marker (see
  finding 3).

A transaction also writes the console event `rp.txn_add` before it adds its partitions, for
reactions (`A=`).

Build and run on Linux with hvx:

```sh
sh hydo/fetch-images.sh                    # the images by digest (network, once)
sh client/build.sh amd64; sh client/build.sh arm64
hydo build hydo/hydo.yaml                  # write caching off
hydo build hydo/hydo.yaml --set bootstrap=bootstrap-write-caching.yaml
hydo run "disk=none | run"
hydo explore --seeds 1-60 --strategy process --more-faults --new-faults --storage-faults --cluster-faults
```

### What the guest needed: a cgroup v2 mount for Seastar

The first boot ended with `ERROR service start (n1)`. Every node stopped at once with `Could
not initialize seastar: std::out_of_range (sstring out of range)`, and its restart policy
started it again until the readiness deadline. The guest's `/proc/self/cgroup` was empty. hvx's
guest kernel has cgroups, but Linux shows the cgroup v2 line in that file only after the cgroup2
file system has been mounted once. Seastar parses that file at start and fails on an
empty one. Each node's `setup:` then mounted cgroup2 on `/sys/fs/cgroup`, and the nodes started.

Since Milestone 283 hydo mounts cgroup2 at every boot of an image built at fingerprint epoch 4 or
later (`guest.cgroup2`), and no `setup:` mounts it. The values in this file are from epoch-3
images with the `setup:` mount. On hvx the epoch-4 image without it gave `OK` and one fingerprint
over two runs of `disk=none | run` (`c49b22134b52331c`).

## Without faults

Every run used hvx on the bare-metal machine, with this branch's `hydo`, `hydo-build` and
`hydo-hvx` over the 0.3.4 Linux release, at the fingerprint epoch 3. The client was built with
Go 1.27.3 (`client/build.sh`).

| image | schedule | result |
|---|---|---|
| `base`: write caching off (`explore-ob-redpanda-disk.img` `2ffbd469…`) | `disk=none \| run`, twice | OK both times, `fp=5157c617d4767009`, 163 commands |
| `base2`: the client writes the drain's reason to the console (`be52d3c1…`) | the same, twice | OK, `fp=ab8ed796605e7359` both times |
| `base3`: the invariant `tx.no_hanging` and the regression of finding 1 (`5c943d90…`) | the same, twice | OK, `fp=288c5fe34940f059` both times; `tx.no_hanging` held 0.02 s after the heal |
| `wc2`: write caching on, the client of `base2` (`c7da8c21…`) | (directed runs only, below) | |
| `base4`: the client's event `rp.txn_add` (`888a8ed0…`) | (reaction runs only, finding 3) | |
| `base6`: the client's `dump` (`53d15f53…`) | `disk=none \| run`, twice | OK, `fp=aa6ee52ec59b96b6` both times |
| `base7`: finding 3's regression as the one-kill reaction schedule (`706de89d…`) | `disk=none \| run`, twice | OK, `fp=dbe6a8ebad1212ca` both times |
| `base8`: the committed folder, finding 1's `expect` line updated (`630d04fa…`) | `hydo repro --regressions` | 2 of 2 as expected: finding 3's `VIOLATION` (`fp=460f49a057d10a5a`), finding 1's `fp=159d182a7374c040` |
| `trace`: the committed folder with the `tx` logger at trace level (`65b1c401…`) | (reaction runs only, finding 3) | |

The images with write caching off differ only in the client and in what `hydo/` held when
they were built. The blind explorations ran on `base`, `base3` and `base8`.

A run takes about 130 seconds of guest time and 3 minutes on the machine. It acknowledges 163
records, 22 sends end `fail` (the aborted transactions) and 8 `info`. Each of the six claims
judges something: 163 records for `no-loss`, `duplicates` and `fan-out`, 263 offsets for
`order`, and 1,789 pairs of a commit and a later receive for `after-ack`.

## The durability promises held

These directed schedules ran once each, on the image with write caching off (`base2`,
`be52d3c1…`) and on the image with write caching on (`wc2`, `c7da8c21…`). Every one ended OK,
with all six claims held and no acknowledged record lost.

| image | schedule | what it does | acknowledged records |
|---|---|---|---|
| write caching off | `disk=cut \| run C=60` | the whole guest loses power before step 60, on a disk whose cache loses what was not flushed | 85, all read back in the recovery boot |
| write caching off | `disk=none \| run P=n1@2+10 L=n2@10+2:strict K=n3@12+6` | n1 is paused, so n2 and n3 acknowledge alone; n2 loses every write it had not synced, with its links cut; then n3 is killed, so n1 and n2 must elect a leader with every acknowledged record | 163 |
| write caching off | `disk=none \| run L=n1@40+3:strict` | the strict power loss of one node | 163 |
| write caching off | `disk=none \| run L=n2@70+3:strict` | the same, later and on another node | 163 |
| write caching on | `disk=cut \| run C=60` | the whole guest loses power before step 60 | 85 |
| write caching on | `disk=cut \| run C=90` | the same before step 90 | 126 |

The second schedule is the shape of Jepsen's NATS finding 3.5, which loses acknowledged messages
on NATS 2.12.1 (`hydo/onboard/examples/nats-jetstream/storage`). Redpanda keeps them. Its writes
go to the disk with direct I/O, and it fsyncs a batch before it acknowledges it, so the strict
power loss finds nothing unsynced to drop: the console showed less than 400 kB dirty in the
whole guest at each loss.

With write caching on, the two whole-guest cuts lost no record either. The cut came at a step
boundary, with no acknowledgement younger than Redpanda's flush interval (100 ms), so these two
runs do not show the loss that the documentation allows. They do show that transactional
records and offset commits survived. The code agrees with the documented promise: transactional
batches are replicated with a forced flush (`make_replicate_options` in `cluster/rm_stm.cc`), and
`ntp_config::write_caching()` turns write caching off for every topic that is not a user topic,
so `__consumer_offsets` is always flushed.

## Finding 1 (new candidate): one partition's zeroed snapshot stops the whole node at every start

**What happens.** Each partition of Redpanda keeps the state of its transactions and idempotent
producers in a small snapshot file next to its log, `tx.snapshot` (53 bytes here). When that
file of one partition is damaged, the node stops at start with an assertion, for every
partition and every client of the node, and its restart policy starts it again into the same
assertion. Under Docker the node crashed 8 times in 20 seconds:

```text
Assert failure: (src/v/raft/persisted_stm.cc:579) 'false' [[{kafka/t/1}] (tx.snapshot)]Can't load
snapshot from '/var/lib/redpanda/data/kafka/t/1_9/tx.snapshot'. Got error: std::runtime_error
(Corrupt snapshot. Failed to verify header crc: 3152373923 != 0: ...
```

**The schedule.** The blind exploration's seed 6 ended with `ERROR n1 restart failed (its files
were corrupted)`. Its `G=n1@1+2:zero,len=4096,seed=626bd178` killed n1 before step 1, zeroed
the first 53 bytes of `kafka/t/1_11/tx.snapshot`, and started n1 again; n1 never became ready.
The token alone, with the file named, is the saved regression
`hydo/regressions/zeroed-tx-snapshot-stops-node.schedule`: on every image n1 stopped at the
same assertion, twice each, and on the committed folder's image (`base8`) both runs gave
`fp=159d182a7374c040`, the value its `expect` line names. Its fingerprint depends on the other
regression files the image carries, not on its own `expect` line: two images that differed only
in that line gave one fingerprint.

**The cause, in the code** (v26.2.2, and the `dev` branch of 2026-11-12).
`persisted_stm_base::start()` in `src/v/raft/persisted_stm.cc` loads the local snapshot inside a
`try` block, and its `catch` block is `vassert(false, "... Can't load snapshot from ...")`. Further down, the same
function treats a missing snapshot ("starting without snapshot") and a snapshot that the state
machine rejects ("will be recovered from the log") as normal: the state is rebuilt from the
log. So the snapshot is a cache of the log, yet a damaged copy stops the node instead of being
dropped. Every state machine on this path behaves alike: `tx.snapshot` of `rm_stm`, and the
snapshots of the transaction coordinator, the id allocator, the consumer groups' transaction
tracker and the archival metadata.

**Is it documented?** No page of Redpanda's documentation that this search found says that a
damaged snapshot file stops a broker, or how an operator recovers. Deleting the file lets the
node start, because a missing snapshot is rebuilt from the log.

**Is it a bug?** It is an availability defect, not a loss of data. In a cluster of three, one
node is then down for good, and one more failure stops every partition. Redpanda writes the
snapshot to a partial file, renames it, and syncs the directory, and no power cut or strict power
loss in this work damaged it: the damage needs a bad disk or a stray write. A careful reader could
call the assertion a deliberate choice to stop on damaged files. Even so, the code's own
fallbacks for a missing or rejected snapshot show that the node could rebuild the state.

**Without hydo.** `native/zero-snapshot.sh` starts the cluster of `docker-compose.yaml`
(v26.2.7, arm64), stops n1, zeroes 53 bytes of its `tx.snapshot`, and starts it: the node is
`restarting` with the assertion above on every start.

**Another state machine.** Seed 35 of the fourth exploration (`base8`) zeroed the first 68 bytes
of n1's `kafka_internal/tx/0_12/tx.coordinator.snapshot`, the transaction coordinator's
snapshot, and n1 did not start again either (`ERROR n1 restart failed`).

**A second schedule, with bit flips.** The directed `G=n1,n2@40+3:flip,n=8,file=kafka/t/` flipped
8 bits of n1's `kafka/t/0_11/tx.snapshot` and 8 bits of n2's `stm_manager.snapshot`. n1 stopped
at the same assertion ("Failed to verify header crc: 2983498800 != 250850419").

**Duplicate search** (GitHub issues, pull requests and discussions of redpanda-data/redpanda,
every state, on 2026-11-16; Jepsen's list above). The terms: "Can't load snapshot", "Failed to
verify header crc", "Corrupt snapshot", `tx.snapshot`, "snapshot crc mismatch", "corrupted
snapshot startup", "snapshot corruption crash", "persisted_stm snapshot corrupted",
"persisted_stm.cc assert", "crash loop snapshot", and in discussions "snapshot corrupt",
"corruption", "crash loop". No issue reports it. Two results are close:

- Issue 4950 (2022, closed) is about a deleted controller directory: that node hung at start,
  and the reporter asked for it to stop instead. It is not this case.
- Pull request 24662 ("raft: Make load_snapshot exception safe", merged on 2025-01-02) changed
  this function. Its description says: "If the metadata can't be loaded (snapshot is truncated)
  the method returns nullopt which should trigger log replay." The merged code closes the
  reader and throws the exception again, and `start()` still asserts on it. So the behaviour
  that the pull request describes, a replay of the log, is what this finding asks for, and it is
  not what v26.2.2 and v26.2.7 do.

## Finding 2 (new candidate): a transaction recorded as empty is never aborted

**What happens.** A transaction stays open on its partitions after its client is gone, past its
timeout and for good. While it is open, the partition's last stable offset does not move, so a
`read_committed` consumer reads nothing beyond it. That covers every consumer group and every
transactional consume-transform-produce application. Each partition asks the transaction
coordinator every 10 seconds whether to abort, and the coordinator fails each time with:

```text
WARN tx - [{kafka_internal/tx/2}] - tm_stm.cc:321 - [tx_id=tx-0] error updating transaction
status - Can not update transaction state from empty to expired as this transition is invalid
WARN tx - tx_gateway_frontend.cc:602 - [tx_id=tx-0] error aborting transaction - 2
```

The partition then logs that the transaction's state is unknown (`rm_stm.cc`, "state of
transaction ... ms_since_last_update: 230240ms ... timeout: optional(10000ms)") and keeps it
open. In the run below, the transaction of producer id 5 kept partition 0 of `t` open until the
drain gave up, 230 seconds after its 10-second timeout had passed.

**The cause, in the code** (v26.2.2, and the `dev` branch of 2026-11-12). A partition that
times out a transaction calls the coordinator's `do_try_abort` in
`src/v/cluster/tx_gateway_frontend.cc`. When the coordinator's record has the same producer id
and sequence, the function switches on its status:

```cpp
    case tx_status::empty:
        [[fallthrough]];
    case tx_status::ongoing: {
        auto killed_tx = co_await stm->update_transaction_status(
          term, tx.id, tx_status::preparing_internal_abort);
```

The coordinator's state machine (`is_state_transition_valid` in `src/v/cluster/tm_stm_types.cc`)
allows `preparing_internal_abort` ("expired" in the logs) only from `ongoing` or
`preparing_internal_abort`, not from `empty`. So for a record in `empty` the update fails with a
conflict, `do_try_abort` returns an error instead of "aborted", and the partition asks again
without end. The coordinator's own expiry of the transactional id does not help sooner: it
runs after `transactional_id_expiration_ms`, 7 days by default.

The new coordinator logged the record it found, every 10 seconds:

```text
[tx_id=tx-0] found transaction {id: tx-0, status: empty, pid: {producer_identity: id=5,
epoch=12}, last_pid: {producer_identity: id=-1, epoch=-1}, etag: 3, seq: 0, partitions: } to abort
```

The record has the partitions' producer id and sequence, and no partitions at all. A record is
`empty` after `InitProducerId` and before the coordinator stores the transaction's partitions.
`do_add_partition_to_tx` first starts the transaction on each data partition (`begin_tx`) and
only then stores the partitions (`stm->add_partitions`, which also makes the record `ongoing`).
A coordinator that fails over between the two writes, or whose second write fails, leaves
exactly this state behind. The run's faults fit that: the coordinator partition
`kafka_internal/tx/2` changed its leader four times. n2 led it first and lost its power before
step 11, n1 led it next and was paused before step 16 and killed before step 60, and n1 led it
again after n2 was killed before step 105.

**Is it documented?** The page "Transactions" says that a long timeout means "the longer
consumers may be blocked", so a client's timeout bounds the wait. Nothing says that a
transaction can stay open after its timeout. The switch itself sends `empty` down the path of
`ongoing`, which shows the intent: the transaction should be aborted.

**The schedule.** The blind exploration's seed 9 (`base` image) ended with `ERROR check rp...
could not be evaluated: its final: read-back exited with status 1`:

```text
disk=none dseed=a16e4eaefdb58ac0 | run K=n1@60+1 P=n1@16+7 O=n2-n3@80-120:resp:delay,2282ms W=5372f125,p3,q1,r1000 G=n2@105+2:flip,seed=108b1a43 L=n2@11+3
```

`hydo explore --schedule` of it on the same image gave the same failure again, and `hydo repro
--transcript` of that run gave it a third time, with the logs above.

**The faults, one at a time.** The bit flip is not needed: with `file=pid.lock` in place of the
seeded file, the flip changes only the node's lock file, and the run hangs the same way. What the
`G=` token still does is kill n2 before step 105 and start it again before step 107. Each of the
other faults was left out once, through `hydo explore --schedule` on the same image:

| left out | result |
|---|---|
| the corruption (`G=`, so also n2's kill before step 105) | OK |
| the one-way delay between n2 and n3 (`O=`) | OK |
| the corruption's file, replaced by `pid.lock` | the same hang |

Leaving out a fault moves the timing of everything after it, so these runs show only that the
hang needs a narrow window. A fresh boot of the schedule (`hydo run` on the image with the next
client build, `base2`) passed too. The cause in the code does not depend on the faults: any
failure between the two writes of `do_add_partition_to_tx` leaves the record in `empty`.
No reproduction without hydo was tried, because the cause is plain in the code.

**Duplicate search** (GitHub issues, pull requests and discussions of redpanda-data/redpanda,
every state, on 2026-11-16; Jepsen's list above). The terms: "hanging transaction", "state of
transaction" unknown, "from empty to expired", "error aborting transaction", `do_try_abort`,
`preparing_internal_abort`, `try_abort_old_tx`, "transaction stuck LSO", "last stable offset
stuck", "stuck LSO", "lso not advancing", "stuck transaction read_committed", "open transaction
never aborted", "transaction timeout not aborted", "invalid state transition" transaction, and in
discussions "hanging transaction", "stuck transaction", "last stable offset". No result
describes this state. The closest:

- Issue 14624 (2023, closed): a transaction that "cannot be expired" after its topic was deleted,
  with the coordinator answering "not coordinator". Another cause.
- Issue 24468 (2024, closed): an internal abort that fences the client. The maintainers found it
  correct.
- Issue 8236 (2023, closed), "Transaction stuck": on 22.3.10 a producer could not begin a
  consumer group's transaction, because the group still held an older one.
- Issue 17966 (2024, closed as stale): a partition whose end a `read_committed` consumer never
  reached after a restart. No cause was found.
- Pull request 30071 (in 26.2.1): "dangling open transactions" from a race between snapshots and
  compaction of `__consumer_offsets`. It is about consumer-group transactions, not data partitions.
- Pull request 21504 (24.3.1) added `transaction_max_timeout_ms`, "to prevent hanging transactions
  from blocking consumer progress". It bounds the client's timeout; it does not cover a
  transaction that outlives its timeout.

Jepsen's 3616 (lost transactional writes after a lost and regained leadership) was fixed in
21.11.15; it lost writes, and this candidate loses none.

**A second schedule.** Seed 37 of the fourth exploration (`base8`), with the invariant
`tx.no_hanging`, reached the same state on another path:

```text
disk=cut dseed=b867e156d88e8c87 cf=1279 | run T=81:-300 O=n1-n3@30-43:resp:delay,1090ms W=d6d260d0,p3,q1,r500 L=n2@10+2
```

n2 loses its power before step 10, the links between n1 and n3 delay their answers, and the
whole guest loses power at the disk's 1,279th flush. In the recovery boot partition 0 kept a
transaction of producer 1005 open: `VIOLATION [tx.no_hanging] not satisfied 90.24s after heal
... p0 has an open transaction: last stable offset 27, high watermark 28`. `hydo repro
--transcript` of the run gave the same violation, and the nodes logged the same loop for
`tx-0`: `found transaction {id: tx-0, status: empty, pid: {producer_identity: id=1005,
epoch=5}, ... partitions: }`, then "Can not update transaction state from empty to expired".
Neither schedule is a saved regression: each reproduces on the image it was found on, and every
change of the `hydo/` folder or the client moves the timing.

**What the example checks now.** Such a hang ends the run with an ERROR, because the
drain cannot finish, and hydo's explorer shrinks only violations. The image `base3` adds the
`eventually` invariant `tx.no_hanging`: 90 seconds after the faults heal, every partition's last
stable offset has reached its high watermark (`rpclient lso`). A hanging transaction is then a
`VIOLATION [tx.no_hanging]`, which the explorer can shrink.

## Finding 3 (new candidate): a committed transaction visible on one partition only

Reported as redpanda-data/community#395 (2026-11-17). A Redpanda engineer answered on 2026-11-18
that it is fixed for the release v26.3.1, and closed the issue.

**What happens.** Transaction 169 wrote `m169a` to partition 1 and `m169b` to partition 2 and
committed: franz-go's `EndTransaction(TryCommit)` returned no error, and the client recorded both
records as acknowledged. Every `read_committed` reader then read `m169b` (partition 2, offset
178) and never `m169a` (partition 1, offset 149), also after the heal, while the records after
it on partition 1 (`m170b` at offset 152, `m172`, `m173`) were read. The messages check reported:

```text
VIOLATION [rp.no-loss] m169a was acknowledged to process 169 at 181.660 s and never delivered in
channel t, which was drained; m170b of the same key at offset 152 was delivered, so offset 149
is a gap
```

`m169a` is in partition 1's log: at the end of the run, a `read_uncommitted` read of partition 1
(`rpk topic consume t -p 1 -o 145`) returned `149 m169a`, and no partition had an open
transaction. So partition 1 holds the record as aborted, and partition 2 holds the same
transaction as committed. The page "Transactions" promises "either all messages are committed
or none".

**The schedule.** Seed 24 of the third blind exploration (`base3`):

```text
disk=none dseed=2aa0ad4523ae213f | run K=nodes.primary@66+1 P=n2@42+11 T=106:-86400 W=02ecd045,p2,q2,r500
```

n2 is paused for steps 42 to 52, the leader of partition 0 (n3) is killed before step 66 and
started again before step 67, and the guest's wall clock steps back one day before step 106.
Transaction 169 ran in round 116, after the clock step. `hydo repro --transcript` of the run gave
the same fingerprint (`2056e09f7de8c39a`) and the same violation, `hydo explore --schedule` gave
it again, and the explorer's own reproduction of the seed gave it a fourth time.

**The faults, shrunk.** `hydo explore --seeds 24-24` with the explorer's shrink (`base3`) left out
the clock step, then tried shorter pauses, other kill targets and smaller compositions, and kept:

```text
disk=none dseed=2aa0ad4523ae213f | run K=nodes.primary@66+1 P=n2@42+11 W=02ecd045,p2,q2,r500
```

Both runs of the minimal schedule broke the same claim the same way, with transaction 155
(`m155a` lost on its partition, `m156b` read at a later offset). Without the pause of n2, or
without the kill of the leader, the run passed. The clock step is not needed. The two faults end
before step 67; the transaction that breaks runs about 50 steps later, so the faults leave
something behind that a later commit meets.

`hydo run` of the minimal schedule from a fresh boot broke the claim the same way, twice, with
one fingerprint (`673dbce467076077`, `base3`). On the image of the committed folder (`base6`,
with the client's later commands) the same schedule passed: the timing had moved. The saved
regression `hydo/regressions/half-committed-transaction.schedule` (`expect: violation`) is
therefore the one-fault reaction schedule below, which breaks the claim on that image.

**A single kill is enough.** With the client's console event `rp.txn_add` (written just before
a transaction adds its partitions; image `base4`), the reaction `A=rp.txn_add@20+<ms>:kill-<node>`
kills one node at that moment and keeps it down until the heal. Of nine such runs (each node, at
0, 3 and 8 ms), five broke the claim the same way, with no other fault. The schedule
`disk=none | run A=rp.txn_add@20+3:kill-n1` broke it on both runs on `base6`
(`fp=5123749b8edaeabc`) and on both runs on `base7` (`fp=460f49a057d10a5a`), with transaction
118 each time.

**What the partitions hold.** The client's `dump` command (image `base6`) sends one raw
`read_committed` fetch to each partition's leader and prints every batch with its producer id,
epoch and control marker. At the end of that run, `hydo inspect exec` gave for producer id 9:

```text
partition 1 (leader n3)
batch base=107 producer=9 epoch=7 txn=true  107:m118a
batch base=108 producer=9 epoch=7 txn=true  control marker=abort
batch base=110 producer=9 epoch=8 txn=true  110:m117b
batch base=111 producer=9 epoch=8 txn=true  control marker=commit
partition 0 (leader n2)
batch base=111 producer=9 epoch=7 txn=true  111:m118b
batch base=112 producer=9 epoch=7 txn=true  control marker=commit
```

The same producer, 9 at epoch 7, has a commit marker on partition 0 and an abort marker on
partition 1. Every Kafka client then drops `m118a`, so this is not the client's filter.
Commands 117 and 118 ran at the same time with the same transactional id: 118 kept epoch 7 and
committed, and 117 got epoch 8 later. Both clients' commits returned no error, 118's at
124.44 s and 117's at 124.49 s.

**The trace.** Redpanda's transaction code logs to one logger, `tx`. The image `trace` is the
committed folder with `--logger-log-level tx=trace` added to each node's command line. On it the
regression's schedule passed, because the logging moved the timing. The nine reaction variants
broke the claim five times, as on `base4`:

| reaction | n1 | n2 | n3 |
|---|---|---|---|
| `A=rp.txn_add@20:kill-<node>` | OK | OK | OK |
| `A=rp.txn_add@20+3:kill-<node>` | OK | VIOLATION | VIOLATION |
| `A=rp.txn_add@20+8:kill-<node>` | VIOLATION | VIOLATION | VIOLATION |

The run of `A=rp.txn_add@20+3:kill-n2` (`fp=2680039ca02e7bb9`) lost `m117a` on partition 2. Two
clients used the transactional id `tx-2`, whose coordinator was n1. Partition 2's leader was n3,
and partition 1's leader was n1. These lines of the trace, shortened, are in the order of the
guest's clock:

```text
00:01:58,625 n1 [tx_id=tx-2] sending name:init_tm_tx, pid: {id=9, epoch=7}, ec: none     (client A)
00:01:58,629 n1 [tx_id=tx-2] adding partition to tx. pid: 9, epoch: 7, partitions: [2]   (client A)
00:01:58,631 n1 [tx_id=tx-2] init_tm_tx request begin, expected_pid: none                (client B)
00:01:58,643 n1 [tx_id=tx-2] tx is ongoing, aborting                                     (client B)
00:01:58,643 n1 [tx_id=tx-2] adding partition to tx. pid: 9, epoch: 7, partitions: [1]   (client A, waits)
00:01:58,654 n3 [kafka/t/2] Adding aborted transaction range: pid: {id=9, epoch=7}, range: [112, 113]
00:01:58,656 n1 [tx_id=tx-2] finishing transaction with status of: completed_abort
00:01:58,661 n1 [tx_id=tx-2] reset transaction state to: {status: empty, pid: {id=9, epoch=7}, seq: 1, partitions: }
00:01:58,661 n1 [tx_id=tx-2] sending name:init_tm_tx, pid: {id=-1, epoch=0}, ec: concurrent_transactions
00:01:58,670 n1 [tx_id=tx-2] begin_tx request for pid: {id=9, epoch=7} at ntp: {kafka/t/1} result: none
00:01:58,674 n1 [tx_id=tx-2] end transaction. producer id: 9, producer epoch: 7, committed: true
00:01:58,684 n1 [tx_id=tx-2] finishing transaction with status of: completed_commit   (seq: 1, partitions: kafka/t/1)
00:01:58,689 n1 [tx_id=tx-2] sending name:init_tm_tx, pid: {id=9, epoch=8}, ec: none   (client B, again)
```

Client A wrote to partition 2 in transaction sequence 0. Client B's InitProducerId aborted that
transaction and kept the epoch at 7. Client A's next AddPartitionsToTxn, still at epoch 7, then
started transaction sequence 1 with partition 1 only, and client A's commit committed it. No line
of the trace is a warning or an error.

**The cause** (v26.2.2, and the same three files on `dev` at `d9944871`). Three places in
`src/v/cluster/tx_gateway_frontend.cc` act together:

1. `do_init_tm_tx` (line 1011) handles an InitProducerId whose transactional id has an `ongoing`
   transaction. It aborts the transaction with `do_abort_tm_tx` and answers
   `CONCURRENT_TRANSACTIONS`. `do_abort_tm_tx` ends with `finish_transaction(...,
   completed_abort)` (line 2023) and does not set the argument `bump_producer_epoch`. So the
   coordinator still holds the old producer's epoch.
2. The old producer's next AddPartitionsToTxn passes `get_latest_tx` (line 2517), because the
   stored producer id and epoch equal the request's. `init_add_resource_to_tx` (line 1264) sees
   a finished transaction and calls `tm_stm::reset_transaction_state`. That function starts a
   new transaction with the next sequence number and no partitions, and the request's partition
   joins it.
3. The old producer's EndTxn commits the new transaction and answers with no error. Its records
   on the partitions it added before the abort belong to the aborted transaction.

The Kafka protocol has no transaction sequence number, so the client cannot see that its
transaction became two. The same file bumps the epoch when it aborts an expired transaction:
`maybe_progress_transaction` (line 2134) sets `needs_epoch_bump` for
`preparing_internal_abort`, and the old producer is then fenced.

**What Kafka does.** Apache Kafka's `TransactionCoordinator.scala` handles InitProducerId for an
`Ongoing` transaction in `prepareInitProducerIdTransit`. It aborts the transaction with
`txnMetadata.prepareFenceProducerEpoch()` and answers `CONCURRENT_TRANSACTIONS`. The abort raises
the epoch, so the old instance's next request fails with `PRODUCER_FENCED`. KIP-98 says that
InitPidRequest "bumps up the epoch of the PID, so that the any previous zombie instance of the
producer is fenced off." It also promises "exactly one active producer with a given
TransactionalId." Two producers with one transactional id are the case that fencing exists for.

**What the kill does.** franz-go adds a transaction's partitions in one AddPartitionsToTxn per
partition leader. In the trace the two partitions had leaders on two nodes. So client A sent two
requests 14 ms apart, and client B's InitProducerId came between them. With one leader for both
partitions, one request adds both, and the abort covers the whole transaction. The kill moves
leaders. Why the runs without faults passed was not traced.

The earlier reading of this finding was wrong. `do_commit_tm_tx` answering before the commit
markers is not the cause: the commit reached every partition of its transaction. The abort on
partition 1 of the `base6` dump came from the other command's InitProducerId.

**Without hydo.** `native/half-commit.sh` starts the cluster of `docker-compose.yaml`. It builds
`native/halfcommit` (Go, with franz-go 1.22.2 as in the guest's client) and runs it in the
client container. Its mode `steps` makes the order of the trace in three steps:

1. Client A writes to partition 1.
2. Client B sends InitProducerId with the same transactional id.
3. Client A writes to partition 2 and commits.

Its mode `race` runs pairs of producers with one transactional id at the same time,
without a fault. Then it reads partitions 1 and 2 with `read_committed`. On this Mac (Docker in
Colima, arm64, v26.2.7), `sh native/half-commit.sh 10` gave:

```text
9: steps-21-9: A p1@28; B's InitProducerId: CONCURRENT_TRANSACTIONS: ...; A p2@28 produce: <nil>; A commit: <nil>
commits without error: 10 of 10
committed transactions 10 (20 records); records a read_committed reader never read: 10
HALF-COMMIT s21-0a: never read with read_committed, in the log at p1@1
...
```

`rpclient dump` shows each case as one producer and epoch with an abort marker on partition 1 and
a commit marker on partition 2:

```text
batch base=1 last=1 producer=1 epoch=0 txn=true control=false 1:s21-0a        (partition 1)
batch base=2 last=2 producer=1 epoch=0 txn=true control=true marker=abort     (partition 1)
batch base=1 last=1 producer=1 epoch=0 txn=true control=false 1:s21-0b        (partition 2)
batch base=2 last=2 producer=1 epoch=0 txn=true control=true marker=commit    (partition 2)
```

The mode `race` with 200 pairs and no fault gave 205 commits without error, and 5 of them lost
their record on partition 1. The same program against Apache Kafka 3.9.1 (one broker of the
image `apache/kafka:3.9.1`) gave no such commit. In `steps` client A's produce to partition 2
failed with `PRODUCER_FENCED`, and its commit was not attempted, 5 times of 5. In `race` 100
pairs gave 123 commits, and every record of them was read.

**Duplicate search** (the same sources, on 2026-11-17). The terms: "partially committed
transaction", "transaction partially visible", "atomicity violation transaction", "aborted on one
partition committed", "commit_tx" rejected, "transactional read_committed missing", "tx
atomicity", "transaction not atomic", "committed transaction missing records", "commit marker
missing", "clock jump transaction", and in discussions "atomic transaction", "partial commit".
The closest is issue 17921 (2024, v23.1.7, closed as stale): a transaction whose
`commit_transaction()` succeeded and whose records were never visible, with "commit_tx on data
partition ... was rejected" in the coordinator's log. A maintainer pointed at state lost in a
crash between begin and commit, fixed in 24.2. Here the version is 26.2.7, no rejection was
logged, and one half of the transaction is visible. Issue 10669 (2023, closed) was a non-atomic
consume-transform-produce loop in Redpanda's own chaos tests, fixed then. Jepsen's 3616 lost
transactional writes after a lost and regained leadership in 21.11.2, fixed in 21.11.15. None
of the three describes a half-visible commit on a current version, or this cause.

With the cause known, five more searches of the issues and pull requests found no report of it:

- `producer fenced transaction`
- `epoch bump`
- `InitProducerId`
- `bump epoch abort`
- `concurrent producers same transactional id`

Issue 3278 ("Support KIP-360") added the epoch bump of a producer that retries after an error.

## Finding 4 (new candidate): after a change of leader, a produce answer names an earlier transaction's offset

Reported as redpanda-data/community#396 (2026-11-17). A Redpanda engineer answered on 2026-11-18
that a fix is in progress, and closed the issue.

**What happens.** A transactional producer writes a record and gets the offset of another
record as the answer. That record is the one that the same transactional id wrote at the same
sequence number in an earlier transaction. When the request with this answer was a retry, the
record is in the log at another offset. When the request was new, Redpanda does not write the
record at all: the producer's commit returns no error, and no reader reads the record. Both
cases need a change of the partition's leader during the transaction, and nothing else.

**The schedule.** The KVM engine ran the example on GitHub's runners, in run 44046143553 of a
one-off workflow, with the image built from `main` at `7944ad08`. There `disk=none | run
A=rp.txn_add@20:kill-n3` broke two claims the same way on three hosts: two AMD EPYC 9V74 and one
Intel Xeon 6975P-C. The run on the first AMD host
gave `fp=ff1ea63a6c2f966f`:

```text
VIOLATION [rp.duplicates] m28b is stored twice: it carries the system ids 5 and 15
```

The claim `rp.order` reported "m11a and m28b share offset 5 of key p1 (inconsistent offsets)".
The client's history, shortened:

```text
HYDO-HIST rp 11 ok 61830000000 send t m11a;k=p1;o=5;s=ok m11b;k=p2;o=11;s=ok
HYDO-HIST rp 28 invoke 80130000000 send t m28a;k=p2 m28b;k=p1
HYDO-HIST rp 28 ok 85950000000 send t m28a;k=p2;o=28;s=ok m28b;k=p1;o=5;s=ok
HYDO-HIST rp 36 ok 88450000000 recv t g ... m28b;k=p1;o=15 ...
```

Transactions 11 and 28 both used the transactional id `tx-3`. The reaction killed n3 just
before transaction 28 added its partitions, and transaction 28 took 5.8 seconds. Every reader
read `m28b` at offset 15, and offset 5 holds `m11a`. On the first host the other seven reaction
variants of that workflow passed (each node, at 0, 3 and 8 ms). hvx did not run this schedule.

**Where the client's offset comes from.** The client records the offset of franz-go's produce
promise (`txn` in `client/main.go`). franz-go 1.22.2 takes it from the answer that ends the
batch: `handleReqRespBatch` in `pkg/kgo/sink.go` passes the ProduceResponse's `BaseOffset` to
`finishBatch`, which gives the batch's records `baseOffset + i`. A retried batch gets the offset
of the answer to the retry, and franz-go keeps no offset from an earlier answer. The error
`DUPLICATE_SEQUENCE_NUMBER` is treated as a success with the answer's base offset, but
Redpanda's answer here has no error. So the offset 5 is Redpanda's answer, not franz-go's.

**The cause, in the code** (v26.2.2, and the same two files on `dev` at `d9944871`). Each
partition keeps, for each producer id, its 5 newest finished requests (`requests` in
`src/v/cluster/producer_state.cc`). `requests::try_emplace` looks a new request up by its first
and last sequence number only, without the epoch, and answers with the first match's offset. A
transactional producer starts its sequence numbers at 0 on each partition after each
InitProducerId, so the transactions of one transactional id use the same sequence ranges. Two
places reset the cache:

- `do_transactional_replicate` in `src/v/cluster/rm_stm.cc` resets it for the first batch of a
  transaction, while the partition's transaction state is `initialized`
  (`reset_sequence_tracking`). Only the leader runs this function.
- `producer_state::reset_with_new_epoch` resets it for a new epoch. `producer_state::apply_data`
  calls it only for batches that are not transactional, and `apply_transaction_begin` sets a
  transaction's new epoch without a reset.

A follower only applies batches, so it keeps the entries of the earlier epochs next to the new
ones. When it becomes the leader, it has already applied the transaction's first batch, the
state is `ongoing`, and nothing resets the cache. A request whose sequence range an earlier
epoch used then matches the earlier entry:

- A retry of a batch that is already in the log gets the earlier batch's offset. The record stays
  at its own offset. That is the KVM run: the kill of n3 lost the answer to franz-go's first
  try, and franz-go sent the batch again to the new leader.
- A new request gets the earlier batch's offset and is not written. The producer counts the
  record as written, and the commit succeeds without it.

Apache Kafka's `ProducerStateEntry` clears its batch metadata when the epoch changes
(`maybeUpdateProducerEpoch`), on the leader and on the followers, and `findDuplicateBatch` returns
no match for a batch of another epoch.

**Without hydo.** `native/stale-offset.sh` starts the cluster of `docker-compose.yaml`, builds
`native/staleoffset` (Go, with franz-go 1.22.2) and runs it in the client container. Each round
uses a new transactional id, moves the partition's leader with the HTTP endpoint
`transfer_leadership` of each node's port 9644, and waits until the metadata names the new leader:

- `retry`: transaction 1 writes `A0` and commits. Transaction 2 writes `B0`, the leader moves,
  and the same Produce request of `B0` goes again to the new leader, as a client sends it after a
  lost answer. Then transaction 2 commits.
- `loss`: transaction 1 writes `A0` and `A1`, one request each, and commits. Transaction 2
  writes `B0`, the leader moves, and transaction 2 writes `B1` and commits. No request is sent
  twice.

With `-transfer=false` the leader stays. On this Mac (Docker in Colima, arm64, v26.2.7), 20
rounds of each gave:

| mode | the leader moves | result |
|---|---|---|
| `retry` | yes | 20 of 20 answers to the second `B0` name `A0`'s offset; `B0` is read at its first offset |
| `retry` | no | 0 of 20 |
| `loss` | yes | 20 of 20 commits returned no error and lost `B1`: its promise names `A1`'s offset, and `B1` is not in the log, also with `read_uncommitted` |
| `loss` | no | 0 of 20 |

```text
0: pid 7: A0@40 (epoch 0); B0@43 (epoch 1) on n2; B0 sent again to n3: answer @40 <nil>; commit: <nil>
0: A0@1 A1@2; B0@5 on n3; B1@2 on n1; produce: <nil> <nil>; commit: <nil>
LOST l55-0-B1: given offset 2, which holds "l55-0-A1"; no read_committed reader reads l55-0-B1; in the log at all (read_uncommitted): false
```

The loss needs no fault and no retry, only one move of the leader between two produce requests
of a transaction. An earlier transaction's batch with the same sequence range must still be in
the new leader's cache. Redpanda's leader balancer moves leaders by itself (`enable_leader_balancer`, on
by default), and a node's restart moves them too.

**Is it a bug?** Yes. The page "Transactions" promises "either all messages are committed or
none", and in the `loss` mode a committed transaction lost one of its records. In the `retry`
mode, a produce answer names another record's offset; an idempotent producer's retry should get
its own batch's offset, as on the same leader without a move.

**What the check said.** The claim `duplicates` read the two offsets of `m28b` as two stored
copies. The readers saw one copy, at 15, and the acknowledgement named another record. The
claim's message now says so when every delivery carries one id and an acknowledgement names
another (`duplicates` in `hydo/crates/hydo-history/src/messages/claims.rs`). On the KVM run's
console it reads:

```text
VIOLATION [rp.duplicates] m28b was acknowledged with the system id 5, but each of its 2 deliveries
carries 15: the system acknowledged another id than the copy it stored, or it stored a second copy
that no reader read
```

The example's client writes one record per partition in each transaction, so it can show the
wrong offset but not the lost record.

**Duplicate search** (GitHub issues and pull requests of redpanda-data/redpanda and twmb/franz-go,
every state, on 2026-11-17, and Jepsen's list). The terms for Redpanda: "reset_sequence_tracking",
"returns old offset", "lost updates", "dedup", "producer epoch sequence reset", "transactional"
with "leadership transfer", "wrong offset", "transactional" with "lost", "producer_state",
"sequence" with "follower", "idempotent duplicate", "sequence number transactional", and
"transaction" with "missing record". The terms for franz-go: "redpanda offset", "duplicate
sequence", "record offset wrong", and "transactional offset". No result describes this case. The
closest is pull request 5026 ("Fix lost update consistency error", merged on 2022-06-15, for
issue 5438, "Lost updates in chaos test runs"). Its description names the same answer: a client
starts a new transaction, "reuses the same seq_id", and "Redpanda dedupes seq_id=42 using the
original record and returns old offset". Its fix is the leader's reset in `do_transactional_replicate`, so a follower that
becomes the leader later keeps the old entries. Jepsen's 3039 (no deduplication) and 3616 (lost
transactional writes after a lost and regained leadership, fixed in 21.11.15) have other causes.

## Not a bug: records deleted by retention after a clock step of a year

Seed 25 of the fourth exploration (`base8`) ended `VIOLATION [rp.fan-out] m0 was acknowledged
... and never delivered`: the full read of partition 2 started at offset 74. Its schedule stepped
the guest's wall clock forward by a year before step 117 (`T=117:31536000`). Redpanda deletes a
partition's old segments after `log_retention_ms`, 7 days by default, measured on the wall
clock, so the step deleted every record of the first segments before the drain read them. Group
`g` had read `m0` before the step. That is retention doing what it says. A topic created with
`retention.ms=-1` would keep the check strict under such steps; this example's topic keeps the
default.

## Known bugs reached

None of Jepsen's 2022 issues came back. No aborted record was read with `read_committed`. The
lost records of findings 3 and 4 have other causes than Jepsen's 3616, and finding 4 is not
Jepsen's 3039.

## The blind exploration

`hydo explore --strategy process --more-faults --new-faults --storage-faults --cluster-faults
--workers 1` drew from every fault kind of `faults:`, on one worker: a guest of 4,608 MiB leaves
no room for a second one on the shared machine. A schedule took 3 to 14 minutes, most of it in
recovery boots after power cuts and in drains that waited for a stuck partition.

| exploration | image | seeds | schedules | failures |
|---|---|---|---|---|
| 1 | `base` | 1 to 8 | 8 | seed 6: `ERROR n1 restart failed` (finding 1) |
| 2 | `base` | 9 and 10 (stopped during 11) | 2 | seed 9: `ERROR` of the drain (finding 2) |
| 3 | `base3` | 11 to 24 (stopped during 25) | 14 | seed 24: `VIOLATION [rp.no-loss]` (finding 3) |
| 4 | `base8` | 25 to 37 (stopped by its time limit) | 13 | seed 25: `VIOLATION [rp.fan-out]` (retention after a clock step, not a bug); seed 35: `ERROR n1 restart failed` (finding 1, the coordinator's snapshot); seed 37: `VIOLATION [tx.no_hanging]` (finding 2) |

That is 37 blind schedules, fewer than the 60 to 100 planned: on one worker the machine gave 4
to 20 schedules an hour, and the work on each failure took the machine as long as the
exploration that found it. The explorer drew a storage fault (`G=` or `L=`) in 28 of the 37
schedules, a power cut in 13, and a clock step in 28. Of the 37, six failed: two each of
findings 1 and 2's kinds, one of finding 3, and one by design.

## Onboarding retro

The work followed the onboarding skill and `hydo guide`, with `hydo guide messages` for the
check and the drain, on this branch's hydo and hvx. The clock times are from the start of the
work (18:53 UTC).

**Time to the first clean run: 35 minutes.** Scouting without VMs took 20 minutes: the
promises in Redpanda's documentation, Jepsen's list, the image, and Redpanda's own start
options. The cluster and the client ran first under Docker on the Mac. The first hvx run ended
`ERROR service start (n1)` (the cgroup mount), and the second gave OK.

**Schedules before the first finding: 6.** Seed 6 of the first blind exploration gave finding 1,
seed 9 gave finding 2, and seed 24 (the third exploration) gave finding 3.

**The findings.** Three new candidates. A damaged `tx.snapshot` stops its node at every start
(finding 1), and a transaction that the coordinator still records as `empty` is never aborted
(finding 2); both causes are in the code of `dev`. A committed transaction is visible on one of
its partitions only (finding 3). An InitProducerId with the same transactional id aborts the
open transaction and does not fence the old producer, whose commit then covers the rest. It
reproduces without hydo on every try. A KVM run after the onboarding gave a fourth (finding
4): after a change of leader, a produce answer names an earlier transaction's offset, and a new
request can get such an answer without being written.
The durability promises held under strict power losses of one node and whole-guest power cuts,
whether write caching was on or off.

**What took the most time.**

- Finding 2's evidence: about two hours. Its run ended with an ERROR from the drain, whose output
  stays in the guest. A fresh boot of the same schedule passed, so `hydo inspect` on a `hydo
  run` showed nothing. `hydo explore --schedule` on the image of the exploration reproduced it,
  `hydo repro --transcript` made a run directory of it, and two replays read the nodes' logs.
- The speed of the runs: a schedule took 3 to 14 minutes on one worker, so an hour gave 4 to 20
  schedules. Three Seastar nodes need a guest of 4,608 MiB, and the shared machine had room for
  one.
- Leaving out one fault at a time for finding 2: every run took 12 minutes, and each changed the
  timing so much that it passed.
- Finding 3's cause: the default logs said nothing, so the client gained a raw fetch (`dump`),
  and a reaction on a console event of the client (`A=rp.txn_add...`) found a one-fault
  schedule. Each new client meant a new image, and the earlier schedules moved with the timing.
  An image with the `tx` logger at trace level lost the regression's schedule, but five of the
  nine reaction variants still broke the claim, and one trace showed the cause.

**What hydo lacked.**

1. Seastar programs need cgroup2 mounted once in the guest, or they stop with `sstring out of
   range`. `hydo guide hazards` does not say so; the example mounted it in `setup:`. Milestone
   283 made hydo mount it at every boot, from the fingerprint epoch 4.
2. The output of the messages check's `final:` read-back stays in the guest, also when it fails.
   The client writes its reason to the console.
3. hydo shrinks only violations. A node that does not start again and a drain that cannot finish
   are ERRORs, and were shrunk by hand.
4. A failure from `hydo explore` did not come back under `hydo run`, because a fresh boot is
   timed differently from the explorer's run. `hydo explore --schedule` and `hydo repro
   --transcript` replay it; `hydo report` does not point to them for an ERROR.
5. `G=` picks the file and the place from its seed and the files of the moment, so the same token
   damaged other files on another image. A byte, a batch or a single file cannot be named.
6. A regression whose verdict is an ERROR can be pinned only by its fingerprint, and the
   fingerprint depends on the regression files the image carries (open item 132). Its `expect`
   line does not change the fingerprint: two images that differed only there gave the same one.
7. A client's build is part of the image, so every new command of the client (the drain's
   reason, `lso`, the reaction event, `dump`) moved the timing of every schedule: seed 9's hang
   came back only on the image it was found on. A way to add a diagnostic program to a replay
   without changing the run would have saved two images.
8. `hydo rules scan --kind clock` reported no place in Redpanda's 2,694 C++ files. The
   transaction coordinator times transaction expiry with Seastar's `lowres_system_clock`, a
   wall clock, which the audit does not know.

**What the playbook should say.**

- For a Seastar program (Redpanda, ScyllaDB): run it with `--smp 1 --memory <size>
  --overprovisioned`. Check `/proc/self/cgroup` in the guest first. On an image built before the
  fingerprint epoch 4, mount cgroup2 in `setup:`; hydo mounts it from epoch 4 on.
- For a log with partitions: one messages channel, the partition as the key, its offset as `o=`,
  and `order: {kind: log, scope: key}`. Record an aborted transaction's records as `fail`, so
  that `no-phantom` catches an aborted read.
- For transactions: add an `eventually` invariant that no transaction stays open after the heal,
  so that a hang is a violation that the explorer shrinks, not a drain's ERROR.
- Write the drain's reason for giving up to the console.
- Replay an explorer failure with `hydo explore --schedule` on the same image before anything
  else, and read logs with `hydo repro --transcript` and `hydo inspect`.
- When a messages check reports a lost record of a transaction, dump the partitions raw (each
  batch's producer, epoch and control marker) at the run's end with `hydo inspect exec`: the
  markers tell a broker's decision from a client's filter.
- Give the client a console event before each operation that matters (`HYDO-ASSERT`), so that
  reactions (`A=`) can put a fault inside it.
- Leave out development modes that relax durability: `rpk`'s `--mode dev-container` bypasses
  fsync and turns on write caching.

