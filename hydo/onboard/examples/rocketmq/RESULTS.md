# Apache RocketMQ under hydo: a replica group in controller mode

Apache RocketMQ (github.com/apache/rocketmq) is a message broker in Java. This example runs RocketMQ
5.5.0, the newest release with an official image, in controller mode: a NameServer with the
controller embedded, and one replica group `broker-a` of three brokers, in one guest of hvx on the
bare-metal machine. It uses the settings that RocketMQ's documentation names for data safety:
`allAckInSyncStateSet`, which acknowledges a message only after every replica of the SyncStateSet
has it, and `SYNC_FLUSH`. It looks for bugs that nobody reported.

## The short answers

- **What was found?** One new candidate, a clock effect, one finding known in its parts, and one
  known finding. Nothing was posted.
  1. **Known in its parts: after a power cut, the slaves never start again.** A broker writes its
     epoch file `epochFileCheckpoint` without a sync when the controller gives it a role. A power
     cut 17 s later left both slaves' files at 0 bytes, and each broker exits at every start on the
     unchecked parse of an empty header. The replica group keeps one broker. Issue #11460 names the
     parse; #5905 named config files of 0 bytes after a forced restart
     (`regressions/slaves-never-start-after-power-cut.schedule`). One broker's power loss does the
     same.
  2. **Known: a failover moves a group's committed offsets back by up to 10 s.** A slave copies the
     offsets from its master every 10 s, so after a kill of the master the consumers get
     acknowledged messages again (#7790). The same copy holds the delay offsets and the transaction
     check's progress, so a failover can store a delayed message or a committed transaction a second
     time (`hydo-offsets.yaml`, its inline regression).
  3. **New, a clock effect: after a backward step of the wall clock, the transaction check stores
     committed transactions again, or checks none.** The broker writes the commit and rollback
     records of transactions in batches timed by the wall clock, so after a step back it writes none
     for the step's length. A step back of an hour made the broker store each committed transaction
     again at every check, six times in 50 s
     (`regressions/committed-transaction-stored-again-after-clock-step.schedule`). A step back of 30
     days stopped the check at a half message without its record, so no transaction was checked, and
     one that waited for a check was never delivered
     (`regressions/no-transaction-checked-after-clock-step.schedule`).
- **What held?** No message that `SendSync` acknowledged with `SEND_OK` was lost under kills, pauses
  past the controller's heartbeat timeout, partitions and one-way loss, failovers, a kill of the
  NameServer with its controller, or clock steps, whenever every broker came back. A rolled-back
  transaction was never delivered, a delayed message never came before its delay, a poison message
  reached the dead-letter topic after its retries, and each key's messages came in order.
- **What is the environment or detected damage?** A damaged file of a broker's store (`G=`) made
  that broker refuse to start (exit status 253): RocketMQ detects it, and the run ends in an ERROR.
- **What hydo lacked?** The section "What hydo lacked" lists thirteen gaps. The two that cost most
  were the KVM boots on AMD EPYC 7763 runners, which spin before S1 and count as failures, and hvx's
  console of 64 KiB, which the drain's records filled.

## The version

- RocketMQ 5.5.0 (`rocketmq-all-5.5.0`, released 2026-04-10). Its official image
  `apache/rocketmq:5.5.0` (published 2026-05-04) is the newest: 5.5.1 (2026-08-20) has no image. The
  image is
  `docker.io/apache/rocketmq@sha256:7e8f6c9dbd9df742ed26ba69c00d4ad69e2f86a56f3ca7782ff8144dd0798132`,
  linux/amd64 `sha256:a613f1341f3404b96900cf11e5c35f2d049c103504a1dd60242a04cebf5be467`, with
  Eclipse Temurin's Java 8u482.
- The client: rocketmq-client-go v2.1.2, the newest release of RocketMQ's Go client for the remoting
  protocol, with hydo's Go SDK v0.6.0 (`hydohist`), and a small remoting client of the example's own
  (`client/remoting.go`) for what the Go client does not offer.
- hydo of `main` at `bdb403f4` (0.7.0 and Milestones 368 to 374), with the guest files of the
  published 0.6.0 Linux release, on hvx.

## The promises

These are the promises under test, in the words of RocketMQ's documentation:

- Controller mode (`docs/en/controller/deploy.md`): with `allAckInSyncStateSet`, "a message needs to
  be replicated to every replica in SyncStateSet before it is returned to the client as successful,
  and this parameter can ensure that the message is not lost". The controller elects a master only
  from the SyncStateSet while `enableElectUncleanMaster` is `false` (the default): electing from
  outside "may select a replica with lagging data as Master and lose messages".
- `SYNC_FLUSH`: the master answers a send after the message is on its disk.
- Transactional messages: a half message is delivered only after its commit, never after a rollback,
  and the broker asks the producer group about a transaction whose producer sent neither.
- Ordered messages: the messages of one queue are delivered in the order they were stored.
- Delayed messages and retries: a message with a delay level is delivered after its delay; a message
  that a consumer gives back is delivered again from the retry topic, and after the maximum
  reconsume times it goes to the dead-letter topic.
- Consumer offsets: the broker keeps a group's committed offset per queue, and a consumer goes on
  from it. The promise that an offset holds across a failover is the task's, unconfirmed by the
  owner: the documentation promises at-least-once delivery.

## The project

`docker-compose.yaml` runs the NameServer `namesrv` with the controller embedded
(`config/namesrv.conf`, the documentation's "Embed NameServer deployment" with one controller) and
three brokers `broker0` to `broker2` of the replica group `broker-a` (`config/broker.conf`, started
by `config/start-broker.sh`). Every broker starts as a slave with `brokerId -1`, and the controller
chooses the master. Each node's store is in a volume of its own. The JVMs have small heaps (128 MiB
for the NameServer, 256 MiB for a broker), and the commit log's files are 64 MiB. `hydo compose`
imported it, and `hydo/hydo.yaml` changes the import in these ways:

- each service keeps the address of the Compose file: the importer moved every address by one
  (`ipv4_address 10.89.0.10 ignored; the service is at 10.89.0.11`), and the brokers' configuration
  names the NameServer's and the controller's address;
- each node's store (`/home/rocketmq/store`) is a file system of its own (`storage:`), the target of
  `L=` and `G=`, which `setup:` gives to the image's user (uid 3000), as `hydo build --check`
  advised;
- a broker's port is 10911 and its readiness check a connect to it; the importer took 9876 from the
  image's `EXPOSE` for each broker;
- the logs and the JVM's GC log go to `/dev/shm`, because the image's root is read-only in the
  guest;
- the brokers' settings `allAckInSyncStateSet`, `flushDiskType`, `minInSyncReplicas` and
  `transactionCheckInterval` are build parameters; the first two are `configs:` entries;
- the three brokers form the group `brokers`, with `primary: /bin/rmq leader` (the master that the
  NameServer names), `links: all` and `restart: together` (Milestone 374).

Deviations from RocketMQ's defaults, each for the test: `transactionCheckInterval` is 10 s instead
of 60 s and `transactionTimeOut` 6 s, so that a check comes within a run; `fileReservedTime` is 100
years, so that a forward clock step deletes no file.

## The workload

The test client `rmq` (`client/`, `build.sh`) waits in `provision:` until the NameServer names a
master and every broker, and creates the topics `j` and `o` (4 queues each) and the group `w`'s
retry and dead-letter topics `%RETRY%w` and `%DLQ%w` (1 queue each, readable) on every broker. The
test composer runs 30 rounds of 1 s with up to 2 of these commands each:

- `parallel_driver_pub`: one `SendSync` to `j`, to the queue of the command's number. One in five
  messages is a poison message (`x<id>`), and one in five is delayed by level 2, 5 s (`d<id>`). Only
  `SEND_OK` counts as acknowledged; another status or an error is an unknown outcome.
- `parallel_driver_tx`: one transactional message to `j`. `t<id>` commits, `r<id>` rolls back, and
  `u<id>` answers "unknown", so that the broker's check commits it. The decision is written to a
  synced file before the half message is sent, and every producer of the group `txg` answers checks
  from it.
- `parallel_driver_opub`: one `SendSync` to `o`, to the queue of the message's key (`k0` to `k3`).
- `parallel_driver_consume` and `parallel_driver_oconsume`: one consumer of the group `w` (on a
  queue of `j` or on `%RETRY%w`) or `og` (on a queue of `o`). It asks the master for the group's
  committed offset, pulls up to 4 messages from it, gives each poison message back with
  `CONSUMER_SEND_MSG_BACK` (delay level 1, at most 2 reconsumes), and commits the next offset with a
  synchronous `UPDATE_CONSUMER_OFFSET`. A lock per queue and group lets one consumer at a time take
  a queue, as RocketMQ's rebalancing does. rocketmq-client-go sends offsets one-way, so the
  example's own remoting client sends them.

The drain (`rmq drain`) answers the broker's transaction checks and takes every message left of `j`,
`o`, `%RETRY%w` and `%DLQ%w`. It ends after a quiet window of 45 s without a message that it had not
received yet, so that a message that comes again and again (finding 3) does not keep it open. It
records a receive only when it got messages, which keeps hvx's 64 KiB of console per boot for the
workload's records.

## The rules

Two `type: messages` checks, one history each (`hydo guide messages`):

- `jobs` (history `q`): `delivery: at-least-once`, `duplicates: forbidden` (one stored copy per
  message, so a committed transaction is delivered as one message), `dead_letter: {after: 3, to:
  dlq}` and `delay: per-message`. A rolled-back transaction is a failed send, so a delivery of it
  would be an aborted delivery (`no-phantom`).
- `order` (history `o`): `delivery: at-least-once` and `order: {kind: fifo, scope: key}`.

The drain is the `final:` of `jobs`; `order` has the placeholder `final: "true"`.

`hydo/hydo-offsets.yaml` is `hydo.yaml` with one more claim in both checks, `after_ack:
no-redelivery`: a committed consumer offset holds, also across a failover. Every failover breaks it
(finding 2), so the example's manifest leaves it out, and the variant keeps that finding's
regression.

## Without faults, and the regressions

The final image `explore-ob-rocketmq-disk.img` (`0198df27c8ec09e5…`) gives `b40afc5cabb53082` on two
clean runs of `disk=none | run` on hvx. The run's 46 commands send 12 messages and 8 transactions to
`j` and 9 messages to `o`, and consume 17 times; every claim holds. Each saved regression gave one
fingerprint over two fresh runs on hvx:

| regression | finding | fingerprint |
|---|---|---|
| `slaves-never-start-after-power-cut.schedule` | 1 | `b5252e4bc9b73a2c` |
| `committed-transaction-stored-again-after-clock-step.schedule` | 3a | `5fc8566af3802f2f` |
| `no-transaction-checked-after-clock-step.schedule` | 3b | `e4f10ed629b3d8be` |
| `offsets-move-back-at-failover` (inline in `hydo-offsets.yaml`) | 2 | `8cd67eee89799522`, on that manifest's image |

`hydo repro --regressions` on the final image gave 3 of 3 as expected, with the same fingerprints.
On the image of `hydo-offsets.yaml` it gave 4 of 4 as expected; that image's fingerprints are its
own.

## What was found

### 1. Known in its parts: after a power cut, two of the three brokers never start again

**The schedule.** `disk=cut | run C=20`: a power cut of the whole guest before step 20. In the
recovery boot `broker0` exits with status 255 at every start, until its readiness limit of 300 s
runs out: `VIOLATION service cannot recover (committed data unrecoverable) (broker0)`. Its log ends
with:

```text
java.lang.NumberFormatException: null
	at java.lang.Integer.parseInt(Integer.java:542)
	at org.apache.rocketmq.common.utils.CheckpointFile.read(CheckpointFile.java:107)
	at org.apache.rocketmq.common.utils.CheckpointFile.read(CheckpointFile.java:146)
	at org.apache.rocketmq.store.ha.autoswitch.EpochFileCache.initCacheFromFile(EpochFileCache.java:59)
	at org.apache.rocketmq.store.ha.autoswitch.AutoSwitchHAService.init(AutoSwitchHAService.java:84)
	at org.apache.rocketmq.store.DefaultMessageStore.start(DefaultMessageStore.java:426)
```

**What the disk held.** `hydo inspect` at the end of the recovery boot listed each broker's store.
The epoch file `epochFileCheckpoint` of both slaves, `broker0` and `broker1`, is 0 bytes; each was
written at 00:00:38 of the guest clock, when the controller made the node a slave, 17 s before the
cut, and neither has a `.bak` file. The master `broker2` started again and rewrote its own file in
the recovery boot (`1-5858`). Every broker's commit log folder was there.

**The cause.**

1. A broker in controller mode keeps its epochs in `epochFileCheckpoint` (`EpochFileCache.flush` and
   `CheckpointFile.write`). The file is written with `MixAll.string2File`, which copies the old
   content to `.bak` and then writes the file in place (`IOTinyUtils.writeStringToFile`). Neither
   write is synced, and the directory is not synced. A power cut in the next seconds leaves the file
   truncated to 0 bytes: the new size reached the file system's journal, its data did not.
2. At the next start `CheckpointFile.read` parses the first line with `Integer.parseInt`. An empty
   file gives `null`, and `NumberFormatException` is not an `IOException`, so `read()` never tries
   the `.bak` file, and `EpochFileCache.initCacheFromFile` does not catch it. `BrokerStartup` exits.
3. The broker stops this way at every start, so the replica group runs on one broker. A loss of
   that broker would leave the group without a master, because the controller elects only from the
   SyncStateSet (`enableElectUncleanMaster=false`).

**The duplicate search.** Issue #11460 (open since 2026-10-20) names step 2 from a unit test: an
empty or malformed header skips the `.bak` recovery. Pull request #11463 (open) turns the parse
error into an `IOException`. Here no `.bak` exists, so with that fix the read returns no entries and
the broker would start with an empty epoch cache. Issue #5905 (closed, RocketMQ 4.x) reported config
files of size 0 after a forced restart of a VM; the code since writes in place, still without a
sync. No issue shows a broker that never starts after a power cut in controller mode. Label: known
in its parts (#11460, #5905); the power cut that makes the empty file is the new detail.

**Native run.** None: the cause is plain in the code, and the empty file is on the disk.

### 2. Known: a failover moves a group's committed offsets back

**The schedule.** `disk=none | run K=brokers.primary@10+3`: a kill of the master (`broker2`) before
step 10, a restart before step 13. A pause of the master past the controller's heartbeat timeout
(`P=brokers.primary@10+15`) does the same. On the image of `hydo-offsets.yaml`: `VIOLATION
[jobs.after-ack] m2 was delivered again to process 27 in group w of channel j after process 7's
acknowledgement of it completed at 40.037 s`. On the first image `order.after-ack` broke in the same
run for `o6`. No acknowledged message was lost, and on the final image of `hydo.yaml` the schedule
passes every claim.

**The cause.** A slave copies the consumer offsets from the master every 10 s
(`ReplicasManager.handleSlaveSynchronize`, which calls `SlaveSynchronize.syncAll()` when 10 s of the
wall clock passed). The new master starts with the offsets of its last copy, so a group goes back up
to 10 s and gets those messages again. A slave measures the 10 s on the wall clock, so after a
backward clock step it copies nothing for the length of the step, and a failover in that time moves
the offsets back further. The same copy holds the delay offsets and the transaction check's progress
(the section "The explorations"), so a failover can also store a delayed message or a committed
transaction a second time.

**The duplicate search.** Issue #7790 (DLedger mode, closed as stale) reports repeated consumption
after a master switch; a maintainer's answer there: "Since the consumption progress is synchronized
every 10 seconds, ... in theory, repeated consumption will occur as long as a master-slave switch
occurs." Label: known, and RocketMQ promises at-least-once delivery. The task's promise that an
offset is not moved back after a failover does not hold; the documentation does not make it.

### 3. New, a clock effect: after a backward clock step, the transaction check stores committed transactions again, or checks none

The broker writes the end of each transaction, its commit or rollback, as an operation record of the
topic `RMQ_SYS_TRANS_OP_HALF_TOPIC`. It does not write each one at once: it collects them per queue
and writes the batch when `transactionOpBatchInterval` (3 s) has passed since the last write,
measured with `System.currentTimeMillis()` (`TransactionalMessageServiceImpl. batchSendOpMessage`,
`startTime - mqContext.getLastWriteTimestamp() < interval`). After a step back of the wall clock
that difference is negative, so the records of every transaction that ends after the step stay in
memory until the clock passes the last write again: for the size of the step, or until 4,096 bytes
of them collect (`transactionOpMsgMaxSize`). The check (`TransactionalMessageServiceImpl.check`)
decides from those records which half messages still need a question to the producer. Two schedules
show the two outcomes.

**3a. A committed transaction is stored again at every check.** `disk=none | run T=10:-3600`: the
wall clock steps back by an hour before step 10, and nothing else. `VIOLATION [jobs.duplicates] u17
is stored twice: it carries the system ids 1000000 and 1000005`. `u17`, `u20` and `u44` are
transactional messages whose producer answered "unknown", so the broker's check committed them.
Their commit records stayed in memory, so the next check found them without one and asked again. The
check also puts each half message back into the half queue (`putBackHalfMsgQueue`). The drain's
producer answered "commit" each time from the decisions file, and the broker stored the message
again: the drain received `u17` at the offsets 1000000 and 1000005 to 1000009, and `u20` and `u44`
six times each, in the last 50 s of the run, once per check interval of 10 s. In the clean run the
broker checks each of them once. The check is eager here for a second reason: it asks about a half
message without a record when the last operation record it read was born more than the transaction
timeout after the check began (lines 295 to 297), and the records of the transactions that ended
before the step were born an hour ahead.

**3b. No transaction is checked, and an acknowledged one is never delivered.** `disk=none | run
T=2:-2592000`: the wall clock steps back by 30 days before step 2. `VIOLATION [jobs.no-loss] u17 was
acknowledged to process 17 at 46.316 s and never delivered in group w of channel j, which was
drained`. The client's notes show no check at all in the run. `r3`, rolled back in the round before
the step, is the only transaction before it; the code gives the rest. Unless its record was written
in the 3 s before the step, it waited in the batch. The check reads the half queue in order, finds
`r3` without a record, and `r3` was stored 30 days "after" the check began, so the check stops there
as at a fresh message (`if (msgExt.getStoreTimestamp() >= startTime) break;`, line 269). It stops
there at every round, so no later transaction is checked either: `u17`, `u20` and `u44`, whose
producers answered "unknown", are never committed. With a step back of 30 s the stop lasts 30 s, and
in e2 five schedules with steps back of 30 s to a day ended with such a message not delivered when
the drain ended.

A committed transaction then becomes several messages (3a), or an acknowledged one waits for the
size of the step (3b). RocketMQ's promise for transactions is that a committed message is delivered
once to each group, and a rolled-back one never.

**The duplicate search.** Issue #9640 (closed) reports checks of transactions that the producer had
committed, under load, from the gap between the birth times of a half message and of its operation
record; it names no clock step and no message stored twice. No issue or pull request names a clock
step, `batchSendOpMessage`, or `transactionOpBatchInterval` together with a lost or repeated
transaction (the section "The duplicate searches"). Label: new as far as the search found, a clock
effect.

**Native run.** None: the cause is plain in the code, and the client's notes show which checks came.

## The duplicate searches

Each phrase was searched with `gh search issues --repo apache/rocketmq --include-prs`, open and
closed, on 2026-11-27.

| phrase | finding | what it found |
|---|---|---|
| `epochFileCheckpoint` | 1 | #6586, #6250, #6100: none about an empty file |
| `CheckpointFile`, `EpochFileCache` | 1 | #11460 and its pull request #11463 (open): the unchecked parse of an empty header skips `.bak`; #10863, #11032, #11642: empty checkpoint state, not a power cut |
| `string2File`, `power off`, `power failure` | 1 | #5905 (closed, 4.x): config files of 0 bytes after a forced restart of a VM; #4463, #405, #3129: other files |
| `controller mode broker start`, `NumberFormatException epoch`, `broker cannot start after` | 1 | nothing |
| `repeated consumption`, `syncConsumerOffset` | 2 | #7790 (closed as stale): repeated consumption after a master switch, with a maintainer's explanation |
| `SlaveSynchronize`, `consume offset master slave switch`, `delayOffset` | 2 | #9984, #9996, #11041: other metadata of the copy; nothing about offsets moving back |
| `batchSendOpMessage`, `transactionOpBatchInterval` | 3 | #10966 and #10967: a busy loop of the same service, not a clock step |
| `transaction check`, `half message`, `valueOfCurrentMinusBorn` | 3 | #9640 (closed): checks of committed transactions under load; #9486, #11574, #11622: other causes |
| `system time`, `clock`, `currentTimeMillis`, `time rollback`, `clock jump` | 3 | #9455 (closed): a broker that does not register after a forward and back jump; #6679: a client's subscription after a step back |
| `transaction duplicate`, `transactional message delivered twice`, `delay message duplicate` | 3 | nothing |

## The explorations

Both explorations ran on GitHub's KVM runners with `explore.yml` (`ci/remote-explore.sh`), 20 parts
of 2 seeds each, one worker per part, with `--strategy process --new-faults --cluster-faults
--storage-faults --no-shrink`: the shrinks of a power cut or a clock step take hours on KVM (the
Garnet example), and directed runs on hvx judged each failure instead. A first run took 5 to 12
minutes on a runner; e1's last part ran 2.5 hours, and e2's two slowest about as long. The runners
were Intel Xeon 6975P-C and Platinum 8573C hosts and AMD EPYC 9V45, 9V74 and 7763 hosts.

| exploration | image | seeds | runs that ran | failures by cause |
|---|---|---|---|---|
| e1 | the first client (commit `910deefc`) | 1-40 | 32: the 4 parts on EPYC 7763 hosts did not boot | finding 1 at a cut (seeds 12, 20, 21, 40); one broker's power loss stopping it at its restart (seeds 1, 6, 9, 16, 25, 26, 28, 29); finding 3a as an exhausted exit budget of the drain (seed 7); the provisioning message `p` that a failover delivered again (11 seeds: the client's mistake, and finding 2); the attempt count of a redelivery (seeds 4 and 15: the client's mistake); a no-loss of an unknown transaction after a step back of 30 days (seed 18, finding 3b; on hvx its replay broke `dead-letter` first); detected damage of a file (seed 13); the group's `primary:` that named no member (seed 10); 2 seeds passed |
| e2 | the final client and image (commit `4b28431b`) | 41-80 | 34: the parts on EPYC 7763 hosts and one EPYC 9V74 host did not boot | finding 3b after steps back of 30 s to a day (seeds 42, 45, 51, 56, 66, 79); an unknown transaction not delivered after forward steps of 5 minutes and a day, each with kills (seeds 61, 77, 80; not traced); finding 1 at a cut (seeds 49, 52, 57, 78) and at one broker's power loss (seeds 48, 50, 72); a committed transaction stored twice after failovers (seeds 47, 69) and a delayed message stored twice after a failover (seeds 58, 62, 70): the 10-s copy of finding 2 (see below); the attempt count of a give-back that got no answer (seed 53, gap 12 below); an exhausted exit budget after a step back of an hour (seeds 54 and 59, as e1's seed 7); detected damage (seed 67); 9 seeds passed |

**The copy every 10 s is more than the consumer offsets.** `SlaveSynchronize.syncAll()` also copies
the delay offsets of `SCHEDULE_TOPIC_XXXX` and the consume offsets of the transaction check's half
and operation queues. After a failover the new master starts from them, so a delayed message whose
delay ended after the last copy is stored in `j` again, and a transaction that the old master had
committed is checked and committed again. e2's seeds 47, 58, 62, 69 and 70 broke `duplicates` so,
each with a kill or a pause of a broker, and seed 69 with a forward clock step besides. DELAYED

## What hydo lacked

In the order of the time each cost:

1. **On AMD EPYC 7763 runners the image does not boot on KVM, and the parts count it as failures.**
   Every part of e1 and e2 on an EPYC 7763 host, and one on an EPYC 9V74 host, stopped in the boot
   before S1: `ERROR boot: SPIN (guest loops without exits; run invalid)` after 12,695 to 18,135
   stops. So 14 of the 80 seeds never ran. The part's job passed, its `exploration.json` counted
   both seeds as failures with the signature `ERROR worker: ''`, its `explore.txt` alone said why,
   and the merged report called them "The run stopped early after hydo damaged a file of broker1".
   Other EPYC 9V74 hosts, the EPYC 9V45 and the Intel hosts booted the same image.
2. **The drain filled hvx's console.** hvx keeps 64 KiB of console per boot. The first drain
   recorded every empty receive of 10 queues once a second, 1,111 history records in a clean run,
   and the console reached 65,673 bytes. The drain now records a receive only when it got messages,
   and writes its invocation after the pull; the clean run's console is 24 KB. Nothing warns while a
   run's records are cut off (as open item 181 says for Pulsar).
3. **The exploration's problems name the wrong fault.** The merged report of e1 grouped the brokers
   that one broker's power loss stopped as "broker2 did not start again after hydo damaged a file of
   broker1": the class follows the first storage fault of the schedule, and the schedules had a `G=`
   beside the `L=`. Only directed runs separated them.
4. **`L=` takes no group selector.** `K=brokers.primary@10+3` and `P=brokers.primary@...` aim at the
   master, but `L=brokers.primary@10+3` stopped with `"brokers.primary" is not a service name`
   before any VM. One broker's power loss has to name the broker, which depends on the image.
5. **A claim cannot be a build parameter.** Every failover breaks `after_ack: no-redelivery` by
   design (finding 2), which would hide every other failure of a failover. The example keeps the
   claim in a second manifest, `hydo-offsets.yaml`, built as an image of its own, with its
   regression inline (as open items 140 and 181 say for `faults:` and claims).
6. **`hydo build --check` passes a disk that `hydo build` refuses.** With 1,792 MiB of data disk and
   four storage file systems of 1,664 MiB, `--check` printed OK, and the build stopped after about 2
   minutes: `the disk with the services' own file systems is 4101 MiB, more than the 4096 MiB an
   engine maps`.
7. **The import moved every address by one.** The Compose file gives the NameServer `10.89.0.10` and
   the brokers `.11` to `.13`. `hydo compose` warned `ipv4_address 10.89.0.10 ignored; the service
   is at 10.89.0.11` for each, and wrote addresses one higher (as open item 188). The brokers'
   configuration names the NameServer's and the controller's address, so the example sets each
   `addr:` back by hand.
8. **The import took the first port of `EXPOSE`.** The image exposes 9876, 10909, 10911 and 10912,
   and every broker got `port: 9876` and a readiness check on it, the NameServer's port.
9. **`ci/remote-explore.sh --ref` with a short commit fails on the runner.** The build's checkout
   fetched `refs/heads/4b28431b*` and stopped after a minute; a branch name works.
10. **A routine run replays itself.** `hydo run` makes a diagnosis replay after every failing run,
    which doubles the time of a directed run of 5 to 10 minutes. The directed runs used
    `--no-diagnose`, and then the diagnosis of a broker that did not start says only that no exit or
    readiness result was recorded.
11. **The verdict overstates a broker that does not start.** Finding 1 ends `VIOLATION service
    cannot recover (committed data unrecoverable) (broker0)`, while the stop came from the epoch
    file, and the commit log's folder was there.
12. **`dead_letter` counts a delivery whose give-back failed.** RocketMQ counts only the give-backs
    it accepted (`maxReconsumeTimes`). In the replay of e1's seed 18, the first give-back of `x5`
    got no answer during a failover, the message came again from `j`, and it went to the dead-letter
    topic after four deliveries: `x5 was delivered 4 times ... more than the limit of 3`.
13. **A group's `primary:` must name a member.** In e1's seed 10 the NameServer named no master
    while one was killed and the controller elected the next, and the run ended in `ERROR
    brokers.primary: the group's primary: command named no member`, before its checks (as open item
    192 for Garnet).

## Time

| phase | wall time |
|---|---|
| scouting: the release, the image, the controller's documentation, the Helm chart | 30 minutes |
| setup to a first clean run: the import, the client, the console's limit, the check-backs | 1 hour 20 minutes, about 30 minutes of it waiting for builds on the loaded machine |
| directed runs on hvx | about 7 hours, most of it waiting for runs of 5 to 10 minutes on the machine that a release pass loaded |
| explorations on KVM | about 5 hours, beside the directed runs |
| judging, the code of the transaction check, and the write-up | about 2 hours |
