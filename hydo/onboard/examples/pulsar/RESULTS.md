# Apache Pulsar under hydo: three ZooKeeper servers, three bookies, two brokers

Apache Pulsar is a distributed message broker written in Java. Its brokers keep no data of
their own: each partition of a topic is a managed ledger, a list of Apache BookKeeper ledgers
that the broker writes to several bookies, and the metadata (ledgers, cursors, ownership) lives
in ZooKeeper. This example runs production's shape, scaled down only in memory, in one guest of
the hvx engine, and looks for bugs that nobody reported.

## The short answers

- **What was found?** One known bug of the official Go client, one new bug and one new clock
  effect of the transaction coordinator. Nothing was posted.
  1. **Known (pulsar-client-go#1498, open pull request 1499): with pulsar-client-go v0.21.0,
     every message of an aborted transaction is delivered.** The client writes the
     transaction's id into the send command and not into the message's metadata, and the broker
     filters aborted messages by the metadata. It broke the first run without faults
     (`regressions/aborted-transaction-messages-stay-hidden.schedule`, with `--set
     client=release`). The rest of the example runs the client with pull request 1499.
  2. **New, a clock effect: after a backward step of the wall clock, a transaction that its
     client left open stays open for the step's length, and its partitions deliver nothing
     sent after it.** The coordinator times a transaction out on the wall clock. With one
     transaction left open by a crashed client and a step back of 600 s inside its timeout of
     10 s, 18 acknowledged messages were not delivered by the drain, and a committed
     transaction showed one of its two messages
     (`regressions/transaction-left-open-after-a-backward-clock-step.schedule`).
  3. **New: after the brokers restart, a transaction that its client left open is not aborted
     until some transactional client connects to its coordinator, and until then its partitions
     deliver nothing sent after it.** A coordinator and its timeouts start only when a client
     connects to it. After a power cut of the whole guest, the recovery boot ran no
     transactional client, so a transaction left open before the cut stayed open, and an
     acknowledged message, also of a committed transaction, was never delivered. Found blind
     three times; `disk=cut | run C=35` shows it on the default composition
     (`regressions/transaction-left-open-after-a-restart.schedule`). A transactional client
     aborted it at once.
- **What held?** No acknowledged publish was lost, deduplication stored each message once, and
  the failover subscription kept each key's order, through kills, pauses past ZooKeeper's
  session timeout, partitions, one node's power loss or damaged files, power cuts of the whole
  guest at steps, at disk flushes and 0 to 40 ms after an acknowledgement, and clock steps but
  those of finding 2. That was 84 blind schedules in six explorations and about 50 directed runs.
- **By design.** A consumer's acknowledgement with the broker's receipt can come back after a
  broker crashes: the broker writes a subscription's position at most once a second and keeps
  the rest in memory, and the client's documentation says so. The claim `after_ack:
  no-redelivery` broke at the first power cut; the check claims `after_ack: allowed`.
- **What hydo lacked?** Six gaps, in the section "What hydo lacked". The largest: hvx keeps
  64 KiB of the guest's console per boot, so the first workload's records were cut off, and the
  claims were undecided without a warning. Also, `kill-workload/<name>` found no process of the
  composer's commands.

## The version

- Apache Pulsar 5.0.1 (2026-10-26, the newest release), with BookKeeper 4.18.1 and ZooKeeper
  3.9.6 inside, from the official image `apachepulsar/pulsar:5.0.1`: index
  `sha256:4507075d8c11b34629f3a0b41f48e3c659951e601538562ee0c088cd2c737ec0`, linux/amd64
  `sha256:fb95bc2d73687ef7df3a21f3424a897860926e25303bd3857898ab887ead85c3`. The image runs JDK 25
  on Alpine.
- The test client is built on pulsar-client-go. `client/go.mod` pins v0.21.0, the newest
  release; `client/go.fix.mod` replaces it with the head of pull request 1499
  (`jiangmocc/pulsar-client-go` at `a4d7bf4b`, 2 commits on top of a commit 14 commits before
  v0.21.0). The build parameter `client` chooses one (`fix` by default).
- The sources read for the causes are the tags `v5.0.1` of apache/pulsar and `release-4.18.1`
  of apache/bookkeeper.
- hydo: this tree's `hydo`, `hydo-build`, `hydo-fetch-image` and `hydo-hvx` (0.5.0 with
  Milestones 340 to 354, the fingerprint epoch 6), in a copy of the 0.5.0 Linux release's guest
  files, on the bare-metal machine.

## The promises

Pulsar's documentation (concepts-messaging, concepts-transactions, the configuration reference)
and code state these; no owner of Pulsar confirmed them for this example, so each is
"unconfirmed by the owner".

| promise | where it is stated | judged by |
|---|---|---|
| a publish that the broker acknowledged is stored and reaches every subscription, while an ack quorum of bookies survives | concepts-architecture (ack quorum), concepts-messaging ("Acknowledgment") | `pulsar.no-loss`, `pulsar.fan-out` |
| with deduplication on, a message is stored once, also when its producer sends it again with the same sequence id after a crash | concepts-messaging ("Message deduplication") | `pulsar.duplicates` |
| a failover subscription delivers each key's messages in the order they were stored | concepts-messaging ("Failover"; the documentation warns of duplicates and reordering at a switch of the active consumer) | `order.order`, first deliveries only |
| a transaction's messages and acknowledgements are all or nothing; a transaction that its client leaves open is aborted after its timeout | concepts-transactions | `checks/final_txn`, `pulsar.no-phantom`, `pulsar.no-loss` |
| an acknowledged message is not delivered again | not a promise: the client's `isAckReceiptEnabled` documentation says that a receipt does not keep it from coming again | `after_ack: allowed` (section "By design") |

## The project

`docker-compose.yaml` runs the cluster as Pulsar's own multi-node Compose file
(`docker-compose/kitchen-sink/docker-compose.yml` of the Pulsar repository) does: three
ZooKeeper servers, three bookies, two brokers and a one-shot job that writes the cluster's
metadata, without the proxy, the functions worker and websockets. `hydo compose` imported it
into `hydo/hydo.yaml`. The changes from the import:

- Each ZooKeeper server's data directory and each bookie's journal and ledger directories are on
  a file system of their own (`storage:`), the target of `L=` and `G=`.
- The import had no readiness check (the image has no `EXPOSE` and no healthcheck). zk1 is ready
  at its port; zk2 and zk3 start with it and are ready when `srvr` names a mode, that is with a
  quorum; a bookie is ready at its port; a broker when `/admin/v2/brokers/ready` answers.
- Only bookie1 runs the job (`conf/init.sh`, `bin/pulsar initialize-cluster-metadata`) in its
  setup; the command keeps what exists.
- The configuration is the image's own files with the changed keys set (`conf/pconf.sh`, the keys
  in `conf/zookeeper.conf`, `conf/bookkeeper.conf` and `conf/broker.conf`): ensemble 3, write
  quorum 3, ack quorum 2, `journalSyncData=true`, deduplication and transactions on, and ledgers
  that roll over after 100 entries, so that a run of a minute rolls ledgers and cursor ledgers
  over as a long-lived topic does.
- Each JVM has a small heap, the serial collector and the first JIT tier only: ZooKeeper 96 MiB,
  a bookie 192 MiB and 160 MiB of direct memory, a broker 320 MiB and 192 MiB. The guest has
  5,120 MiB; the hvx engine held about 10 GB of the host for it.
- The durability settings are build parameters for `hydo explore --configs`: `ackquorum` (2, 1,
  3) and `jsync` (true, false).

## The workload

The client `pulsarc` (`client/main.go`) writes the records of `hydo guide messages`. Every
choice comes from the seed. The topic `t` has 3 partitions and the subscriptions `s` (shared),
`f` (failover) and `a` (read only by the transaction check), made by `provision:` before the
window under test.

| command | what it does | records |
|---|---|---|
| `parallel_driver_produce` | two messages with keys from the seed, from the producer `p<id>` with the sequence ids 1 and 2; a send that fails is sent once more from a new producer of the same name with the same sequence id | one send of both: ok with ledger and entry, info without an answer |
| `serial_driver_resend` | the producer `r`, one process at a time: first the message whose send got no answer in the command before, with its sequence id, then a new one | as above |
| `parallel_driver_consume` | a consumer of `s`: up to 5..24 messages, each acknowledged with the broker's receipt | one receive and one acknowledgement per batch |
| `serial_driver_failover` | a consumer of `f`, one process at a time | the same, also in the history `o` |
| `parallel_driver_txn` | a transaction with a timeout of 10 s: two messages and the acknowledgement of one message of `s`, then a commit (3 in 4) or an abort; in 1 of 8 the client exits before either, as a crashed client | the sends ok after the commit, fail after the abort, info otherwise; the outcome in `$HYDO_OUTCOMES` |
| `anytime_unload` | the admin API unloads one partition (`hydo rules scan --kind writes` lists the route): its broker hands it over | none |

The drain (`pulsarc drain`, the `final:` of the check `pulsar`) reads `s` and then `f` until 12 s
pass without a message or an error, acknowledges what it reads, and writes `drained`. It gives up
after 230 s.

## The rules

- `pulsar` (`type: messages`, history `p`): `delivery: at-least-once`, `duplicates: forbidden`
  (one stored copy, by ledger and entry), `fan_out: every-group` (`s` and `f`), `after_ack:
  allowed`.
- `order` (history `o`): `order: {kind: fifo, scope: key}` on `f`, whose consumers run one at a
  time. A shared subscription with prefetching consumers keeps no such order, so `s` has no
  records in `o`.
- `checks/final_txn`: reads `a` from the start until it is empty and judges every transaction of
  `$HYDO_OUTCOMES`: a committed one has both of its messages, an aborted one neither, one
  without a known outcome both or neither.
- `checks/eventually_no_open_transaction` (180 s after the heal): no partition's transaction
  buffer has an open transaction (`/admin/v3/transactions/transactionBufferStats`), after the
  check loads every partition with a producer that sends nothing. It judges how each
  transaction that the client left open ends, also when no acknowledged message is behind it.

## Without faults, and the regressions

`disk=none | run` passes twice with `fp=811b19bbb54fcf71` on the final image (disk `16f3c0f9…`, about 40 KiB
of console). Of the 78 commands some fail as they should: the transactions that the client
leaves open, and producers and consumers that a partition being unloaded refused with
`ServiceNotReady: Topic is temporarily unavailable` (`TopicFencedException`).

| file | expect | fingerprint |
|---|---|---|
| `transaction-left-open-after-a-backward-clock-step.schedule` | violation | `bcfa5492766b3941` |
| `transaction-left-open-after-a-restart.schedule` | violation | `c26c630a0f1df04e` twice |
| `aborted-transaction-messages-stay-hidden.schedule` | ok; with `--set client=release` a violation | `811b19bbb54fcf71`; `8cf005d04d8a1961` with the release client |

## What was found

### 1. Known: with pulsar-client-go v0.21.0, an aborted transaction's messages are delivered

The first run without faults failed `final_txn`: `x35 aborted, delivered a=true b=true` and
`x64 aborted, delivered a=true b=true`. The client had sent both messages, acknowledged one
message of `s`, and aborted with success 50 ms later; 3 s later the consumers of `f` and `s`
received both messages. Every aborted transaction of the run was delivered. On the final image
built with `--set client=release` (`dc37a55a…`), the same run gives `VIOLATION [txn] 3 of 16
transactions not all or nothing: 0 committed, 3 aborted, 0 unknown` (`fp=8cf005d04d8a1961`):
all three aborted transactions were delivered.

The cause, in the client:

1. `pulsar/internal/batch_builder.go` lines 233 to 236 (`batchContainer.Add`) and
   `pulsar/internal/commands.go` lines 357 to 360 put the transaction's id into `CommandSend`
   only. The message's `MessageMetadata` has no `txnid_most_bits` or `txnid_least_bits`.
2. The broker stores the message in the transaction buffer by the command's id
   (`ServerCnx.java` line 3100), so it holds it back until the transaction ends.
3. The dispatcher drops an aborted message by the id in its metadata
   (`AbstractBaseDispatcher.java` lines 188 to 209, `isTxnAborted`). Without one, the message
   counts as a plain message and is delivered once the transaction ends, committed or not. The
   Java client writes the id into the metadata (`TypedMessageBuilderImpl.java` line 108).

pulsar-client-go#1498 ("Transactional publishes do not set txn id in MessageMetadata", open,
2026-05) names this cause, and pull request 1499 (approved, not merged) fixes it. With the client
of pull request 1499 (`--set client=fix`, the default), every aborted transaction stayed hidden in
every run. With `--set client=release` the same run fails on every run. No native run was made:
the cause is plain in the code and known.

### 2. New, a clock effect: a transaction left open stays open for the length of a step back

Schedule `disk=none | run T=32:-600`. On the image before `eventually_no_open_transaction`
(`fp=778588dfaff8d4f3` twice):

```text
VIOLATION [txn] 1 of 16 transactions not all or nothing: 1 committed, 0 aborted, 0 unknown
  x69 committed, delivered a=true b=false
VIOLATION [pulsar.no-loss] m57.1 was acknowledged to process 57 at 138.290 s and never delivered in group f of channel t, which was drained
  sends: 54 ok, 8 failed, 2 unknown; lost 18
```

On the final image (`fp=bcfa5492766b3941`) the first broken property is the new check:

```text
VIOLATION [no_open_transaction] not satisfied 181.74s after heal, deadline 180s (last check rc=1: 2 of 3 partitions hold back their messages behind an open transaction)
```

Command 54's client left its transaction open at 136.9 s of uptime with a message on two
partitions, as a crashed client does. The clock went back by 600 s before step 32, inside the
transaction's timeout of 10 s. At the run's end, after the drain, the coordinator still listed
the transaction as `OPEN` (`/admin/v3/transactions/slowTransactions`: `openTimestamp
1795564936369`, `timeoutAt 10000`, the wall clock 600 s before that), and the two partitions had
a backlog of 17 and 4 messages in each subscription that no consumer received.

The cause:

1. `TransactionTimeoutTrackerImpl` (pulsar-broker, `transaction/timeout`) reads
   `Clock.systemUTC()` (line 55). `addTransaction` stores the deadline as the wall clock plus the
   timeout (lines 65 and 66), and `run` aborts a transaction only when its deadline is earlier
   than the wall clock (lines 112 to 119); otherwise it waits again for the difference. After a
   step back of 600 s the difference is 600 s.
2. A transaction recovered after a coordinator's move keeps its stored deadline
   (`replayAddTransaction`, line 85), so a broker's crash does not end the wait.
3. Each partition's transaction buffer holds back every message after the first open
   transaction (`TopicTransactionBuffer.removeTxnAndUpdateMaxReadPosition`, lines 656 to 664,
   and the dispatchers read up to `topic.getMaxReadPosition()`). So one abandoned transaction
   stops every subscription of the partition, also for messages without a transaction.

Without the step the same composition passes. A step before the transaction began, or one after
its timeout, breaks nothing (`T=12:-600` and `T=28:-600` passed; command 54 began after both).
Transactions that the client ends are not affected. A forward step aborts open transactions
early, which their clients see as a failed commit.

The judgment: a bug of a duration on the wall clock. Pulsar's transaction timeout is what
bounds how long a crashed client blocks a partition's consumers. After a step back the bound
becomes the step's length, and every consumer of the partition waits, whatever the type of its subscription. This is the clock-effect pattern
of Valkey and Temporal: report it as a bug of a duration measured on the wall clock. The
duplicate searches found no issue (section "The duplicate searches"). The cause is plain in the
code, so no native run was made.

### 3. New: after a restart, an open transaction waits for a transactional client

On the final image `disk=cut | run C=35` fails twice the same way (`fp=c26c630a0f1df04e`, once
from a fresh checkout of `main` at `3bac22f8` with hydo built there):

```text
VIOLATION [no_open_transaction] not satisfied 184.99s after heal, deadline 180s (last check rc=1: 2 of 3 partitions hold back their messages behind an open transaction)
  t-partition-0: 1 open transactions, reads end at 67:10
  t-partition-2: 1 open transactions, reads end at 82:0
VIOLATION [pulsar.no-loss] m57.1 was acknowledged to process 57 at 138.980 s and never delivered in group f of channel t, which was drained
VIOLATION [txn] 1 of 14 transactions not all or nothing: 1 committed, 0 aborted, 0 unknown
  x59 committed, delivered a=false b=false
```

There command 54's client left its transaction open at 136.96 s, with `m54.a` at `84:0`
(partition 2) and `m54.b` at `67:11` (partition 0). The cut came before step 35, about 5 s
later and inside the transaction's timeout. After the recovery boot the plain message `m57.1`
(`67:13`, acknowledged at 138.98 s) was never delivered. Neither were both messages of the
committed transaction `x59` (`84:3` and `84:4`, its commit acknowledged at 141.51 s). Each stands
behind the open transaction's entry on its partition. The cuts before steps 30 and 32 left it
open too (`fp=64f7f37dc52cfe4c` and `60c4ae0fa4861169`).

In a `hydo inspect` session after that run's checks, 211 s after the recovery boot started:
- `coordinatorStats` answered 500 on both brokers with `Transaction coordinator not found!
  coordinator id : 0`, and `transactionMetadata/1/11` answered 404 with `Transaction
  coordinator with id 1 not found!`;
- the open transaction is `(1,11)`: the entry `67:11` has `X-Pulsar-txnid-most-bits: 1`,
  `X-Pulsar-txnid-least-bits: 11` and `X-Pulsar-txn-uncommitted: true`, and
  `transactionInBufferStats` of partitions 0 and 2 gave `{"startPosition":"67:11","aborted":false}`
  and `{"startPosition":"84:0","aborted":false}`;
- each partition's transaction buffer was `Ready`, with `ongoingTxnSize` 1 and
  `maxReadPosition` `67:10` and `82:0`; its recovery had ended 107 s before the coordinators'
  recovery below;
- the cursors of `s`, `f` and `a` read at `67:11` and `84:0`, and the last entries were `67:18`
  and `84:5`;
- `/bin/pulsarc provision`, which begins and aborts one transaction, took 1.5 s. Then
  `coordinatorStats` answered 200, with every coordinator `Ready` and recovered in that second.
  `transactionMetadata/1/11` said `not found`, and `transactionInBufferStats` said
  `{"aborted":true}`. `ongoingTxnSize` was 0 on both partitions, and a drain read `s` and `f` of
  partition 0 to its last entry.

A second session gave the same answers.

The cut before step 34, the regression of M356, fails otherwise (`fp=0aa2f73c75dcea55` twice):
`no_open_transaction` ends with `rc=2: no partition's transaction buffer answered`, and the
drain does not finish. In that recovery boot bookie2's AutoRecovery stopped 23 s after its start.
Its auditor's bookie check failed with `KeeperException$BadVersionException` for
`/ledgers/underreplication/ledgers/0000/0000/0000/0027/urL0000000039`, so
`AutoRecoveryDeathWatcher` ended the whole bookie with exit status 0. The example's `restart:
on-failure` does not restart a process that exits 0. With two bookies the brokers cannot create
a ledger of ensemble 3 (`Not enough non-faulty bookies available`). So no partition loaded:
each partition's transaction buffer answered 404, and every consumer was refused. Why the
auditor met another version of the znode was not traced. On the image before the 180 s deadline (`3ff0fec1…`, deadline 90 s),
the same schedule gave `VIOLATION [no_open_transaction] not satisfied 94.15s after heal ...
(last check rc=1: 1 of 3 partitions hold back their messages behind an open transaction)`
(`fp=3408de439ffd0c0d`). On the image before the check, the blind seed 52
shrunk by hand to `disk=cut dseed=0 | run C=16 W=adbbc231,p4,q1,r500`
(`fp=9d45c70a4a5691f1`): `VIOLATION [txn] 1 of 3 transactions not all or nothing: 1 committed,
0 aborted, 0 unknown`.

e3's seed 52 drew it blind (`disk=cut | run N=zks.ring@34-37:delay,500ms C=16
P=zks.majority.3@28+2`; the faults after the cut at step 16 never came), and seed 51 and e2's
problem 4 (`cf=877`, with and without its clock step) failed the same way. In e2's problem 4
without the clock step (`disk=cut dseed=0 cf=877 | run O=bookie2-*@4-17:resp:delay,1090ms
W=d6d260d0,p3,q1,r1000 L=zk2@3+2`, `fp=2134c420721ba904`), command 16's client left its
transaction open at 144.5 s with a message on two partitions, and command 18 committed a
transaction with one message on one of them at 145.6 s. The power cut came a few seconds later.
After the recovery boot and the drain, both partitions' subscriptions stopped at the open
transaction's first entry (`internalStats`: readPosition `40:3` and `8:4`, the open
transaction's entries; `lastConfirmedEntry` `40:6`), so `m18.a` (`40:4`) of the committed
transaction was never delivered while `m18.b` was. `/admin/v3/transactions/coordinatorStats`
answered 500 on both brokers: no coordinator was loaded. When a transactional client began and
aborted one transaction in that state (`pulsarc provision`, through `hydo inspect`), the
coordinators loaded, the open transaction was aborted, and `slowTransactions` was empty.

The cause:

1. A broker opens a transaction coordinator's store, replays its log, and starts its timeout
   tracker only in `TransactionMetadataStoreService.handleTcClientConnect`
   (`TransactionMetadataStoreService.java` lines 109 to 145; `timeoutTracker.start()` at line
   144), which runs only for a client's `TcClientConnectRequest` (`ServerCnx.java` lines 3969 and
   4005). No other code opens a store: not the ownership of the coordinator's
   `transaction_coordinator_assign` partition, and not a partition's transaction buffer.
2. The recovery puts every open transaction into the timeout tracker
   (`TransactionRecoverTrackerImpl.appendOpenTransactionToTimeoutTracker`, line 119), so its
   timeout runs out only after a client connects.
3. Each partition's transaction buffer holds back every message after an open transaction's
   first entry (finding 2, step 3), for every subscription.

So after a restart of the brokers that own the coordinators, a transaction whose client is gone
blocks its partitions for every consumer until some transactional client of the cluster connects
to that coordinator. In the runs that client never came. Without the cut the next transaction
command reconnects to the coordinators, which is why kills of the brokers alone did not show it.
The check `eventually_no_open_transaction` asks each partition's transaction buffer for open
transactions after the heal; it judges the wait also when no acknowledged message is behind the
open transaction. With the cut before step 37 (after the
transaction's timeout) the check held 94.78 s after the heal on the image with the deadline of
90 s: a recovery boot loads every partition anew, so the deadline is 180 s now.

The judgment: a bug. The documentation promises that a transaction is aborted after its
timeout; after a restart that holds only while transactional clients keep connecting. A
consumer that uses no transaction waits for a producer that uses one. The duplicate searches found no issue. The
cause is plain in the code, so no native run was made.

### By design

- **An acknowledged message comes back after a broker's crash.** The first exploration's first
  schedule, with a power cut at step 40, broke `after_ack: no-redelivery` on an image that
  claimed it: `m19.a was delivered again to process 100002 in group f after process 55's
  acknowledgement of it completed`. On a copy of `hydo.yaml` with `after_ack: no-redelivery`
  (image `ba5d5f62…`), `disk=cut | run C=30` gives `VIOLATION [pulsar.after-ack] m0.1 was
  delivered again to process 100002 in group f of channel t after process 38's acknowledgement
  of it completed at 129.320 s` (`fp=42acd95fa46b424f`), with `no-loss` and `duplicates` held:
  the position of `f` that reached BookKeeper before the cut lacked even the first message's
  acknowledgement. The broker
  answers an acknowledgement before it writes the subscription's position:
  `ManagedCursorImpl.asyncDelete` completes the callback at once when the mark-delete rate limit
  (`managedLedgerDefaultMarkDeleteRateLimit`, 1 a second) refuses a write (lines 3016 to 3020),
  and the position reaches BookKeeper at the next flush (`managedLedgerCursorPositionFlushSeconds`,
  60 s). The Java client's `ConsumerBuilder.isAckReceiptEnabled` says: "Even when the broker
  returns a receipt, it doesn't guarantee that the message won't be redelivered later due to
  certain implementation details." The check claims `after_ack: allowed`.
- **A failover subscription's switch.** The documentation warns that a switch of the active
  consumer can duplicate or reorder messages. The failover consumers run one at a time, so the
  order claim judges first deliveries after each switch.

### The environment

- **A damaged file of a ZooKeeper server stops it at every start.** Six schedules of e2 damaged
  one file of zk1, zk2 or zk3 (`G=`: 4 KiB zeroed, one bit flipped, `version-2/acceptedEpoch`
  cut to 0 bytes) and ended `ERROR zk<n> restart failed (its files were corrupted)`. The other two
  servers kept the quorum, and no check broke before the restart. ZooKeeper does not repair its
  own files; this is detected damage, not a bug of Pulsar.
- **A bookie with 32 flipped bits in one file does not start again** (e2 seed 17). BookKeeper
  checks its files at start; the cluster runs on the two other bookies, which cannot form a new
  ensemble of 3, as the settings say.

## The duplicate searches

Searched on 2026-11-25 with `gh search issues --include-prs`, open and closed.

| repository | phrase | result |
|---|---|---|
| apache/pulsar-client-go | `abort transaction` | #1498 (open), #1499 (open pull request): finding 1 |
| apache/pulsar-client-go | `aborted messages` | #1186 (open): a consumer misses a committed message after an aborted one, another cause |
| apache/pulsar-client-go | `transaction abort`, `txn metadata` | #1002 (the transaction API), #1499 |
| apache/pulsar | `aborted transaction messages delivered go client`, `txnid message metadata go`, `transaction abort visible consumer` | nothing |
| apache/pulsar | `TransactionTimeoutTracker`, `TransactionTimeoutTrackerImpl` | #10179, #22020, #13116 and test changes: the tracker's recovery and return value, not its clock |
| apache/pulsar | `transaction timeout` | #20150, #27371, #19786, #15039, #19629, #13405, #25959 (the new coordinator's timeout 1000 times too long): none about the wall clock |
| apache/pulsar | `clock` | #25660 (open): `java.time.Clock` used for timestamps in some places only, about tests, not steps |
| apache/pulsar | `NTP`, `system clock`, `Clock.systemUTC` | #13567 (`deliverAfter` should use the broker's time), #4562, #21862: none about transactions |
| apache/pulsar | `transaction timeout clock`, `transaction timeout system clock`, `clock jump transaction`, `wall clock transaction timeout`, `clock backwards`, `time goes backward`, `system time changed`, `currentTimeMillis monotonic` | nothing |
| apache/pulsar | `maxReadPosition stuck`, `transaction buffer max read position`, `ongoing transaction block dispatch`, `open transaction blocks`, `hanging transaction`, `transaction stuck consumer`, `consumer cannot receive messages transaction`, `lowWaterMark transaction stuck` | nothing |
| apache/pulsar | `transaction not aborted after timeout`, `transaction never aborted`, `transaction timeout not work`, `abort timeout transaction`, `slowTransactions` | nothing |

## What held, and why

With ensemble 3, write quorum 3, ack quorum 2 and `journalSyncData=true`:

- **No acknowledged publish was lost, and every subscription received each once stored.** This
  held through kills, pauses past ZooKeeper's session timeout of 30 s, partitions and one-way
  loss between the brokers, the bookies and the ZooKeeper servers and of whole groups, one
  bookie's or one ZooKeeper server's power loss, damaged files of one bookie, power cuts of the
  whole guest at steps and at disk flushes (`cf=`), and 15 power cuts aimed 0 to 40 ms after an
  acknowledged send, an acknowledgement, or the end of a transaction (e4). The losses of the runs
  were finding 2's.
- **Deduplication stored every message once** (`pulsar.duplicates`), also when the producer `r`
  sent a message again after its process ended, and when the client sent again from a new
  producer of the same name after an error.
- **The failover subscription kept each key's order** of first deliveries (`order.order`) in
  every run, through broker kills and partition unloads.
- **With the client of pull request 1499, transactions were all or nothing**, through kills,
  pauses and power cuts, but for the clock steps of finding 2.
- **`journalSyncData=false`** lost nothing in three power cuts: at steps 20 and 30, and 5 ms
  after an acknowledged send (`A=pulsar.sent@10+5:cut`, `fp=854e91192d71d18b`). The journal
  acknowledges such an entry after its write and syncs at most once a second
  (`Journal.java` lines 1134 and 1180, `journalPageCacheFlushIntervalMSec` 1,000), so a loss
  needs a cut inside that second before the guest writes the page back; the runs did not show
  why the entry survived. The same interval is measured on the wall clock
  (`System.currentTimeMillis() - lastFlushTimeMs`), so after a step back it waits the step's
  length; that was not run.

## The explorations

All on hvx, one worker of 5,120 MiB, the engine process up to 18.7 GB of the host. A first run
took 2 minutes on average.

| exploration | options | seeds | schedules | failures by cause |
|---|---|---|---|---|
| e1 | `--strategy process --cluster-faults` | 1-5 | 5, and 27 shrink runs | 1, finding 2 (seed 4: `K=zk1@31+9 P=broker1@11+1 T=16:-2592000`); stopped during its shrink |
| e2 | `--strategy process --cluster-faults --storage-faults --more-faults --no-shrink --time-limit 4800` | 6-40 | 35 | 10: 6 a ZooKeeper server's damaged file, 1 a bookie's damaged file, 2 finding 2, 1 finding 3 (`cf=877`) |
| cf1 | `--configs --configs-mode one --storage-faults --no-shrink` | 1-6 | 6, the default combination only | 3: 2 finding 2, 1 a ZooKeeper server's damaged file; stopped, see "What hydo lacked" |
| e4 | `--reactive-at pulsar.sent@12,pulsar.txn_end@12,pulsar.acked@12 --react-actions cut --react-delays 0..40/10` | the grid | 15 | none |
| e3 | `--strategy swarm --new-faults --cluster-faults --no-shrink` | 46-56 | 11 | 2, finding 3 (seeds 51 and 52); stopped by the memory guard when another VM ran beside it |
| e5 | `--strategy process --cluster-faults --more-faults --no-shrink` | 76-87 | 12 | 3, all the first version of `eventually_no_open_transaction`, which read a 404 as a broker that does not answer; fixed |

e1 to e3 and cf1 ran on images before `eventually_no_open_transaction`, e5 on the image before
its fix. `hydo stats explore-e2` judged the curve flat over its last 10 runs: no new verdict or
property outcome after run 12 of 35. The directed runs are in the findings' sections.

## What hydo lacked

In the order of the time each cost.

1. **hvx keeps 64 KiB of the guest's console per boot, and nothing warned.** The first client
   wrote two records per operation into two histories, with empty receives. The run without
   faults wrote 78 KiB; `pulsar.no-loss` and `pulsar.fan-out` were undecided ("no group ... has a
   drained record") and the sends counted 46 unknown, because the records after 64 KiB were
   gone. `hydo guide hazards` names the limit, but the run printed no warning. The client now
   records one receive and one acknowledgement per batch, leaves out empty receives, and the
   order history carries keys only; a run writes about 40 KiB. About 40 minutes. Since Milestone
   368 a cut console warns in `hydo run`, `result.json` and `hydo report`, and a messages claim
   that the missing records break is an ERROR that names the cut.
2. **`hydo explore` refused a check whose claim is a build parameter.** `after_ack:
   "{{afterack}}"` passed `hydo build --check` and the build, and `hydo explore` refused the
   image (`checks[0]: claims.after_ack: one of allowed, no-redelivery, not '{{afterack}}'`,
   status 5): it reads the manifest without the build's parameters, as open item 140 says for
   `faults:`. The claim is now written as `allowed`, and the strict claim ran on a copy of the
   manifest. About 15 minutes.
3. **`kill-workload/<name>` found no process of the composer's commands.**
   `A=pulsar.txn_end@8:kill-workload/pulsarc` logged `KILLED nothing for workload/pulsarc ... no
   process of the workload matches; its processes: 1 java`: the reaction looks for the
   processes of `workload.run`, and a composed workload's commands are not among them. The
   client leaves 1 transaction in 8 open by itself instead. About 15 minutes.
4. **A process has one operation in flight, also when the history is the only reason.** The
   transaction's receive and acknowledgement were records of the sending process, so the checker
   counted the transaction's sends as unknown. They are records of a process of their own now.
   About 10 minutes. Since Milestone 368 a process token `<process>#<op>` gives each operation
   of one process its own completion.
5. **The clock steps of the explorations hide the other failures.** One transaction in eight is
   left open, so every drawn step back that comes within 10 s of one fails the run with finding
   2, and the explorations cannot leave clock steps out without a copy of the manifest (open item
   140). The shrinker spent 27 runs on e1's first failure, a step back of 30 days, before the
   exploration was stopped.
6. **`hydo rules scan --kind writes` sees no binary protocol.** It lists 256 routes of the admin
   API (226 data, 25 admin) and none of the client's own operations, which go over Pulsar's
   protobuf protocol without a service declaration: producing, consuming, acknowledging and
   transactions. The workload covers those and one admin write, the unload of a partition.

## Time

| phase | wall time |
|---|---|
| scouting (the Compose file, the configuration, the clock audit) | 30 minutes |
| setup to the first clean run, with the hydo build on the bare-metal machine | 40 minutes |
| the first finding (the first run without faults) and its cause | 20 minutes |
| the console records and the second finding's directed runs | 1 hour 20 minutes |
| explorations and directed runs, with finding 3 | 3 hours 30 minutes |
| write-up | 1 hour |
