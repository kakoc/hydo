# Properties and measures of message-passing systems (design)

Status: proposal, nothing built. User request, 2026-11-08: "it's for testing a system which
passes messages, and we want to measure various characteristics of those messages." The user
builds an open-source message queue with an Amazon SQS-compatible API and wants to measure it,
and other queues, under the same faults. This note designs that as checks of the system's
**properties over a recorded history**, independent of any API, the way hydo's history checkers
check linearizability (`hydo/HISTORIES.md`). Ready-made clients for particular APIs come last and
only record histories in this model.

Scope: any system where a producer hands messages to the system and consumers get them: queues
(SQS and compatible servers, RabbitMQ queues), logs and streams (Kafka, NATS JetStream streams,
Redis streams), publish-subscribe and event buses, mailboxes. Sources for the semantics: Jepsen's
queue checkers and Kafka workload (`checker.clj` `queue`, `total-queue`, `unique-ids`;
`tests/kafka.clj`; docs/design/jepsen-gaps.md), Knossos's queue models, and the published work on
checking queue linearizability cited below.

## What is checked and what is measured

From one history per run hydo produces two things:

1. **Verdicts on declared properties.** The project declares which properties its system claims
   (a `checks:` entry of `type: messages`). Each property is a separate claim with a precise
   definition. A violated claim names the property and a small, sound witness: the operations
   that together prove the violation. A claim the history cannot decide (for example because a
   clock step fell inside a lease) is counted as undecided, never as a pass or a violation.
2. **Measures.** Counts and distributions that describe the system whether or not it claims
   anything: how many acknowledged messages were lost and which, the duplicate rate and the most
   copies of one message, redeliveries, reorders and the largest reorder distance, delivery
   latency in guest time, how long messages stayed leased, and how each of these changes under
   each fault. They go into the results, into the exploration's report, and into a comparison of
   several systems under the same schedules.

Everything is computed on the host after the run from the consoles, so no fingerprint and no
recorded value moves.

## The history

### Terms

- A **message** is identified by an id `m` that the producer chooses and that is unique per
  sending intent (the client puts it into the body). A producer that retries the same intent
  reuses `m`; a new intent gets a new `m`. hydo cannot check a system whose messages it cannot
  tell apart, so the recorders refuse to write a send of an id they already used for another
  intent (the same rule as list-append's unique elements).
- A **channel** `q` is where messages are sent: a queue, a topic, a stream, a subject.
- A **key** `k` is the ordering scope inside a channel: an SQS FIFO message group, a Kafka
  partition key, a JetStream subject, a RabbitMQ queue. `-` means none.
- A **group** `g` is a set of consumers that share the work: an SQS queue's consumers, a Kafka
  consumer group, a JetStream durable consumer, a RabbitMQ queue's consumers. Every group is meant
  to see every message of its channel (fan-out); inside a group each message is meant for one
  member. `-` means the channel's one group (a plain queue).
- A **consumer** is the record's process. A process has at most one operation in flight
  (HISTORIES.md), so consumers that work concurrently use different process ids.
- A **delivery** is one appearance of a message in a successful receive. Deliveries can carry
  what the system reports about them: its attempt count `n` (SQS `ApproximateReceiveCount`,
  JetStream `num_delivered`, RabbitMQ's `redelivered` flag as `n>=2`), its system id or offset
  `o` (SQS `MessageId`, a Kafka offset, a JetStream stream sequence), a handle `h` (SQS receipt
  handle, AMQP delivery tag, JetStream reply subject), and the lease `v` in milliseconds it was
  given (SQS visibility timeout, JetStream `AckWait`).

### Records

The record line is the existing one, `HYDO-HIST <history> <process> <type> <time> <f> <arg>...`,
with `<type>` one of `invoke`, `ok`, `fail`, `info` (HISTORIES.md "Record format"). This design
adds operations `f` and one item syntax:

- An **item** is one token: the message id followed by optional attributes,
  `m17;k=g2;x=d17;d=5000;o=81;n=2;h=a91f;v=30000;s=ok`. Attributes: `k` key, `x` deduplication id
  (default `m`), `d` requested delay in ms, `o` system id or offset, `n` attempt count, `h`
  handle, `v` lease in ms, `s` the item's own outcome inside a batch (`ok`, `fail`, `info`).
  Values are encoded with the recorders' `enc` (HISTORIES.md), which this design extends to `;`
  and `=`.

| operation | invoke | completion | meaning |
|---|---|---|---|
| send | `send <q> <item>...` | `send <q> <item>...` with `o` and, for a batch, `s` per item | hand messages to the system |
| recv | `recv <q> <g>` | `recv <q> <g> <item>...` (none: an empty receive) | a receive, poll or fetch; one item per delivered message |
| ack | `ack <q> <g> <item>...` | the same | acknowledge or delete the deliveries named (by `m`, and `h` where the API has handles) |
| nack | `nack <q> <g> <item>...` (`;r=0`: do not requeue) | the same | give deliveries back before their lease ends (SQS visibility 0, AMQP `basic.nack`, JetStream `Nak`); with `r=0` reject or terminate |
| extend | `extend <q> <g> <item;v=<ms>>...` | the same | extend a lease (SQS `ChangeMessageVisibility`, JetStream `InProgress`) |
| commit | `commit <q> <g> <key>@<offset>...` | the same | acknowledge every delivery of the key up to the offset (Kafka offset commits) |
| sub | `sub <q> <g> [from=start\|now]` | the same | join a group, create a subscription or a durable consumer |
| drained | (none) | `drained <q> <g>` | written by the drain (below) when it has seen the end of the group's messages |

Push delivery (a callback, an AMQP `basic.consume`, a JetStream push consumer, a WebSocket) has no
request: the callback writes `invoke recv` and `ok recv` back to back when it receives a message.

Example: process 3 sends two messages of group `g2` to the channel `orders`, process 5 receives
one and acknowledges it.

```
HYDO-HIST q 3 invoke 12000000000 send orders m17;k=g2 m18;k=g2
HYDO-HIST q 3 ok 12004000000 send orders m17;k=g2;o=81 m18;k=g2;o=82
HYDO-HIST q 5 invoke 12010000000 recv orders -
HYDO-HIST q 5 ok 12013000000 recv orders - m17;k=g2;o=81;n=1;h=a91f;v=30000
HYDO-HIST q 5 invoke 12500000000 ack orders - m17;h=a91f
HYDO-HIST q 5 ok 12502000000 ack orders - m17;h=a91f
```

### Order and time

**Order** is the records' position in the console, as for every hydo history: a completion
printed before an invocation really happened before it (HISTORIES.md "Ordering, power cuts,
determinism"). Real-time precedence `o1 ≺ o2` means `o1` completed (ok or fail) before `o2` was
invoked, by position. Positions also order the first boot before the recovery boot.

**Time** is the record's guest time. It is used only for durations (leases, delays,
deduplication windows, latencies), and only between two records of the same boot, because each
boot's clock starts again. The SDK recorders read `CLOCK_MONOTONIC`, which `T=` (a step of
`CLOCK_REALTIME`) does not move. Time is read before the request is sent (invoke) and after the
reply is read (completion), so the true interval lies inside the recorded one. The shell and Rust
recorders read `/proc/uptime`, which has a 10 ms resolution and rounds down; the checker adds the
recorder's resolution to completion times so that the bound stays sound (a record says which
recorder wrote it through its time's resolution; the shell's ends in `0000000`).

### Unknown outcomes

As in Jepsen and Elle, `info` (and an invocation left open at the end of a boot or at a power
cut) means the operation may have taken effect, at any time after its invocation, or never.
`fail` means it had no effect.

| operation | `info` is read as | consequence for the checks |
|---|---|---|
| send | the message may be in the system, accepted at any time after the invocation | it is not required to be delivered (no loss is claimed for it); a delivery of it is not a phantom (Jepsen's "recovered"); it precedes no later send (its acceptance time is unknown), but every send that completed before it was invoked precedes it |
| recv | the system may have delivered messages the client never saw, and started leases on them | a later delivery of such a message may be a redelivery; no lease or duplicate claim is made from the unseen delivery. With `ack: on-receive` (the system deletes on delivery) a message missing at the end is excused as "possibly consumed by an unknown receive" when such a receive of its group was invoked after the message's send was invoked |
| ack, commit | the message may or may not be acknowledged | a later delivery is not a redelivery after acknowledgement; the message counts as possibly processed |
| nack | the lease may have been given back | a redelivery after its invocation is not early |
| extend | the lease may or may not have been extended | the lease's lower bound is not extended |
| sub | the group may or may not exist from that point | the group's fan-out duty starts at the first completed `sub` or `recv` |

A batch whose request was answered has per-item outcomes (`s=`): SQS batch calls, for example,
answer each entry separately. A batch whose request was not answered is `info` for every item.

### What a drain is

"Every acknowledged message comes out" holds only if someone reads everything at the end, as
Jepsen's `total-queue` documentation says. The check therefore has a **drain**: a command the
project names (the check's `final:`), run after the heal (and in the recovery boot after a power
cut, as the other checks' read-backs are). It receives from every group the check covers until
the group is empty, acknowledges what it receives, records everything, and writes `drained <q>
<g>` when it believes the group is empty (several empty receives in a row with long polling, or
the end offset reached on a log). A group without `drained` makes the loss claims undecided and
the check an ERROR (as a failed `final:` does today: HYDO-CHECK-FINAL).

### Where the records go

The console is the only channel today that reaches the host at the moment a record is written,
which is what makes records survive a power cut. Its size limits the history: 64 KiB per VM on
hvx (the module's buffer), 4 MiB on the KVM engine and on the Mac (HISTORIES.md). A message's life
is six records (send, receive and acknowledgement, each invoked and completed) in which its item
appears five times. With batches of ten that is about 100 bytes per message, so an hvx run can
record about 600 messages next to its other output, and a KVM or Mac run about 40,000.

- **First:** the console, compact items, and drivers that batch. Messaging bugs show with
  hundreds of messages; the KVM engine and the Mac are the first targets.
- **Later:** a history channel through the agent. The recorder writes each record to the agent
  over a socket and waits for the agent's answer; the agent forwards the record to the host (on
  hvx over the host-call port, which has no 64 KiB limit) before answering. The record is then on
  the host before the client continues, as with the console. This is an agent protocol addition
  and a new agent pin, which moves every image's bytes (a re-baseline, AGENTS.md), so it waits for
  a release that re-baselines anyway. The agent's existing `tail` is not enough: it reads a file
  asynchronously, so records written just before a power cut could be lost, and a lost receive or
  acknowledgement record would make a delivered message look lost.

## Declaring the checks

A `checks:` entry of a new type `messages` (an addition to format 1: a new value of `type`, new
keys under it; older hydo versions refuse it, as they refuse every unknown key):

```yaml
checks:
  - id: orders.queue
    type: messages
    history: q                       # the HYDO-HIST history name
    final: [/bin/orders-client, drain]   # the drain
    ack: explicit                    # explicit (default) | on-receive
    claims:                          # each line is one claim; see "The properties"
      delivery: at-least-once        # at-most-once | at-least-once | exactly-once
      after_ack: no-redelivery
      processing: once
      lease: {ms: 30000}             # or each delivery's v=
      order: {kind: fifo-blocking, scope: key}
      dedup: {window_ms: 300000}
      dead_letter: {after: 3, to: orders-dlq}
      delay: true
      groups: fan-out
      linearizable: fifo             # fifo | unordered
    retention_ms: 345600000          # optional: a message older than this may be gone
    clock_steps: undecided           # undecided (default) | strict
    msg: orders are delivered as the queue's documentation promises
```

- `claims:` may name a **preset** instead of a mapping, or a preset plus changes:
  `claims: sqs-fifo`, or `claims: {preset: sqs-standard, lease: {ms: 60000}}`. The presets write
  down an API's documented semantics (below), so that several servers of one API can be measured
  against the same promises.
- With no `claims:` the check claims nothing and only measures: every quantity is reported and
  the verdict is OK. This is the mode for "measure the characteristics" of a system that makes no
  promise.
- In a test-composer project the same entry goes into `test.checks` with `kind: messages`; an
  executable `hydo/checks/messages_<name>` is then the drain (as `linearizable_<name>` is the
  read-back today), and the claims stay in the entry.

The guest prints the declaration as today (`HYDO-CHECK {"id":"orders.queue","type":"messages",
...}`), so a saved console describes itself. Only manifests with a `messages` check get anything
new in their image (the declaration in the plan); every other image builds the same bytes.

## The properties

Each claim is checked separately and reported as its own property, `<check id>.<claim>`
(`orders.queue.no-loss`). The common delivery words are combinations:

| word | claims |
|---|---|
| at-most-once delivery | `at-most-once` |
| at-least-once delivery | `no-loss` |
| exactly-once delivery | `no-loss` and `at-most-once` |
| effectively-once processing | `no-loss` and `processing: once` (each message's processing is accepted once, even when it is delivered again) |

`no-phantom` is always checked: no system may deliver what nobody sent.

Below, "acknowledged message" means a message with a send item whose outcome is `ok`. "A group
that must receive m" means every group of m's channel whose first completed `sub` (with
`from=now`) or `recv` precedes m's send invocation, and every group subscribed `from=start`.

### no-loss

**Claim.** Every acknowledged message is delivered at least once to every group that must
receive it and that was drained.

**Excused** (counted, never a violation): the message was moved to the dead-letter channel and
delivered there (`dead_letter:`); it expired (`retention_ms`, when the drain began more than that
after the send completed, in the same boot); it was possibly consumed by an unknown receive
(`ack: on-receive` only, above). Across a power cut an expiry cannot be measured, so a missing
message whose retention could have run out during the off time is undecided.

**What the APIs allow.** At-least-once systems (SQS standard and FIFO, JetStream with explicit
acks, RabbitMQ with publisher confirms and manual acks, Kafka with `acks=all` and a
non-auto-commit consumer) promise this. A system that acknowledges before its data is durable
promises it only without power loss; the claim then holds without `C=` and `L=` and fails with
them, which is the finding (NATS's default `sync_interval`, Jepsen's NATS analysis 3.4,
M141).

**Witness.** The message, its acknowledged send (process, position, guest time, `o`), the group,
the drain's `drained` record, and the faults active between the send and the drain (from the
control transcript). On a log with offsets the witness also names a delivered message of the same
key with a higher offset, which proves a gap rather than a slow consumer (Jepsen's `lost-write`
rule in `tests/kafka.clj`).

### no-phantom

**Claim.** Every delivered message was sent: some send of it was invoked, and not every send of
it failed. A delivery of a message no one sent is `unexpected` (Jepsen's `total-queue`); a
delivery of a message whose every send failed is an `aborted delivery` (Elle's G1a for messages).

**Witness.** The delivery and the failed sends, or the delivery alone.

### at-most-once

**Claim.** Each message is delivered at most once to each group.

**Witness.** Two deliveries of one message to one group.

### after_ack: no-redelivery

**Claim.** Once an acknowledgement of a message's delivery completed `ok` in a group, no receive
in that group invoked after it returns the message.

**Excused:** a send of the same message (the producer retried) whose invocation was after the
acknowledgement, or whose outcome is `info` (its acceptance can be late), unless the redelivery
carries the same system id `o` as the acknowledged delivery: then it is surely the same stored
message.

**What the APIs allow.** Not every API promises this. SQS's documentation for `DeleteMessage`
says that a standard queue may on rare occasions return a message after it was deleted, and that
a delete with an older receipt handle can succeed without deleting the message. So the
`sqs-standard` preset does not claim it, and the measure "redelivered after an acknowledgement"
still counts it.

**Witness.** The delivery, its `ok` acknowledgement, and the later receive.

### processing: once

**Claim.** For each message and group, at most one acknowledgement completes `ok` across all its
deliveries, and every acknowledged message of a drained group has at least one acknowledgement
(`ok` or `info`). This is effectively-once processing as a consumer sees it: when two consumers
both finished a message and the system accepted both acknowledgements, the work was done twice.
A system that fences stale handles (refuses an acknowledgement by the holder of an expired lease)
keeps it; a system that accepts any handle does not.

**Witness.** Two deliveries of the message, each with its `ok` acknowledgement, and their leases.

### lease

**Claim.** While a delivery's lease holds, the message is not delivered again in that group (to
any consumer, the same one included). A lease ends at its length after the delivery, later when
it was extended, and earlier when the holder gave it back (acknowledgement or nack).

**Definition on the history.** For a delivery `d1` by receive `r1`, the system started the lease
at some time `s1` within `[t_inv(r1), t_cmp(r1)]`. A lower bound of its end is
`L(d1) = t_inv(r1) + v`, raised to `t_inv(e) + v(e)` for every extension `e` of `d1` that
completed `ok` before the current bound (so the system surely accepted it while the lease held).
The lease surely held until `min(L(d1), R(d1))`, where `R(d1)` is the earliest invocation of an
acknowledgement or nack of `d1` that did not fail. A second delivery `d2` of the same message in
the same group by receive `r2` violates the claim when `r2` surely took the message before that:
when `r1 ≺ r2` and `t_cmp(r2) < min(L(d1), R(d1))`, or, for receives that overlap in time, when
the delivery cannot be ordered either way without breaking the bound (both
`t_cmp(r2) − t_inv(r1)` and `t_cmp(r1) − t_inv(r2)` are below the lease length).

**What the APIs allow.** SQS FIFO, JetStream (`AckWait`) and most leasing queues promise it. SQS
standard queues say a message can occasionally be delivered more than once, so the
`sqs-standard` preset does not claim it.

**Time dependence.** See "Time-dependent claims".

**Witness.** "m41 was delivered to process 7 at 12.410 s while process 3's lease from 12.002 s held
until at least 42.002 s (lease 30 s, no extension, no acknowledgement or nack before 12.410 s)",
with the three records.

### order

Precedence between messages of one scope (`scope: key`, the default, or `scope: channel`): `m1 →
m2` when a send item of `m1` completed `ok` before a send of `m2` was invoked (`ok` or `info`),
or `m1` comes before `m2` in one batch (option `batch_order: true`, as SQS FIFO keeps the order of
a batch's entries). Real-time precedence covers a single producer's order, because a process has
one operation in flight. Kinds:

- **`fifo`.** In each group, for `m1 → m2`, the first delivery of `m2` does not happen before the
  first delivery of `m1`: a violation is a first delivery of `m2` whose receive completed before
  the receive of `m1`'s first delivery was invoked, or that comes earlier in the same receive.
  Redeliveries are out of order by nature and are not compared.
- **`fifo-blocking`.** As `fifo`, and while `m1` is delivered and not acknowledged, no later
  message of its scope is delivered: each delivery of `m2` comes after an acknowledgement of `m1`
  was invoked (not failed), or after `m1` was moved to the dead-letter channel. This is SQS FIFO's
  message-group semantics.
- **`log`.** For logs with offsets: two messages never share an offset of a key, one message
  never has two offsets (both: Jepsen's `inconsistent-offsets` and `duplicate`); a consumer's
  successive receives of a key go up in offset (`nonmonotonic`) and skip no offset that holds a
  message (`skip`), except across a group rebalance the client records (`sub`).
- **`causal`.** For fan-out groups with one member each: precedence also includes "received, then
  sent" (a process completed a receive of `m1` and then invoked the send of `m2`), closed
  transitively, and in every group that delivers `m2`, `m1` was delivered first.

**Witness.** The two messages, the sends that order them, and the deliveries that contradict it.

### dedup

**Claim.** Two sends with the same deduplication id `x` within the window produce one stored
message: their deliveries and acknowledged sends carry at most one system id `o`.

**Definition.** For sends `a` and `b` of `x` (each `ok` or `info`), if
`t_cmp(b) − t_inv(a) < window` (the second surely reached the system within the window of the
first's acceptance) and both have system ids that differ (from the send acknowledgements or from
deliveries), the claim is violated. Without system ids in the records the claim is undecided:
only the system's own ids can tell a second stored copy from a redelivery.

**What the APIs allow.** SQS FIFO (a 5-minute interval), JetStream (`Nats-Msg-Id` and the stream's
duplicate window), Kafka's idempotent producer (per producer session, no time window: claim it
with `window_ms: 0`, meaning "always").

### dead_letter

**Claim** (`after: N`, optionally `to: <channel>`). A message delivered N times without an
acknowledgement is not delivered from its channel again. With `to:` it also appears in the
dead-letter channel (after the drain of that channel), and never in both after its move; without
`to:` (JetStream's maximum deliveries, which stop redelivery without moving the message) it is
excused from `no-loss`. The system's attempt counts `n`, where recorded, are at least the
deliveries observed so far and never decrease.

**Witness.** The deliveries, their counts, and the extra delivery or the missing dead letter.

### delay

**Claim.** A message sent with a delay `d` (its item's `d=`, or the channel's declared delay) is
not delivered before `t_inv(send) + d`: a violation is a delivery whose receive completed earlier.

### groups

**Claims.** `fan-out`: `no-loss` per group, for every group that must receive the message (a
group that joined late is not required to see earlier messages unless it subscribed
`from=start`). `one-member`: inside a group, deliveries to two different members obey `lease`
(for leasing systems) or, for systems without leases (Kafka consumer groups), are counted as
`group-overlap` in the measures and claimed only with `processing: once`.

### linearizable

**Claim.** The sends, receives and empty receives can be ordered, consistently with real time, as
the operations of one queue: `unordered` (a multiset: a receive returns any present message, as
Knossos's `unordered-queue` model) or `fifo` (Knossos's `fifo-queue`). Only for systems that
deliver each message once and acknowledge on receipt (a queue on a consensus log, a Redis list
used as a queue); with leases and redeliveries the claim does not apply and is refused.

**Complexity.** Checking linearizability is NP-complete in general (Gibbons and Korach, "Testing
shared memories", 1997), and Jepsen's own checkers do not attempt it for queues: `queue` reduces
the history with every non-failed send assumed accepted (O(n), it finds receives from nowhere),
and `total-queue` compares multisets. When every value is sent once, queue linearizability
reduces to the absence of a small set of patterns (Bouajjani, Emmi, Enea and Hamza, "On reducing
linearizability to state reachability", ICALP 2015; Emmi and Enea, "Sound, complete, and
tractable linearizability monitoring for concurrent collections", POPL 2018, which gives
polynomial-time monitors). In words: a receive of a value never sent or sent only after it
returned; a value received twice; two values whose sends are ordered and whose receives are
ordered the other way; and an empty receive while some value was surely in the queue for its
whole duration. hydo would implement the patterns as those papers define them (not from this
summary), each in O(n log n) with sorted intervals, report the first one found as the witness,
and confirm the witness with the WGL search hydo-history already has (on the witness's few
operations a queue model's search is small). Messages are unique by construction, which is the
condition the reduction needs.

## Time-dependent claims

`lease`, `delay`, `dedup` and the expiry excuse of `no-loss` compare durations. They are defined
on guest time, which under hydo is deterministic, so a reproduced run gets the same answer.

| situation | effect on the time-dependent claims |
|---|---|
| no clock fault, one boot | decided as defined above |
| `T=` (a step of the guest's `CLOCK_REALTIME`) | the records' times do not move (`CLOCK_MONOTONIC`). A system that measures its leases, delays or windows on the monotonic clock is unaffected. A system that stores them as wall-clock times (a "visible at" time stamp, a retention by append time) sees the step: a forward step ends its leases early in guest time. With `clock_steps: undecided` (the default) every pair whose interval contains the step's guest time is counted as undecided and not judged; with `clock_steps: strict` such pairs are judged, which reports a system whose leases depend on the wall clock. The step's guest time comes from the controller's `fault.clock_*` record |
| a power cut (`C=`, `cf=`) | guest time starts again in the recovery boot, so a pair with one record in each boot has no duration: it is undecided. For `lease` this loses nothing: the holder of a first-boot lease is gone, so a recovery-boot delivery cannot overlap its work. `delay`, `dedup` and the expiry excuse across the cut are undecided and counted |
| `P=` of the system | not a clock fault: the system's clock keeps running while it is stopped, and on resume its leases may all expire at once. That is allowed (the lease ran out); a holder that keeps working past its lease loses it legally |
| `K=` of the system | a restarted system that kept its leases only in memory may deliver a message again while its first holder still works on it: a `lease` violation, and a real finding for a system that claims leases across restarts |
| `L=`, `G=` of the system | as `K=` for time; their effect on data is judged by `no-loss`, `after_ack` and `dedup` |

Order claims (`fifo`, `fifo-blocking`, `log`, `causal`) and `no-loss`, `no-phantom`,
`at-most-once`, `after_ack`, `processing` use positions only and are decided across clock steps
and power cuts.

A claim whose candidate pairs were all undecided, or that had nothing to judge (no message was
acknowledged), passes vacuously: the run warns, as M184's `vacuous_passes` does.

## Measures

Computed for every `messages` check, whether or not it claims anything. Every time is guest time
in milliseconds, within one boot; pairs across a power cut are left out and counted.

| measure | definition |
|---|---|
| `sends` | send items invoked, `ok`, `fail`, `info`; `info` items later delivered (`recovered`) |
| `lost` | acknowledged messages not delivered to a group that had to receive them and was drained; the count per group and the first 20 ids |
| `excused` | missing messages excused, by reason: `expired`, `dead-lettered`, `possibly-consumed` |
| `unexpected`, `aborted` | deliveries of messages never sent, or whose every send failed |
| `deliveries` | deliveries in all; messages delivered at least once; the duplicate rate `(deliveries − messages delivered) / messages delivered`; the most deliveries of one message; a histogram of deliveries per message (1, 2, 3, 4 or more) |
| `redeliveries` | the extra deliveries by cause: `after-lease` (the earlier lease surely ended), `during-lease` (it surely held: a `lease` violation), `after-nack`, `after-ack`, `undecided` (no lease length known, or a clock step or cut in between) |
| `stored_copies` | messages with more than one system id `o` (only with `o` recorded) |
| `processing` | messages with more than one `ok` acknowledgement in a group |
| `reorders` | per scope and group: the number of inverted pairs (`m1 → m2` with `m2`'s first delivery before `m1`'s), and the largest reorder distance: for a message `m`, the number of messages that follow it by precedence and were first delivered before it; the maximum over messages |
| `latency.first_delivery` | from the completion of the send to the completion of the message's first delivery: count, p50, p95, p99, max |
| `latency.final_ack` | from the completion of the send to the completion of the first `ok` acknowledgement |
| `held` | per delivery that was acknowledged: from the completion of the receive to the acknowledgement's completion |
| `invisible` | per delivery that was not acknowledged and was delivered again: from the receive's completion to the next delivery's completion (how long the message stayed leased and out of sight), and its ratio to the lease length |
| `unseen` | at each workload step and at the end: the number of acknowledged messages not yet delivered to some group that must receive them (a backlog curve, as `unseen` in Jepsen's Kafka workload) |
| `availability` | per operation and per window: `ok`, `fail` and `info` counts |

**Windows within a run.** The host controller knows when every fault started and was healed (the
control transcript records the stops and the guest time). Each measure is also given per window:
`before` the first fault, `during:<fault>` for each fault token, `after` the heal, and `recovery`
(the recovery boot). A message belongs to the window of its send's completion; a delivery or
acknowledgement to the window of its own completion.

**Across an exploration.** Each run is classed by the kinds of fault its schedule holds (`none`,
`kill:<service>`, `pause:<service>`, `partition`, `net:<kind>`, `cut`, `powerloss:<service>`,
`corrupt:<service>`, `clock`, `disk:lying`, `disk:ioerr`; with node-set selectors of
docs/design/jepsen-gaps.md the class uses the role, `kill:nodes.primary`). For each class and
measure the exploration reports the number of runs, the median and the maximum, and the schedule
of the run with the maximum. For each claim it reports the share of runs that violated it and the
first minimal schedule. This answers "how does the duplicate rate change under a broker kill, and
under a partition".

## Results

Additions to results format version 1 (checked by `hydo-schema-compat`, readers of the released
schema; no existing field changes):

- `Run.messages`: one entry per `messages` check, with its `claims` (per claim: `held`,
  `violated` or `undecided`, the counts of judged and undecided pairs) and its `measures` (the
  table above, overall and per window).
- A violated claim is a finding of the existing kind `history-check`, with id `<check>.<claim>`
  and `details` carrying the witness: the records involved (process, operation, position, guest
  time, items), the faults active around them, and one sentence. The verdict line names it:
  `HYDO-ORACLE: VIOLATION [orders.queue.lease] m41 delivered to process 7 at 12.410 s while
  process 3's lease from 12.002 s held until at least 42.002 s`.
- `Exploration.messages`: the per-class table above.
- A new document type `comparison` (below).

The explorer's failure signature is the claim id, so it shrinks a lease violation to a minimal
schedule that still violates `lease`, and the report lists one minimal schedule per claim.

## Comparing systems

`hydo compare` runs the same schedules on several systems and reports, per system, which claims
held and what the measures were:

```sh
hydo compare --system plantq=img/plantq.img --system elasticmq=img/elasticmq.img \
  --system mine=img/mine.img --schedules schedules.txt [--claims sqs-fifo] --out cmp/
hydo compare ... --from hydo-out/explore     # the minimal and the sampled schedules of an exploration
```

It writes `comparison.json` (`"type": "comparison"`) and `comparison.md`: a table of claims by
system (held in how many schedules; the first violating schedule), and a table of measures by
system and fault class. `--claims` judges every system against the same claims (a preset, or one
system's), so that servers of one API are measured against the API's promises rather than their
own; without it each system's own declared claims are used.

What makes a schedule portable between systems:

- Fault targets by role, not by service name: `K=broker.one@5+2`, `N=nodes.halves.0@3-9:...`
  (the node-set and role selectors of docs/design/jepsen-gaps.md, ranked item 3). Each system's
  manifest maps the roles to its own services. Until those exist, the systems' manifests must use
  the same service names.
- The workload by command ids, not by seed alone: the resolved composition (`W=:...` with command
  ids, MANIFEST.md "Stable compositions") replays the same commands on each system when their
  commands have the same ids (`parallel_driver_send`, `parallel_driver_consume`). The ready-made
  drivers use the same ids for every API.
- Knobs are system-specific and left out; a schedule with a token a system's image lacks is
  refused before the run (M184's names check) and listed as "not applicable" for that system.

Fingerprints are per image, so a comparison compares verdicts and measures, never fingerprints.
Each system's runs still reproduce on their own.

## Presets of documented semantics

A preset names claims an API documents. They are written from the APIs' public documentation and
must be reviewed against it when they are built (this list is a starting point):

| preset | claims | measured only |
|---|---|---|
| `sqs-standard` | `no-loss`, `delay`, `dead_letter` when the queue has a redrive policy | duplicates, `after_ack`, `lease` (standard queues document occasional extra copies and redelivery after a delete) |
| `sqs-fifo` | `no-loss`, `order: {kind: fifo-blocking, scope: key}` with `batch_order`, `dedup: {window_ms: 300000}`, `lease`, `after_ack: no-redelivery`, `dead_letter` | |
| `jetstream-workqueue` | `no-loss`, `lease` (`AckWait`), `dedup` (the stream's duplicate window), `order: {kind: fifo, scope: key}` per subject, `dead_letter: {after: N}` (maximum deliveries, no move) | |
| `kafka-group` | `no-loss`, `order: {kind: log, scope: key}`, `dedup` with `window_ms: 0` for idempotent producers | group overlap during rebalances |
| `amqp-queue` | `no-loss` (with publisher confirms and manual acks), `order: fifo` per queue for first deliveries | redeliveries after a consumer's channel closes |

## Recorders

The SDKs' history recorders (HISTORIES.md "Recorders") already write any `f` with any arguments,
so a client can record this model today with the generic calls. Each SDK gains helpers that
format items and enforce the message-id rule. The Rust SDK is the reference (AGENTS.md "Language
policy"): its tests write the new shared cases in `sdk/cases/history`, and Go, Python and
JavaScript read them.

```rust
use hydo_assert::history::messages::{Messages, Item};
let q = Messages::new("q");
let op = q.send(3, "orders", &[Item::new("m17").key("g2"), Item::new("m18").key("g2")]);
match client.send_batch(...) {
    Ok(ids) => op.ok(&[Item::new("m17").offset(&ids[0]), Item::new("m18").offset(&ids[1])]),
    Err(e) if e.rejected() => op.fail(),
    Err(_) => op.info(),
}
let r = q.recv(5, "orders", "-");
r.ok(&got.iter().map(|d| Item::new(&d.id).attempt(d.count).handle(&d.receipt).lease(30_000)).collect::<Vec<_>>());
```

The shell gets `hydo_hist_msg` (an item builder) beside `hydo_hist`. The SDKs' files are inside
the recorded images of node-kv, python-kv and rust-kv, so the helpers ship in a release that
re-records those images anyway (as M163 did).

## Ready-made drivers (later, optional)

For systems whose API is common, hydo can write the client, as `hydo sql-gen` does for SQL
(docs/design/workloads.md): `hydo msg-gen --api sqs|kafka|jetstream|amqp --endpoint ... [--channel
...] [--group ...] [--fifo]` writes composer commands (`first_msg_init`, `parallel_driver_send`,
`parallel_driver_consume`, the drain `messages_<name>`) and the check entry with the API's preset.
The commands call one static guest program, `hydo-msg` (its own Cargo workspace like `hydo-sql`,
staged only when a command names it), which speaks the API, records in this model, and draws from
`HYDO_SEED`: batch sizes, keys, deduplication ids (sometimes a reused one, to exercise
deduplication), delays, and how long a consumer holds a message (sometimes longer than the lease,
so that leases expire and stale acknowledgements happen; sometimes with an extension or a nack).

| API | send | recv | ack, nack, extend | items |
|---|---|---|---|---|
| SQS (JSON 1.0 protocol, `X-Amz-Target: AmazonSQS.<Action>`, and the query protocol; SigV4 with fixed test credentials, reusing hydo-sql's `hmac` and `sha2`) | `SendMessage`, `SendMessageBatch` | `ReceiveMessage` (long polling) | `DeleteMessage(Batch)`, `ChangeMessageVisibility` (0 is a nack) | `o` MessageId, `k` MessageGroupId, `x` MessageDeduplicationId, `h` ReceiptHandle, `n` ApproximateReceiveCount, `d` DelaySeconds |
| Kafka (hydo-read's protocol code, with produce added) | `Produce` | `Fetch` | `OffsetCommit` (`commit`) | `o` offset, `k` partition key |
| NATS JetStream | `PUB` with `Nats-Msg-Id`, waiting for the stream's acknowledgement | pull `MSG.NEXT` | `+ACK`, `-NAK`, `+WPI`, `+TERM` | `o` stream sequence, `n` `num_delivered`, `x` `Nats-Msg-Id` |
| AMQP 0-9-1 | `basic.publish` with publisher confirms | `basic.get`, `basic.consume` | `basic.ack`, `basic.nack`, `basic.reject` | `h` delivery tag, `n>=2` from `redelivered` |

A system's own client (the user's queue, for example) does not need any of this: it records the
same lines through an SDK recorder.

## Which faults matter most

From the properties above and the bugs other analyses found, the faults to draw first for a
messaging system are:

- a kill or pause of the broker while deliveries are leased (`K=`, `P=` with timed forms): leases
  kept only in memory, redelivery storms, stale acknowledgements accepted;
- a power cut right after send acknowledgements (`C=`, `cf=<k>` at a flush): acknowledgement
  before durability;
- a power loss or corruption of one broker's storage (`L=`, `G=`): one replica's lost suffix,
  deletes that come back, deduplication tables lost;
- partitions between consumers and the broker, and between brokers (`N=`, `O=` on links; the
  shapes of jepsen-gaps.md): acknowledgements that do not arrive, leader changes;
- clock steps across lease lengths (`T=` with `clock_steps: strict`): leases measured on the wall
  clock.

The explorer needs no messaging-specific strategy: `--strategy process` with `--more-faults` and
`--storage-faults`, with the drivers' hold-time draws making leases expire inside the runs.

## Proof plan

Order, with rough costs (S days, M one to two weeks, L more):

1. **The checker on recorded histories** (M). `hydo-history/src/messages.rs`: the record and item
   parser, each claim, the measures; unit tests per claim with violating, passing and unknown-
   outcome histories, and recorded expected answers (`tests/golden/`); the queue patterns
   cross-checked against a WGL search on small random histories.
2. **The format and the plumbing** (M). `type: messages` in `hydo-manifest` (schema, examples,
   presets), the declaration in the guest's plan, `hydo check-history`, `hydo run` and the
   explorer reporting the findings, `Run.messages` and `Exploration.messages` in `hydo-results`,
   the tables in `report.md`. No image of a manifest without the new check changes; check with
   `hydo-bench rebaseline --check`.
3. **The recorders** (S to M). Rust helpers and shared cases, then Go, Python, JavaScript and the
   shell.
4. **A planted-bug example** (M): `hydo/onboard/examples/messages-planted/`, a small queue server
   written for the purpose (`plantq`, Rust, a few hundred lines: an append-only log with fsync,
   leases, groups, FIFO groups, a deduplication window, a dead-letter channel, delays) and a Rust
   client that records through the SDK. A flag plants one bug at a time, each meant to break
   exactly one claim: `lease-half` (leases end at half their length: `lease`), `ack-before-sync`
   (sends acknowledged before fsync: `no-loss` under `C=` and `L=`), `group-parallel` (a FIFO
   group not blocked while a message is in flight: `order`), `delete-in-memory` (acknowledgements
   not persisted: `after_ack` after `K=`), `dedup-forget` (the deduplication table not persisted:
   `dedup` after `K=`), `wallclock-lease` (leases on the wall clock: `lease` only with `T=` and
   `clock_steps: strict`). The proof: for each bug, `hydo explore` finds the violation of its
   claim and only that claim, shrinks it, and reproduces it twice with one fingerprint; the build
   without a bug passes the same seeds; the measures are plausible (the duplicate rate rises
   under broker kills, latency under pauses). On the KVM engine first (no module, 4 MiB console),
   then hvx with small message counts, then the Mac.
5. **A real system: NATS JetStream** (M). Apache-2.0, one static Go binary, official images for
   x86_64 and arm64, already onboarded (M141, `hydo/onboard/examples/nats-jetstream/`), and its
   published findings are expected answers: Jepsen's 3.1 (2.10.22 deletes the stream when every
   node stops at once; fixed in 2.10.23) must show as `no-loss` violations on 2.10.22 only, and
   3.4 (the default `sync_interval` loses acknowledged messages at a power cut) as `no-loss`
   under `C=` with the default and not with `sync_interval: always`. The example's Go client
   records the model (work-queue stream, pull consumer, `AckWait`, max deliveries, `Nats-Msg-Id`).
   New measures: duplicates and `invisible` times under kills, pauses and partitions of the
   stream's leader (needs the role selectors for "leader", or the leader found by hand).
6. **An SQS-compatible server and the comparison** (L, mostly the SQS driver). ElasticMQ first:
   Apache-2.0, maintained, covers standard and FIFO queues, visibility, delays and dead-letter
   redrive, and ships a native image (`softwaremill/elasticmq-native`; its architectures must be
   checked). goaws (MIT, one Go binary, smaller, in memory) second. LocalStack is not proposed: a
   large Python installation for one API. Then the user's own server, and `hydo compare` of the
   three with `--claims sqs-standard` and `sqs-fifo`. What to look for: in-memory servers lose
   every message at a broker kill or a power cut (no durability claim: the table shows it, it is
   not a bug); visibility leases after a broker restart; leases kept as wall-clock times
   (`T=` with `strict`); FIFO group blocking under concurrent consumers; the deduplication window
   across a restart; the dead-letter move under a kill in the middle of a redrive.

Steps 1 to 3 are needed before any example; step 4 proves the checker against known answers;
steps 5 and 6 can then run in parallel.

## Left open

- Whether the history channel through the agent (unlimited records on hvx) is wanted before the
  first example, or the KVM engine and the Mac are enough at first.
- Transactions across messages (Kafka transactions, consume-transform-produce): the model has
  sends and acknowledgements of one operation at a time; exactly-once across a read and a write
  needs a `txn` operation with send and commit micro-operations, as Jepsen's Kafka workload has,
  and Elle's cycle search over them.
- Retention as a claim (messages kept at least so long) rather than an excuse.
- Priority queues and message expiry per message (TTL): measured as loss today unless declared.

## Questions for the user

1. Which claims does your own server make: standard or FIFO semantics, a deduplication window,
   dead-letter redrive, durability across a crash and across a power loss, leases across a
   restart? The presets list SQS's; your server may promise more, and the comparison can judge
   every server against yours.
2. Which SQS protocol does your server speak (JSON 1.0, the query protocol, or both), and does it
   check SigV4 signatures?
3. Is ElasticMQ the right reference server, or is another SQS-compatible server more relevant to
   you?
4. Should a lease that ends early after a clock step count as a violation by default
   (`clock_steps: strict`), or stay undecided as proposed?
5. Is it acceptable to run the first examples on the KVM engine (no kernel module) and the Mac,
   where the console holds about 40,000 messages, and to add the history channel for hvx later?
