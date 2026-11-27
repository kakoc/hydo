# Apache RocketMQ under hydo: a replica group in controller mode

Apache RocketMQ (github.com/apache/rocketmq) is a message broker in Java. This example runs
RocketMQ 5.5.0, the newest release with an official image, in controller mode: a NameServer with
the controller embedded, and one replica group `broker-a` of three brokers, in one guest of hvx
on the bare-metal machine. It uses the settings that RocketMQ's documentation names for data
safety: `allAckInSyncStateSet`, which acknowledges a message only after every replica of the
SyncStateSet has it, and `SYNC_FLUSH`. It looks for bugs that nobody reported.

## The short answers

FINDINGS

## The version

- RocketMQ 5.5.0 (`rocketmq-all-5.5.0`, released 2026-04-10). Its official image
  `apache/rocketmq:5.5.0` (published 2026-05-04) is the newest: 5.5.1 (2026-08-20) has no image.
  The image is
  `docker.io/apache/rocketmq@sha256:7e8f6c9dbd9df742ed26ba69c00d4ad69e2f86a56f3ca7782ff8144dd0798132`,
  linux/amd64 `sha256:a613f1341f3404b96900cf11e5c35f2d049c103504a1dd60242a04cebf5be467`, with
  Eclipse Temurin's Java 8u482.
- The client: rocketmq-client-go v2.1.2, the newest release of RocketMQ's Go client for the
  remoting protocol, with hydo's Go SDK v0.6.0 (`hydohist`), and a small remoting client of the
  example's own (`client/remoting.go`) for what the Go client does not offer.
- hydo of `main` at `bdb403f4` (0.7.0 and Milestones 368 to 374), with the guest files of the
  published 0.6.0 Linux release, on hvx.

## The promises

These are the promises under test, in the words of RocketMQ's documentation:

- Controller mode (`docs/en/controller/deploy.md`): with `allAckInSyncStateSet`, "a message
  needs to be replicated to every replica in SyncStateSet before it is returned to the client as
  successful, and this parameter can ensure that the message is not lost". The controller elects
  a master only from the SyncStateSet while `enableElectUncleanMaster` is `false` (the default):
  electing from outside "may select a replica with lagging data as Master and lose messages".
- `SYNC_FLUSH`: the master answers a send after the message is on its disk.
- Transactional messages: a half message is delivered only after its commit, never after a
  rollback, and the broker asks the producer group about a transaction whose producer sent
  neither.
- Ordered messages: the messages of one queue are delivered in the order they were stored.
- Delayed messages and retries: a message with a delay level is delivered after its delay; a
  message that a consumer gives back is delivered again from the retry topic, and after the
  maximum reconsume times it goes to the dead-letter topic.
- Consumer offsets: the broker keeps a group's committed offset per queue, and a consumer goes on
  from it. The promise that an offset holds across a failover is the task's, unconfirmed by the
  owner: the documentation promises at-least-once delivery.

## The project

`docker-compose.yaml` runs the NameServer `namesrv` with the controller embedded
(`config/namesrv.conf`, the documentation's "Embed NameServer deployment" with one controller)
and three brokers `broker0` to `broker2` of the replica group `broker-a` (`config/broker.conf`,
started by `config/start-broker.sh`). Every broker starts as a slave with `brokerId -1`, and the
controller chooses the master. Each node's store is in a volume of its own. The JVMs have small
heaps (128 MiB for the NameServer, 256 MiB for a broker), and the commit log's files are 64 MiB.
`hydo compose` imported it, and `hydo/hydo.yaml` changes the import in these ways:

- each service keeps the address of the Compose file: the importer moved every address by one
  (`ipv4_address 10.89.0.10 ignored; the service is at 10.89.0.11`), and the brokers'
  configuration names the NameServer's and the controller's address;
- each node's store (`/home/rocketmq/store`) is a file system of its own (`storage:`), the target
  of `L=` and `G=`, which `setup:` gives to the image's user (uid 3000), as `hydo build --check`
  advised;
- a broker's port is 10911 and its readiness check a connect to it; the importer took 9876 from
  the image's `EXPOSE` for each broker;
- the logs and the JVM's GC log go to `/dev/shm`, because the image's root is read-only in the
  guest;
- the brokers' settings `allAckInSyncStateSet`, `flushDiskType`, `minInSyncReplicas` and
  `transactionCheckInterval` are build parameters; the first two are `configs:` entries;
- the three brokers form the group `brokers`, with `primary: /bin/rmq leader` (the master that
  the NameServer names), `links: all` and `restart: together` (Milestone 374).

Deviations from RocketMQ's defaults, each for the test: `transactionCheckInterval` is 10 s
instead of 60 s and `transactionTimeOut` 6 s, so that a check comes within a run;
`fileReservedTime` is 100 years, so that a forward clock step deletes no file.

## The workload

The test client `rmq` (`client/`, `build.sh`) waits in `provision:` until the NameServer names a
master and every broker, and creates the topics `j` and `o` (4 queues each) and the group `w`'s
retry and dead-letter topics `%RETRY%w` and `%DLQ%w` (1 queue each, readable) on every broker.
The test composer runs 30 rounds of 1 s with up to 2 of these commands each:

- `parallel_driver_pub`: one `SendSync` to `j`, to the queue of the command's number. One in five
  messages is a poison message (`x<id>`), and one in five is delayed by level 2, 5 s (`d<id>`).
  Only `SEND_OK` counts as acknowledged; another status or an error is an unknown outcome.
- `parallel_driver_tx`: one transactional message to `j`. `t<id>` commits, `r<id>` rolls back,
  and `u<id>` answers "unknown", so that the broker's check commits it. The decision is written
  to a synced file before the half message is sent, and every producer of the group `txg`
  answers checks from it.
- `parallel_driver_opub`: one `SendSync` to `o`, to the queue of the message's key (`k0` to
  `k3`).
- `parallel_driver_consume` and `parallel_driver_oconsume`: one consumer of the group `w` (on a
  queue of `j` or on `%RETRY%w`) or `og` (on a queue of `o`). It asks the master for the group's
  committed offset, pulls up to 4 messages from it, gives each poison message back with
  `CONSUMER_SEND_MSG_BACK` (delay level 1, at most 2 reconsumes), and commits the next offset
  with a synchronous `UPDATE_CONSUMER_OFFSET`. A lock per queue and group lets one consumer at a
  time take a queue, as RocketMQ's rebalancing does. rocketmq-client-go sends offsets one-way, so
  the example's own remoting client sends them.

The drain (`rmq drain`) answers the broker's transaction checks and takes every message left of
`j`, `o`, `%RETRY%w` and `%DLQ%w`. It ends after a quiet window of 45 s without a message that it
had not received yet, so that a message that comes again and again (finding 3) does not keep it
open. It records a receive only when it got messages, which keeps hvx's 64 KiB of console per boot
for the workload's records.

## The rules

Two `type: messages` checks, one history each (`hydo guide messages`):

- `jobs` (history `q`): `delivery: at-least-once`, `duplicates: forbidden` (one stored copy per
  message, so a committed transaction is delivered as one message), `dead_letter: {after: 3, to:
  dlq}` and `delay: per-message`. A rolled-back transaction is a failed send, so a delivery of it
  would be an aborted delivery (`no-phantom`).
- `order` (history `o`): `delivery: at-least-once` and `order: {kind: fifo, scope: key}`.

The drain is the `final:` of `jobs`; `order` has the placeholder `final: "true"`.

`hydo/hydo-offsets.yaml` is `hydo.yaml` with one more claim in both checks, `after_ack:
no-redelivery`: a committed consumer offset holds, also across a failover. Every failover breaks
it (finding 2), so the example's manifest leaves it out, and the variant keeps that finding's
regression.

## What was found

### 1. Known in its parts: after a power cut, two of the three brokers never start again

**The schedule.** `disk=cut | run C=20`: a power cut of the whole guest before step 20. In the
recovery boot `broker0` exits with status 255 at every start, until its readiness limit of 300 s
runs out: `VIOLATION service cannot recover (committed data unrecoverable) (broker0)`. Its log
ends with:

```text
java.lang.NumberFormatException: null
	at java.lang.Integer.parseInt(Integer.java:542)
	at org.apache.rocketmq.common.utils.CheckpointFile.read(CheckpointFile.java:107)
	at org.apache.rocketmq.common.utils.CheckpointFile.read(CheckpointFile.java:146)
	at org.apache.rocketmq.store.ha.autoswitch.EpochFileCache.initCacheFromFile(EpochFileCache.java:59)
	at org.apache.rocketmq.store.ha.autoswitch.AutoSwitchHAService.init(AutoSwitchHAService.java:84)
	at org.apache.rocketmq.store.DefaultMessageStore.start(DefaultMessageStore.java:426)
```

**What the disk held.** `hydo inspect` at the end of the recovery boot listed each broker's
store. The epoch file `epochFileCheckpoint` of both slaves, `broker0` and `broker1`, is 0 bytes;
each was written at 00:00:38 of the guest clock, when the controller made the node a slave, 17 s before the cut,
and neither has a `.bak` file. The master's file, `broker2`'s, holds its epoch `1-5858`. The commit
log, which `SYNC_FLUSH` syncs, is whole on all three.

**The cause.**

1. A broker in controller mode keeps its epochs in `epochFileCheckpoint`
   (`EpochFileCache.flush` and `CheckpointFile.write`). The file is written with
   `MixAll.string2File`, which copies the old content to `.bak` and then writes the file in place
   (`IOTinyUtils.writeStringToFile`). Neither write is synced, and the directory is not synced. A
   power cut in the next seconds leaves the file truncated to 0 bytes: the new size reached the
   file system's journal, its data did not.
2. At the next start `CheckpointFile.read` parses the first line with `Integer.parseInt`. An empty
   file gives `null`, and `NumberFormatException` is not an `IOException`, so `read()` never tries
   the `.bak` file, and `EpochFileCache.initCacheFromFile` does not catch it. `BrokerStartup` exits.
3. The broker stops this way at every start, so the replica group keeps one broker. The
   SyncStateSet shrinks to the master, and a loss of that one broker leaves the group without a
   master, because the controller elects only from the SyncStateSet.

**The duplicate search.** Issue #11460 (open since 2026-10-20) names step 2 from a unit test: an
empty or malformed header skips the `.bak` recovery. Pull request #11463 (open) turns the parse
error into an `IOException`. Here no `.bak` exists, so with that fix the read returns no entries
and the broker would start with an empty epoch cache. Issue #5905 (closed, RocketMQ 4.x) reported
config files of size 0 after a forced restart of a VM; the code since writes in place, still
without a sync. No issue shows a broker that never starts after a power cut in controller mode.
Label: known in its parts (#11460, #5905); the power cut that makes the empty file is the new
detail.

**Native run.** None: the cause is plain in the code, and the empty file is on the disk.

### 2. Known: a failover moves a group's committed offsets back

**The schedule.** `disk=none | run K=brokers.primary@10+3`: a kill of the master (`broker2`)
before step 10, a restart before step 13. A pause of the master past the controller's heartbeat
timeout (`P=brokers.primary@10+15`) does the same. With `hydo-offsets.yaml`:
`VIOLATION [jobs.after-ack] m2 was delivered again to process 27 in group w of channel j after
process 7's acknowledgement of it completed at 41.093 s`, and `order.after-ack` breaks in the same
run for `o6`. No acknowledged message was lost (`no-loss` held for 14 and 9 messages).

**The cause.** A slave copies the consumer offsets from the master every 10 s
(`ReplicasManager.handleSlaveSynchronize`, which calls `SlaveSynchronize.syncAll()` when 10 s of
the wall clock passed). The new master starts with the offsets of its last copy, so a group goes
back up to 10 s and gets those messages again. A slave measures the 10 s on the wall clock, so
after a backward clock step it copies nothing for the length of the step, and a failover in that
time moves the offsets back further.

**The duplicate search.** Issue #7790 (DLedger mode, closed as stale) reports repeated consumption
after a master switch; a maintainer's answer there: "Since the consumption progress is synchronized
every 10 seconds, ... in theory, repeated consumption will occur as long as a master-slave switch
occurs." Label: known, and RocketMQ promises at-least-once delivery. The task's promise that an
offset is not moved back after a failover does not hold; the documentation does not make it.

### 3. New, a clock effect: after a backward clock step, the transaction check stores committed transactions again, or checks none

The broker writes the end of each transaction, its commit or rollback, as an operation record
of the topic `RMQ_SYS_TRANS_OP_HALF_TOPIC`. It does not write each one at once: it collects them
per queue and writes the batch when `transactionOpBatchInterval` (3 s) has passed since the last
write, measured with `System.currentTimeMillis()` (`TransactionalMessageServiceImpl.
batchSendOpMessage`, `startTime - mqContext.getLastWriteTimestamp() < interval`). After a step
back of the wall clock that difference is negative, so the records of every transaction that
ends after the step stay in memory until the clock passes the last write again: for the size of
the step, or until 4,096 bytes of them collect (`transactionOpMsgMaxSize`). The check
(`TransactionalMessageServiceImpl.check`) decides from those records which half messages still
need a question to the producer. Two schedules show the two outcomes.

**3a. A committed transaction is stored again at every check.** `disk=none | run T=10:-3600`:
the wall clock steps back by an hour before step 10, and nothing else. `VIOLATION
[jobs.duplicates] u17 is stored twice: it carries the system ids 1000000 and 1000005`. `u17`,
`u20` and `u44` are transactional messages whose producer answered "unknown", so the broker's
check committed them. Their commit records stayed in memory, so the next check found them
without one and asked again. The check also puts each half message back into the half queue
(`putBackHalfMsgQueue`). The drain's producer answered "commit" each time from the decisions
file, and the broker stored the message again: the drain received `u17` at the offsets 1000000
and 1000005 to 1000009, and `u20` and `u44` six times each, in the last 50 s of the run, once per
check interval of 10 s. In the clean run the broker checks each of them once. The check is
eager here for a second reason: it asks about a half message without a record when the last
operation record it read was born more than the transaction timeout after the check began
(lines 295 to 297), and the records of the transactions that ended before the step were born an
hour ahead.

**3b. No transaction is checked, and an acknowledged one is never delivered.** `disk=none |
run T=2:-2592000`: the wall clock steps back by 30 days before step 2. `VIOLATION [jobs.no-loss]
u17 was acknowledged to process 17 at 46.316 s and never delivered in group w of channel j,
which was drained`. The client's notes show no check at all in the run. `r3`, rolled back in
the round before the step, is the only transaction before it; the code gives the rest. Unless its
record was written in the 3 s before the step, it waited in the batch. The check reads the half
queue in order, finds `r3` without a record, and `r3` was stored 30 days "after" the check began,
so the check stops there as at a fresh message (`if (msgExt.getStoreTimestamp() >= startTime)
break;`, line 269). It stops there at every round, so
no later transaction is checked either: `u17`, `u20` and `u44`, whose producers answered
"unknown", are never committed. With a step back of 30 s the stop lasts 30 s, and in e2 five
schedules with steps back of 30 s to a day ended with such a message not delivered when the
drain ended.

A committed transaction then becomes several messages (3a), or an acknowledged one waits for
the size of the step (3b). RocketMQ's promise for transactions is that a committed message is
delivered once to each group, and a rolled-back one never.

**The duplicate search.** Issue #9640 (closed) reports checks of transactions that the producer
had committed, under load, from the gap between the birth times of a half message and of its
operation record; it names no clock step and no message stored twice. No issue or pull request
names a clock step, `batchSendOpMessage`, or `transactionOpBatchInterval` together with a lost
or repeated transaction (the section "The duplicate searches"). Label: new as far as the search
found, a clock effect.

**Native run.** None: the cause is plain in the code, and the client's notes show which checks
came.

RESULTS_REST

## What hydo lacked

In the order of the time each cost:

1. **On AMD EPYC 7763 runners the image does not boot on KVM, and the parts count it as
   failures.** Every part of e1 on an EPYC 7763 host (parts 12, 16, 18 and 19) stopped in the
   boot before S1: `ERROR boot: SPIN (guest loops without exits; run invalid)` after 12,695 to
   18,135 stops. So 8 of e1's 40 seeds never ran. The part's job passed, its `exploration.json`
   counted both seeds as failures with the signature `ERROR worker: ''`, and its `explore.txt`
   alone said why. The EPYC 9V45 and the Intel hosts booted the same image.
2. **The drain filled hvx's console.** hvx keeps 64 KiB of console per boot. The first drain
   recorded every empty receive of 10 queues once a second, 1,111 history records in a clean run,
   and the console reached 65,673 bytes. The drain now records a receive only when it got
   messages, and writes its invocation after the pull; the clean run's console is 24 KB. Nothing
   warns while a run's records are cut off (as open item 181 says for Pulsar).
2. **`L=` takes no group selector.** `K=brokers.primary@10+3` and `P=brokers.primary@...` aim at
   the master, but `L=brokers.primary@10+3` stopped with `"brokers.primary" is not a service name`
   before any VM. One broker's power loss has to name the broker, which depends on the image.
3. **A claim cannot be a build parameter.** Every failover breaks `after_ack: no-redelivery` by
   design (finding 2), which would hide every other failure of a failover. The example keeps the
   claim in a second manifest, `hydo-offsets.yaml`, built as an image of its own (as open items
   140 and 181 say for `faults:` and claims).
4. **`hydo build --check` passes a disk that `hydo build` refuses.** With 1,792 MiB of data disk
   and four storage file systems of 1,664 MiB, `--check` printed OK, and the build stopped after
   about 2 minutes: `the disk with the services' own file systems is 4101 MiB, more than the 4096
   MiB an engine maps`.
5. **The import moved every address by one.** The Compose file gives the NameServer
   `10.89.0.10` and the brokers `.11` to `.13`. `hydo compose` warned `ipv4_address 10.89.0.10
   ignored; the service is at 10.89.0.11` for each, and wrote addresses one higher. The brokers'
   configuration names the NameServer's and the controller's address, so the example sets each
   `addr:` back by hand.
6. **The import took the first port of `EXPOSE`.** The image exposes 9876, 10909, 10911 and
   10912, and every broker got `port: 9876` and a readiness check on it, the NameServer's port.
7. **`ci/remote-explore.sh --ref` with a short commit fails on the runner.** The build's
   checkout fetched `refs/heads/4b28431b*` and stopped after a minute; a branch name works.
8. **A routine run replays itself.** `hydo run` makes a diagnosis replay after every failing
   run, which doubles the time of a directed run of 5 to 10 minutes; the directed runs used
   `--no-diagnose`.
