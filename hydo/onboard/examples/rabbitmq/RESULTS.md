# RabbitMQ under hydo: three nodes with quorum queues and a stream

RabbitMQ (github.com/rabbitmq/rabbitmq-server) is a message broker in Erlang. This example runs
RabbitMQ 4.4.1, the newest release, as a cluster of three nodes in one guest of hvx on the
bare-metal machine. It uses the features that RabbitMQ's documentation names for data safety:
quorum queues, which replicate every message with Raft, a stream, and publisher confirms and
consumer acknowledgements. It looks for bugs that nobody reported.

## The short answers

- **What was found?** Two new candidates and one documented loss, all of the stream. Nothing
  was posted.
  1. **New: a power loss of one node while a second node is away loses a message of the stream
     that its publisher confirm acknowledged, for good.** A stream confirms a chunk once a
     majority of its members wrote it, and it never syncs, as RabbitMQ documents. `rabbit1` was
     paused for about 100 s, past the net tick time, so the writer `rabbit3` and the replica
     `rabbit2` confirmed the message. Then `rabbit3` lost its power and its copy. When all three
     nodes ran again, the stream coordinator chose a new writer from `rabbit1` and `rabbit3`,
     while `rabbit2` waited on a retry timer. Then `rabbit2` deleted its own copy to follow the
     new writer. RabbitMQ's documentation says that a loss on a single node "would typically just
     be re-replicated from the other nodes"
     (`regressions/stream-message-lost-after-one-node-power-loss.schedule`).
  2. **Documented: a power cut of the whole guest loses confirmed messages of the stream.** All
     three nodes shut down uncontrolled at once, and what the stream had not synced is gone. After
     a cut a new message can take the offset of a lost one
     (`regressions/stream-loses-confirmed-messages-at-power-cut.schedule`).
  3. **New, a clock effect: after a backward step of the wall clock by a day, a kill of the
     stream's writer leaves the stream without a writer for the size of the step.** Every publish
     to it answers `Leader not Ready yet` for the rest of the run, while all three nodes run
     again. Every node runs with `multi_time_warp`, so a node keeps its system time a day ahead
     until its next check of the OS clock. The stream coordinator's leader parked the failed
     restarts of two replicas with a retry time from that system time, which was a day away after
     the check, and the one member left was no quorum
     (`regressions/stream-keeps-no-leader-after-backward-clock-step.schedule`).
- **What held?** The quorum queues kept every message that a publisher confirm acknowledged,
  under kills, pauses past the net tick time, partitions, one-way loss, one node's power loss,
  damaged files of one node, power cuts of the whole guest, and clock steps. A consumer that
  crashed gave its message back, and a poison message was dead-lettered at least once after its
  delivery limit, into the dead-letter queue. The single active consumer of `k` got each key's
  messages in publish order. The stream stored no message twice and kept its offsets in publish
  order, and the reader's stored offset held: no message came again after its offset was stored.
- **What is the environment or detected damage?** A damaged metadata file of one node (the Ra
  WAL or `names.dets` of the metadata store, the feature flags file) stopped that node at every
  start: RabbitMQ detects the damage and refuses to start, and the run ends in an ERROR.
- **What hydo lacked?** The section "What hydo lacked" lists twelve gaps. The two that cost most
  were the restart of a killed group one node at a time, and a messages check's `final:` that
  runs before the `eventually_` commands.

## The version

- RabbitMQ 4.4.1 (`v4.4.1`, published 2026-11-16), the official image `rabbitmq:4.4.1`:
  `docker.io/library/rabbitmq@sha256:aa82e78ca0dfa59f4eeb9e8086ac7b5c8e0b31a7bb978d948c6d32756802fb27`,
  linux/amd64 `sha256:ed358b7010f0a5d5cf75ddbe3394a8700b57936f2e80222224bc00b9d7d618ef`. Erlang
  and the Ra library come with the image; RabbitMQ 4.4.1 pins Ra 3.2.0.
- The client: amqp091-go v1.17.0 and rabbitmq-stream-go-client v1.10.0, with hydo's Go SDK
  v0.6.0 (`hydohist`).
- hydo of `main` at `e2058be0` (0.6.0 and Milestones 364 to 367), with the guest files of the
  published 0.6.0 Linux release, on hvx.
- The released hydo 0.7.0 replayed finding 1 on hvx again, from a fresh clone at `1c318f23`,
  and finding 3, from a fresh clone at `4eb78297` (the paragraphs "The replay with hydo 0.7.0"
  of findings 1 and 3).

## The promises

These are the promises under test, in the words of RabbitMQ's documentation:

- Quorum queues (Data Safety): "A message that was successfully confirmed back to the publisher
  using the publisher confirms feature should not be lost as long as at least a majority of
  RabbitMQ nodes hosting the quorum queue are not permanently made unavailable." Confirms come
  only when a quorum of nodes has written and flushed the message to disk.
- Consumer acknowledgements (Consumer Acknowledgements and Publisher Confirms): a message that a
  consumer received and did not acknowledge is requeued when its channel or connection closes,
  and delivered again.
- Delivery limits (Quorum Queues, Poison Message Handling): a quorum queue counts the failed
  deliveries of each message, and a message whose count passes `x-delivery-limit` is dead-lettered
  instead of delivered again. With `x-dead-letter-strategy: at-least-once`, the move to the
  dead-letter queue is itself safe.
- Single active consumer: only one consumer of the queue receives messages at a time, so the
  queue's order holds for its consumer.
- Streams (Data Safety when using Streams): publisher confirms come once a quorum of replicas has
  the message. Streams do not flush (fsync) their data, so "an uncontrolled shutdown of a server
  could result in data loss for replicas hosted on that node".
- Stream offsets (offset tracking): a consumer that stores its offset on the server goes on after
  it, so a message whose offset it stored is not delivered to it again.

## The project

`docker-compose.yaml` runs three nodes `rabbit1` to `rabbit3` of the official image. They find
each other from fixed names with classic peer discovery (`config/rabbit<n>.conf`), share one
Erlang cookie, and have the stream plugin on (`config/enabled_plugins`). Each node's data is in a
volume of its own. `hydo compose` imported it, and `hydo/hydo.yaml` changes the import in these
ways:

- each node's `/var/lib/rabbitmq` is a file system of its own (`storage:`), the target of `L=`
  and `G=`;
- every node's port is 5672; the importer took 4369 (epmd) from the image's `EXPOSE` for
  `rabbit2` and `rabbit3`;
- a node is ready when its Erlang distribution port (25672) answers. A node that restarts with
  data opens its AMQP port only after its peers answer, and hydo restarts the members of a killed
  group one at a time, each after the one before is ready;
- the three nodes start together (`start_after: started`), as Compose starts them;
- the Erlang distribution's `net_ticktime` is the build parameter `ticktime`, 15 s instead of
  60 s, so that a pause of a node past it fits in the workload;
- `consumer_timeout` is 30 s instead of 30 minutes, so that the drain outlasts it;
- the replica count of every queue and of the stream is the build parameter `replicas` (3), for
  `hydo explore --configs`.

RabbitMQ 4.3 removed the partition handling strategies (`pause_minority`, `autoheal`) with
Mnesia: every replicated part is Raft-based now (the partitions guide). So the example has no
partition handling setting to explore; partitions are drawn by `--cluster-faults`.

## The workload

The test client `rmqc` (`client/main.go`, `build.sh`) declares the queues in `provision:`, once
every node takes AMQP connections, and checks that every node names the stream's leader and its
two replicas. A queue declared before a node joined the cluster gets no replica on it:

- `j`, a quorum queue with `x-delivery-limit` 2 (the build parameter `limit`), dead-lettering at
  least once into `jd`, and `x-overflow: reject-publish`, which that strategy needs;
- `jd`, the dead-letter queue, a quorum queue;
- `k`, a quorum queue with a single active consumer;
- `s`, a stream of three replicas with 1 MiB segments.

The test composer runs 30 rounds of 1 s with up to 2 of these commands each:

- `parallel_driver_pub`: one persistent publish to `j` with a publisher confirm, through the node
  that the command's number picks. One in five messages is a poison message (`x<id>`).
- `parallel_driver_consume`: one consumer of `j` with a prefetch of 1. It holds a message 1 s and
  acknowledges it. `basic.ack` has no answer, so a passive declare on the same channel after it
  must come back before the client records the acknowledgement. A consumer crashes on a poison
  message, so that RabbitMQ returns it and counts a failed delivery. A consumer that gets nothing
  in 3 s cancels itself and gives a late message back with a requeue, which RabbitMQ does not
  count.
- `parallel_driver_kpub` and `parallel_driver_sac`: keyed publishes to `k`, and consumers of `k`
  that each consume for 3 s; only the active one receives.
- `parallel_driver_spub`: one stream publish with a producer of its own name and publishing id 1.
  Without a confirm it sends again with a new producer of the same name, which the server must
  deduplicate.
- `serial_driver_sread`: the consumer `r` reads up to 4 messages from its stored offset, then
  stores the offset of the last one and reads it back until the server answers it.
- `eventually_probe`: a publish to `j` and one to `s` are confirmed again within 60 s after the
  faults. It runs after the drain, so its messages are not recorded; the drain starts with one
  recorded publish to `j` and one to `s`, also in the recovery boot after a power cut.

## The rules

Three `type: messages` checks, one history each (`hydo guide messages`):

- `jobs` (history `q`): `delivery: at-least-once`, `lease: {ms: 30000}`, `dead_letter: {after:
  3, to: jd}` and `acked_outcome` with `effect: recorded`. The drain (`rmqc drain`, the check's
  `final:`) takes every message left of `j` and of `jd` with a quiet window of 40 s, longer than
  `consumer_timeout`. Then it writes one status record per published id: `s=ok` and `f=1` when a
  consumer acknowledged it or the drain took it from `jd`.
- `order` (history `o`): `delivery: at-least-once` and `order: {kind: fifo, scope: key}`.
- `stream` (history `s`): `delivery: at-least-once`, `duplicates: forbidden`, `order: {kind:
  log}`, `after_ack: no-redelivery` for the reader `r`, and `fan_out: every-group` over `r` and
  the drain's group `all`, which reads the stream from its first offset.

One drain takes all five groups in parallel; the checks `order` and `stream` have the
placeholder `final: "true"`.

## Without faults, and the regressions

The final image `explore-ob-rabbitmq-disk.img` (`0f93b10463dfec99…`) gives `08ff275ad7f8aac4` on
two clean runs of `disk=none | run`. Each run publishes about 8 messages to `j`, 7 to `k` and 9
to the stream; every claim holds. Each saved regression gave one fingerprint over two fresh runs:

| regression | finding | fingerprint |
|---|---|---|
| `stream-message-lost-after-one-node-power-loss.schedule` | 1 | `46d06ab61b82f53e` |
| `stream-loses-confirmed-messages-at-power-cut.schedule` | 2 | `2d220bcd3276c1b7` |
| `stream-keeps-no-leader-after-backward-clock-step.schedule` | 3 | `efcd83d18848454a` |

`hydo repro --regressions` on the final image gave 3 of 3 as expected, with the same
fingerprints.

## What was found

### 1. New: a power loss of one node while a second node is away loses a confirmed message of the stream for good

**The schedule.** `disk=none dseed=0 | run K=rabbit2@1+3 N=nodes.bridge.6@21-23:loss,100%
P=rabbit1@16+11 W=5764a73c,p3,q2,r1000 G=rabbit3@20:flip,n=4,seed=4979bdc4 L=rabbit3@21+3`, seed
2 of the exploration e2. `rabbit2` is killed before step 1 and starts again before step 4.
`rabbit1` is paused from step 16 to step 27. That is 11 workload steps, about 100 s of guest time,
and longer than the net tick time of 15 s: `rabbit2` logged `node rabbit@rabbit1 down:
net_tick_timeout`. `rabbit3` loses its power before step 21 and starts again before step 24. The
publish of `s29` was confirmed while `rabbit1` was paused, and the drain's group `all`, which
reads the stream from its first offset, never received it: `VIOLATION [stream.no-loss] s29 was
acknowledged to process 29 at 84.029 s and never delivered in group all of channel s`. Every
quorum queue claim held in the same run.

**Separating the faults.** On e2's image (`explore-ob-rabbitmq-nocut-disk.img` of that round) the
schedule gave `83cc413c2d60d1ae` twice. Without `N=` and `G=` it failed too, and without `K=`,
without `P=` or without `L=` it passed. So the loss needs the kill, the pause and the power loss.
On the final image the window is narrower: the schedule fails twice with one fingerprint, and
each run without one of its five faults passes. `G=` flips a bit of a classic queue's transient
store, which the workload does not use, and with `N=` it moves the moment of the confirm. Why the
loss also needs the kill of `rabbit2` before step 1 was not traced.

**The replay with hydo 0.7.0.** The released hydo 0.7.0, from a fresh clone of `main` at
`1c318f23`, built the final image again on hvx with the same sha256 (`0f93b10463dfec99…`). The
schedule gave `VIOLATION [stream.no-loss] s29 was acknowledged to process 29 at 84.029 s and never
delivered ...` with `46d06ab61b82f53e` twice, the value in the table of the regressions. Without
`L=rabbit3@21+3` the run passed with `30301878046ba223`, and without `P=rabbit1@16+11` it passed
with `5f708bb378946198`. The run directory's `logs/` kept every node's log, also the log of each
node before its restart. The next paragraphs come from those logs and from the code.

**What happened.** RabbitMQ 4.4.1 runs osiris v1.14.1, and `main` of both has the same code:

1. `rabbit1` was the writer of epoch 1. During its pause the stream coordinator started epoch 2
   with `rabbit3` as the writer and `rabbit2` as its replica: `started writer ... on
   rabbit@rabbit3 in 2` at 00:01:05.88 of the guest's clock. It gave `rabbit1` the role
   `{replica,2}`, but no member ran on `rabbit1` in epoch 2 (`rabbit_stream_coordinator.erl` line
   2098).
2. `rabbit3` and `rabbit2` wrote `s29`, so the writer confirmed it: a writer confirms the middle
   tail of its members' sorted tails (`osiris_writer.erl` lines 321 and 653).
3. `rabbit3` lost its power at about 00:01:21. No member syncs a segment file: osiris has no
   `file:sync` call in `src/`, and osiris pull request 87 left fsync "to be implemented
   separately". So `rabbit3` lost its two chunks of epoch 2, and its tail on disk was `{1,4}`,
   offset 4 of epoch 1. `rabbit1` was still paused, so only `rabbit2` ran.
4. `rabbit1` came back at 00:02:33.66, and all three nodes ran before the election. The writer of
   epoch 1 on `rabbit1` wrote one more chunk, a tracking chunk at offset 5 in epoch 1. Then it
   stopped with the tail `{1,5}`.
5. The replica on `rabbit2` failed to start again (`writer_unavailable` at 00:02:33.088). The
   coordinator parked that action for about 6 s (`park_action`, lines 2392 to 2398), so
   `rabbit2` was not among the stopped members.
6. The coordinator elects a writer as soon as a quorum of the members has stopped (lines 1414,
   1744 to 1747 and 2056). It takes the newest tail among them (`select_leader/2`, lines 2534 to
   2548). `rabbit1` and `rabbit3` were that quorum, and `rabbit1` had the newer tail: `started
   writer ... on rabbit@rabbit1 in 3` at 00:02:33.706.
7. At 00:02:39 `rabbit2` started as a replica of epoch 3. The writer named offset 5 as epoch 1,
   but `rabbit2` held offset 5 in epoch 2. So `truncate_to` (`osiris_log.erl` lines 948 to 1022)
   deleted every segment of `rabbit2`, and `rabbit2` copied the log again from offset 0.

After it all three nodes report `{{0,7},[{1,5},{3,7}]}`: offsets 0 to 7, the last of epoch 1 at
offset 5 and the last of epoch 3 at offset 7. No node holds `s29`. On e2's image a held run
(`hydo inspect`) had shown the same end: epoch 3 with `rabbit1` as its writer, and all three
members at offset 10 without `s29`.

**Is it a bug?** The loss needs two conditions that RabbitMQ documents or accepts:

- Streams do not sync: "they do not explicitly flush (fsync) the data" (rabbitmq.com/docs/streams,
  "Data Safety"). An uncontrolled shutdown "could result in data loss for replicas hosted on that
  node".
- A second node was away during the power loss. While `rabbit3` had no power, `rabbit1` was down
  too, and only one of the three nodes ran. A maintainer's answer to discussion 18370 says that a
  power-off of two of three nodes is outside the stream's requirements. Here one node lost its
  power, and the other was paused.

The candidate is the part that no page documents. The coordinator elected from a quorum that left
out the member with the confirmed chunk, because that member waited on a retry timer. Then that
member deleted its copy. The documentation says that "the loss of data on a single node would
typically just be re-replicated from the other nodes". Here one node lost data, all three nodes
ran at the election, and the other node's copy was deleted, not copied back. A report would name
both the documented trade-off and this part.

**Duplicates.** No issue or discussion names this loss (the section "The duplicate searches").
These are the closest:

- rabbitmq-server issue 9457 (2023, closed): streams lose messages during a failover in which
  the leader's pod is deleted.
- osiris issue 145 (open): after an unclean shutdown that loses the page cache,
  `last_epochs_offsets` can name offsets whose data is missing.
- osiris pull request 87 (handle data loss in index and segment files) and issue 79 (a crash of
  the operating system can leave a log unusable).
- discussion 18370 (2026-11-16): a replica that never restarts after a power loss of two nodes,
  on a torn index record. osiris pull request 246 fixes it.
- rabbitmq-server issue 16822, a stream left stopped after two of three nodes are forgotten, and
  issue 9275 (open), a faster response of streams to network partitions.

**Native reproduction.** None: the window depends on the order of a pause, a confirm and a power
loss within seconds, which Docker cannot reproduce without a power cut of one container's page
cache.

### 2. Documented: a power cut of the whole guest loses confirmed messages of the stream

`disk=cut | run C=25` cuts the power of all three nodes before step 25. In the recovery boot the
stream lacks `s34`, which its publisher confirm acknowledged at 62.222 s: `VIOLATION
[stream.no-loss] s34 ... never delivered in group all`. `C=12` lost every message of the stream,
and the drain's recorded publish in the recovery boot took offset 0 again: `VIOLATION
[stream.order] s2 and sp30020 share offset 0`. The quorum queues `j`, `jd` and `k` lost nothing at
any cut. This is the documented trade-off of streams: they do not flush their data, and the
documentation advises quorum queues where more data safety is needed. A maintainer answered
discussion 18370 that powering off two of three nodes "explicitly violates" the requirement that
a quorum of the stream's nodes be available. The regression keeps it as a documented loss.

### 3. New, a clock effect: after a backward clock step, a kill of the stream's writer leaves the stream without a writer for the size of the step

**The schedule.** `disk=none dseed=0 | run K=nodes.primary@12+2 N=rabbit1-*@14-20:delay,402ms
P=rabbit3@2+3 T=7:-86400 W=a36fcf62,p3,q2,r2000 G=rabbit3@7+2:trunc,seed=8657d94c`, seed 10 of the
exploration e3. Before step 7 `rabbit3` is killed, one file of its quorum queues' WAL is cut, and
the wall clock steps back by a day; `rabbit3` starts again before step 9. Before step 12 the
stream's writer `rabbit1`, also the stream coordinator's leader, is killed; it is back before
step 14.

**What happens.** The stream coordinator's new leader on `rabbit2` logs `failed to stop member __s_…
rabbit@rabbit1 Error: {{nodedown,rabbit@rabbit1},…}`, and the replicas on `rabbit2` and `rabbit3`
fail to start again (`writer_unavailable`, `noproc`). Then no node logs anything of the stream
again: no new writer starts, also after `rabbit1` is back and all three nodes run. Every publish to
the stream answers `Leader not Ready yet` for the rest of the run and of the drain, and
`eventually_probe` fails: `VIOLATION [eventually_probe] ... no publish was confirmed within 60 s
after the faults`. The quorum queues work.

**Separating the faults.** On e3's image the schedule gave `33f9b1d69512cda5` twice. Without `T=`
it passed, without `K=` it passed, and without `G=`, whose kill restarts `rabbit3` after the
clock step, it passed. On the final image the same schedule passes; with the kill before step 13
instead of step 12 it fails, and that is the saved regression (`K=nodes.primary@13+2`). The kill
before step 10, 11 or 12 passes there, and so does the kill before step 13 with the clock step
before step 8. Seed 4 of e4 failed `eventually_probe` too, without a damaged file:
`K=rabbit1@21+9 P=rabbit1@8+1 T=10:-2592000`, a step back of 30 days. Its probe's output was not
read, and on the final image it passes.

**The replay with hydo 0.7.0.** The released hydo 0.7.0, from a fresh clone of `main` at
`4eb78297`, ran the saved regression on hvx on the final image (`0f93b10463dfec99…`). It gave
`VIOLATION [eventually_probe] ... no publish was confirmed within 60 s after the faults` with
`efcd83d18848454a` twice, the value in the table of the regressions. Without `T=7:-86400` the run
passed with `5c593130087afccd`, without `K=nodes.primary@13+2` with `056c67d4bc69ded8`, and
without `G=`, so without the restart of `rabbit3`, with `af49e5ec61e49f53`. Held runs (`hydo
inspect`) read each node's clocks and the coordinator's state at four points, and the run
directory's `logs/` kept every node's log. The next paragraphs come from them and from the code of
RabbitMQ 4.4.1, whose `rabbit_stream_coordinator.erl` is the same on `main`, and of Ra 3.2.0.

**What happened.**

1. Every node runs with `multi_time_warp`, as each node reports. In that mode the Erlang runtime
   keeps its system time when the OS clock steps. It moves its time offset only at its next check
   of the OS clock, every 15 s at first and every 60 s later (`erl_time_sup.c` lines 316 to 318
   and 497 to 505). 19 s after the step, `erlang:system_time` of `rabbit1` and `rabbit2` was
   still 86,400,543 ms ahead of `os:system_time`. `rabbit3`, which started again after the step,
   matched. 23 s after the step `rabbit2` matched too.
2. The kill came between those two points. `rabbit2` became the coordinator's leader in term 2.
   `rabbit1`'s member was parked as `{sleeping,nodeup}` (`backoff_class/1`, lines 1277 to 1282).
   `rabbit2`'s replica (`writer_unavailable`) was parked as `{sleeping,1795651263161}` with the
   target `stopped`, and `rabbit3`'s (`noproc`) as `{sleeping,1795651264168}`.
3. A parked retry time is the system time of the command plus 5,000 ms and a jitter of up to
   5,000 ms (`park_action/8` for `short`, lines 2392 to 2399, and the macros at lines 12 and 17 of
   `rabbit_stream_coordinator.hrl`). That system time is the timestamp that the Ra leader puts on
   each command with `erlang:system_time(millisecond)` (Ra's `ra_server_proc.erl` lines 2524 to
   2529, renamed to `system_time` in `ra_server.erl` lines 3443 to 3447). `rabbit2` stamped the
   commands before its check, so after the check both retry times were a day and a few seconds
   away.
4. The retry timer is a relative timeout of the retry time minus the system time
   (`retry_reconcile_delay/2`, lines 2348 to 2350), 5 to 10 s here. Ra arms it as a timeout of its
   process and turns it into a command (`ra_server_proc.erl` lines 1812 to 1813 and 628 to 629).
   It fired. Its `{timeout, retry_reconcile}` command carried `rabbit2`'s system time after the
   check. `expire_parked/4` found nothing due (lines 2440 to 2442), and `reconcile_retries/2`
   armed the timer again for about 86,400 s (lines 765 to 766 and 2403 to 2430).
5. When `rabbit1` came back, its `nodeup` woke its member, `{stopped,1,{1,1}}`. One stopped member
   of three is no quorum (`is_quorum/2`, lines 1412 to 1415, and the election at lines 1744 to
   1747), so no writer was elected. A member parked on a time is woken early only by
   `wake_timed_sleep` (lines 2605 to 2615). The coordinator calls it when a writer goes down or
   fails to start (lines 1797 and 1813), and with no writer neither can happen. `fail_action/3`
   leaves such a member alone (lines 2307 to 2311). `state_enter(leader)` and `tick/2` only arm
   the timer for the same time again (lines 784 to 806 and 820 to 831).
6. The coordinator's commit index stayed at 27 for the rest of the held run, about 7 minutes. At
   its end `rabbit2`'s retry was still 85,994,948 ms away. So the stream would wait about
   86,406.6 s, the size of the step and the retry delay.

**Without the restart.** In the run without `G=`, no node had started after the step. After the
kill `rabbit3` led the coordinator, `rabbit2`'s replica stopped cleanly, and only `rabbit3`'s
replica was parked. With `rabbit1` back, two members were stopped, a quorum, and the run ends in
epoch 2 with `rabbit3` as the writer and all three members running. So the restart changes which
node leads the coordinator and how many replicas are parked. The retry time a day ahead comes
from the step alone.

**The first version of this section was wrong** on two points. It said that Erlang starts
without a time warp, so that a node keeps its offset from its start. It also said that the wait
needs the coordinator's leader to move between nodes whose clocks differ. Both are false: every
node warps, and one leader stamped the retry times and judged them.

**Where the parking came from.** Pull request 16515 ("Stream coordinator: v8 optimisations and
bug fixes", merged on 2026-08-19, commits `c5a668ef1f`, `f712c3d228` and `fd80774359`) brought
the parked retries. It shipped in 4.4.0 (2026-10-19), whose notes say "Failed operations are now
retried after a short randomised delay". The first version of this section also named pull
request 17450, which has no part in it.

**Is it a bug?** It needs a step back of the wall clock, which NTP or an operator makes after a
clock ran fast. The step leaves the stream without a writer for the size of the step while all
three of its nodes run, although a stream needs only a quorum of them. The cause is the
coordinator's: a retry time from the system time of a command, judged later against a system
time that the runtime's correction moved back. It is a clock effect and a candidate.

**Native reproduction.** None: the cause is plain in the code and in the states above.

## The duplicate searches

Each phrase went to rabbitmq-server's issues and pull requests, its discussions, and osiris's
issues (`gh search issues --include-prs`, and the GraphQL search of discussions), on
2026-11-26.

| phrase | found |
|---|---|
| stream confirmed message lost; stream data loss power; publisher confirm stream lost | discussion 16472 (Raft and partitions, a question) |
| stream fsync; osiris fsync; stream not durable | discussion 17696 (quorum queue member placement) |
| stream replica truncate; stream replica power loss; unclean shutdown stream; stream coordinator epoch | discussion 18370 (a replica that never restarts after a power loss of two nodes, a torn index record; osiris pull request 246) |
| stream leader election data loss; stopped_in_epoch; select_leader | issue 16822 (a stream left stopped after two of three nodes are forgotten), 16871 (leadership on node drain), 3965 (closed in 2022: a wrong comparison in `select_leader`) |
| stream offset reused; stream offset tracking lost; stream restart node loses messages; single node failure stream | nothing |
| osiris_writer; osiris_replica truncate; stream quorum confirm | nothing new |
| Leader not Ready yet; failed to stop member; writer_unavailable | discussions 17703 and 17710 (classic queues on Kubernetes; quorum queues in their first 500 ms), issues 9159 and 5393 (closed in 2022 and 2023) |
| retry_reconcile; sleeping nodeup; stream coordinator nodeup; park action stream coordinator | pull request 16515 (the coordinator's machine version 8, 2026-08), issue 18475 (a member parked as `{sleeping, nodeup}` for a forgotten node, open) |
| (2026-11-27, after the trace of finding 3) the coordinator's system time, its retry time, Ra's command timestamp, time warp | pull requests 18314 and 18315 (the coordinator's determinism), Ra pull request 54 (2019, the command's timestamp); none names a retry time that a clock step moved |
| stream leaderless; stream coordinator stuck; stream leader not elected; stream stuck stopped; stream unavailable after kill; stream leader election hangs | nothing |

The release notes of 4.4.0 and 4.4.1 were read for streams. 4.4.0 changed the stream
coordinator's retries (pull request 16515): "actions that failed because a node was down are
retried when that node comes back". Its bug fixes name one with finding 3's symptom: "The
stream coordinator could leave a stream without a writer and never recover it" (16515). 4.4.1
names nothing of the stream coordinator, and nothing of a confirmed message lost.

## The explorations

All ran on hvx with one worker and `--no-shrink`, beside other sessions on the bare-metal
machine; each first run took 25 s to 2 minutes. Each failure was judged by directed runs instead
of the shrinker, because the stream's documented loss at a power cut would have taken every
shrink.

| exploration | image and options | schedules | failures by cause |
|---|---|---|---|
| e1 | the first image, `--seeds 1-20 --strategy process --cluster-faults --storage-faults` | 2, stopped | seed 1: finding 2 (a power cut); the client then changed |
| e2 | without power cuts (`explore-nocut.yaml`), the readiness on AMQP, the same options | 20 | finding 1 (seed 2); hydo's restart of a killed group one node at a time (seeds 5, 6, 18); detected damage of the metadata store, `names.dets` or the feature flags file (seeds 9, 16, 17) |
| e3 | without power cuts, the final client, `--seeds 1-40 --strategy process --new-faults --cluster-faults --storage-faults` | 15, stopped | finding 3 (seed 10); seed 4, a drain that did not end within 250 s after `T=10:-2592000` and a kill of `rabbit1`, probably finding 3 (not separated) |
| e4 | with power cuts and the stream check without `delivery` and `fan_out` (`explore-cutqq.yaml`), `--seeds 1-12 --strategy process` | 12 | probably finding 3 (seed 4: `eventually_probe` after `K=rabbit1@21+9 P=rabbit1@8+1 T=10:-2592000`, no damaged file, not separated); finding 2 at 3 of the 5 cuts: a new message at a lost one's offset (seeds 8, 12), and the reader's stored offset back before its acknowledgement (seed 5, `after_ack`) |
| e5 | the final image, `--reactive-at rmq.pub_confirmed@10,rmq.stream_confirmed@10 --react-delays 0..200/200 --react-actions kill-now,powerloss` | 24 | none: a kill of each node at the moment of a quorum queue's or the stream's confirm, and one node's power loss then, lost nothing; every reaction fired |
| cfg | `hydo explore --configs explore-nocut.yaml --seeds 1-6 --strategy process --storage-faults`, the setting `replicas` | 6 of `replicas=3`, 5 of `replicas=1` | finding 1 again, blind (seed 2, without `N=`: `K=rabbit2@1+3 P=rabbit1@16+11 ... G=rabbit3@20:flip,... L=rabbit3@21+3`), and the drain of seed 4 that did not end (`T=10:-2592000` and kills of `rabbit1`), as in e3's seed 4. With `replicas=1`, seed 1 lost `s1` at the power loss of `rabbit1` (`L=rabbit1@4+3`), the node of the stream's one replica: the setting's documented trade-off, as its `tradeoff:` note says; with three replicas seed 1 passed. Seed 4 lost `s16` the same way, and seed 5 failed `eventually_probe`, not separated. The exploration was stopped during its shrinks, so `configs.json` has no table |

No failure of any exploration named a claim of the quorum queues.

## What hydo lacked

In the order of the time each cost:

1. **A restart of several nodes one at a time.** After `K=nodes.all` hydo restarts the members
   one at a time and waits for each one's readiness. A RabbitMQ node that restarts with data
   opens its AMQP port only after its peers answer, so the first node never got ready, and 3 of
   e2's 20 schedules ended in `ERROR rabbit1 restart failed` (open item 147 has the same for
   TiKV's PD). The example's readiness is now the Erlang distribution port (25672), which opens
   before the node waits for its peers. A node that never serves is then caught by the drain and
   by `eventually_probe`, not by the readiness check.
2. **A messages check's `final:` runs before the `eventually_` commands.** The first probe after
   the faults was an `eventually_` command that published recorded messages, and every clean run
   then failed `no-loss`: the drain had ended before the probe published. `hydo guide checks`
   says that a `final_` check runs after the `eventually_` commands; a messages check's `final:`
   runs before them, and no guide says so. The drain now starts with the recorded probe.
3. **One drain for several histories.** Each history needs a check, and a check that claims
   `delivery` needs a `final:`. One drain takes all five groups in parallel (one quiet window
   instead of three), so the checks `order` and `stream` have the placeholder `final: "true"`.
4. **The tagged Go SDK has no `Drained`.** `hydo guide messages` shows `q.Drained(...)`, but
   `github.com/kakoc/hydo/sdk/go` v0.6.0, the newest tag, lacks it (it is on `main` since
   Milestone 363). The client writes the record with `History.Record`.
5. **An exploration cannot leave out one fault kind.** Every power cut of the whole guest loses
   what the stream had not synced, which RabbitMQ documents. An exploration with `cut` spends its
   failures on it. The example explores without cuts through a copy of `hydo.yaml`
   (`explore-nocut.yaml`) and judges the cuts with a copy whose stream check claims no delivery
   (`explore-cutqq.yaml`), as the SurrealDB and Dragonfly examples did (open items 140 and 141).
6. **Two images in one folder.** With the copies built, `hydo run "<schedule>"` refused: "there
   are 2 images ... pass one with `--image`". Every directed run names its image (open item
   188).
7. **The delivery limit counts what the client cannot see.** A consumer that gave up after 3 s
   closed its connection; a message that reached it just then came back as a failed delivery,
   which counts against `x-delivery-limit`. The checker saw fewer deliveries than RabbitMQ
   counted. The consumer now cancels itself and gives such a message back with a requeue, which
   RabbitMQ does not count. `hydo guide messages` does not say that a give-up must be visible
   to the system's own counter.
8. **The clock audit reads no Erlang.** `hydo rules scan --kind clock` read 9 files of RabbitMQ's
   sources and found nothing: Erlang is not one of its languages. The example's clock steps are
   the explorer's own draws.
9. **`T=<step>:+<secs>` is refused.** A forward step written with a plus sign stops with
   `"+120" is not a decimal number`; only `T=10:120` works.
10. **The importer.** The import ran with the 0.6.0 release's `hydo compose`, before this tree's
    programs were built. It took 4369 (epmd's port) from the image's `EXPOSE` for the nodes
    without `ports:`, as for Saleor and Hatchet; Milestone 367 warns of that now. It also dropped
    Compose's `hostname:` without a warning. RabbitMQ names its nodes after the host name, so the
    example sets `RABBITMQ_NODENAME`.
11. **`hydo explore --configs` read the copies' images first.** Before its first build it read
    every file under the example's `.hydo/` for 23 minutes: the unpacked roots and the disk
    images of the copies `explore-nocut.yaml` and `explore-cutqq.yaml`, which are not inputs of
    its images.
12. **hvx's console of 64 KiB.** The workload stays at about 60 commands per run, whose history
    records fill about half of it (open item 181).

## Time

| phase | wall time |
|---|---|
| scouting: the documentation, the source, the image, the cluster's formation | 30 minutes |
| setup to the first clean run, with the build of this tree's programs | 40 minutes |
| client changes and new images (the give-back, the probe, the declaration after every node joined) | about 2 hours |
| explorations and directed runs | about 3 hours, two VMs at most, beside other sessions |
| duplicate searches and the write-up | 1 hour |
