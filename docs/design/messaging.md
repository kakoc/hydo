# Properties and measures of message-passing systems (design)

Status: steps 1 to 3 of the proof plan are built (Milestone 196: the checker,
the declaration and its plumbing, the Rust recorder). `docs/HISTORY.md` Milestone 196 lists the
choices this text left open and how they were decided. User request, 2026-11-08: "it's for
testing a system which passes messages, and we want to measure various characteristics of those
messages."

Scope: any system where a producer hands messages to the system and consumers get them: queues,
logs and streams, publish-subscribe, event buses, mailboxes. The checks work on a **history**
that the system's own client records, the way hydo's history checkers check linearizability
(`hydo/HISTORIES.md`).

**The rule this design follows: hydo knows nothing about any particular system.** It does not
know a system's protocol, its API, or which other system it is compatible with. What a system
promises is declared by the project, in configuration: a `checks:` entry of `type: messages`
lists the claims and their parameters, and hydo checks exactly those claims and measures the
history. Bundles of declarations that describe a family of systems may exist, as files a project
includes and can replace; the checker never depends on one.

Sources for the semantics: Jepsen's queue checkers and Kafka workload (`checker.clj` `queue`,
`total-queue`, `unique-ids`; `tests/kafka.clj`; docs/design/jepsen-gaps.md), Knossos's queue
models, and the published work on checking queue linearizability cited below.

## What a run produces

From one history per run hydo produces two things:

1. **Verdicts on the declared claims.** Each claim is a separate property with a precise
   definition. A violated claim names the property and a small, sound witness: the operations
   that together prove the violation. A claim the history cannot decide (for example because a
   clock step fell inside a lease) is counted as undecided, never as a pass or a violation.
2. **Measures.** Counts and distributions that describe the system whether or not it claims
   anything: how many acknowledged messages were lost and which, the duplicate rate and the most
   copies of one message, redeliveries, reorders and the largest reorder distance, delivery
   latency in guest time, how long messages stayed leased, and how each of these changes under
   each fault. They go into the results, the exploration's report, and a comparison of several
   systems under the same schedules.

Everything is computed on the host after the run from the consoles, so no fingerprint and no
recorded value moves.

## The declaration

This is the centre of the design: a project says what its system promises, and the declaration
alone decides what hydo checks.

### Syntax

Every key except `id`, `type` and `history` is optional. Every default claims nothing, so an
entry with only those three keys measures the history and passes.

```yaml
checks:
  - id: <id>                        # the check's id; each claim is reported as <id>.<claim>
    type: messages
    history: <name>                 # the HYDO-HIST history the client records
    final: [<argv>]                 # the drain (below); needed by the claims that read the end
    include: [<file>, ...]          # bundles of declarations, merged in order (below)
    msg: <text>

    # how the system is used: facts that decide what counts as wrong
    acknowledgement: explicit       # explicit | on-receive | none
    retention_ms: <n>               # a message older than this may be gone (an excuse, not a claim)
    clock_steps: undecided          # undecided | strict  (time-dependent claims across a T= step)

    claims:
      delivery: none                # none | at-most-once | at-least-once | exactly-once
      duplicates: allowed           # allowed | forbidden | {by: dedup-id | message, window_ms: <n>}
      after_ack: allowed            # allowed | no-redelivery
      processing: any               # any | once
      lease: none                   # none | {ms: <n>} | {ms: per-delivery}
      order: none                   # none | fifo | fifo-blocking | log | causal
                                    #   or {kind: <one of those>, scope: key | channel,
                                    #       batch: ordered | unordered}
      dead_letter: none             # none | {after: <n>} | {after: <n>, to: <channel>}
      delay: none                   # none | per-message | {ms: <n>}
      fan_out: none                 # none | every-group
      members: any                  # any | exclusive
      linearizable: none            # none | fifo | unordered

    measures:                       # optional; the defaults are shown
      windows: true                 # also per fault window
      quantiles: [0.5, 0.95, 0.99, 1]
      channels: all                 # all | [<channel>, ...]
```

### What each key means

| key | values | meaning |
|---|---|---|
| `acknowledgement` | `explicit` (default) | consumers acknowledge each delivery; an unacknowledged delivery may come back |
| | `on-receive` | a delivery consumes the message (auto-acknowledgement); a receive whose answer was lost may have consumed messages |
| | `none` | consumers never acknowledge (a publish-subscribe system that pushes and forgets) |
| `retention_ms` | a number | a message may legitimately disappear this long after it was sent; a missing message that old is excused, not lost |
| `clock_steps` | `undecided` (default), `strict` | whether time-dependent claims judge pairs whose interval contains a `T=` step ("Time-dependent claims") |
| `claims.delivery` | `at-least-once` | every acknowledged message is delivered (`no-loss`) |
| | `at-most-once` | no message is delivered twice to one group |
| | `exactly-once` | both |
| `claims.duplicates` | `forbidden` | each message is stored once: all its deliveries and acknowledged sends carry one system id |
| | `{by, window_ms}` | sends with the same message id (`by: message`) or the same deduplication id (`by: dedup-id`) within the window are stored once; `window_ms: 0` means "always" |
| `claims.after_ack` | `no-redelivery` | after an acknowledgement completed, the message is not delivered again in that group |
| `claims.processing` | `once` | for each message and group, at most one acknowledgement is accepted (stale acknowledgements are refused) |
| `claims.lease` | `{ms: n}` | a delivered message is not delivered again in its group for n ms unless given back; `per-delivery` takes the length each delivery records (`v=`) |
| `claims.order` | `fifo` | first deliveries follow the order of the sends, per scope |
| | `fifo-blocking` | as `fifo`, and nothing later in the scope is delivered while an earlier message is out and unacknowledged |
| | `log` | offsets: one message per offset, one offset per message, each consumer's reads go up without gaps |
| | `causal` | a message sent after its producer received another is delivered after it, to every group |
| `claims.dead_letter` | `{after: n[, to: q]}` | after n deliveries without acknowledgement the message is not delivered again; with `to:` it appears in that channel instead |
| `claims.delay` | `per-message` | a message is not delivered before the delay its send recorded (`d=`) |
| | `{ms: n}` | the same with one delay for every message |
| `claims.fan_out` | `every-group` | every group that must receive a message receives it (`no-loss` per group) |
| `claims.members` | `exclusive` | inside a group, two members do not hold one message at once |
| `claims.linearizable` | `fifo`, `unordered` | the sends and receives are linearizable as one FIFO queue or one multiset |

### Examples

A work queue with leases, retries and a dead-letter channel:

```yaml
checks:
  - id: jobs
    type: messages
    history: jobs
    final: [/bin/jobs-client, drain]
    claims:
      delivery: at-least-once
      lease: {ms: 30000}
      after_ack: no-redelivery
      dead_letter: {after: 5, to: jobs-dead}
      delay: per-message
```

A log with keys and consumer groups:

```yaml
checks:
  - id: events
    type: messages
    history: ev
    final: [/bin/events-client, drain]
    retention_ms: 604800000
    claims:
      delivery: at-least-once
      duplicates: {by: message, window_ms: 0}     # the producer is idempotent
      order: {kind: log, scope: key}
      fan_out: every-group
```

Ordered message groups that block while a message is out, with a deduplication window:

```yaml
checks:
  - id: payments
    type: messages
    history: pay
    final: [/bin/pay-client, drain]
    claims:
      delivery: at-least-once
      duplicates: {by: dedup-id, window_ms: 300000}
      order: {kind: fifo-blocking, scope: key, batch: ordered}
      lease: {ms: per-delivery}
      after_ack: no-redelivery
      processing: once
```

A system that promises nothing yet: measure only.

```yaml
checks:
  - id: bus
    type: messages
    history: bus
    final: [/bin/bus-client, drain]
```

### Bundles: `include:`

A bundle is a YAML file in the project with the same keys as a check entry, except `id`, `type`,
`history` and `final` (a bundle cannot name what to read). `include:` merges the bundles in the
order listed, then the entry's own keys:

- a key a later source gives replaces the earlier one whole; inside `claims:` and `measures:` the
  rule applies per key, so an entry can change one claim of a bundle;
- the merged result must be a valid declaration on its own; a bundle path that does not exist is
  an error.

`hydo build` resolves the includes and puts the resolved declaration into the image's plan, so
the checker sees only resolved keys and values, and a bundle changed later does not change what
an existing image claims. `hydo build --check` prints the resolved declaration.

hydo's guide (`hydo guide messages`) shows example bundles named by behaviour, not by product
(`work-queue-with-leases.yaml`, `ordered-groups.yaml`, `keyed-log.yaml`, `fan-out.yaml`), as text
to copy into a project. hydo does not resolve bundle names and ships no bundle the checker
reads. A project, or a community around one API, may write bundles that describe that API's
documented promises; they are ordinary project files.

### Combinations refused before any run

`hydo build --check` refuses a declaration whose claims contradict the facts it states, naming
both keys:

| refused | why |
|---|---|
| `linearizable` without `acknowledgement: on-receive` | a queue with acknowledgements redelivers, so it is not one linearizable queue; the claim would always fail |
| `processing: once`, `after_ack`, `lease`, `dead_letter` or `order: fifo-blocking` with `acknowledgement: on-receive` or `none` | they are about acknowledgements |
| `delivery: at-most-once` or `exactly-once` with `lease` | a lease exists to allow redelivery; at-most-once forbids it |
| any claim that reads the end (`delivery` with loss, `fan_out`, `processing`, `dead_letter` with `to:`) without `final:` | "every message comes out" needs someone to read everything at the end |

Claims whose records may be missing are not refused at build time: `duplicates` needs system ids
(`o=`) and `order: log` needs offsets. A run without them reports the claim as undecided with the
reason, and warns.

### From a declaration to checks and measures

| declaration | properties reported | records it needs | measures it adds |
|---|---|---|---|
| (always) | `<id>.no-phantom` | sends and receives | every measure of "Measures" that needs no parameter |
| `delivery: at-least-once` | `<id>.no-loss` | sends, receives, `drained` | `lost`, `excused`, `unseen` |
| `delivery: at-most-once` | `<id>.at-most-once` | receives | |
| `delivery: exactly-once` | both | as both | |
| `duplicates` | `<id>.duplicates` | `o=` on send acknowledgements or deliveries; `x=` for `by: dedup-id` | `stored_copies` |
| `after_ack` | `<id>.after-ack` | acknowledgements | `redeliveries.after-ack` is measured always |
| `processing: once` | `<id>.processing` | acknowledgements | |
| `lease` | `<id>.lease` | receive times; `v=` for `per-delivery`; extensions, nacks | `redeliveries` split by lease; the `invisible` ratio to the lease |
| `order` | `<id>.order` | sends and receives; `k=` for `scope: key`; `o=` for `log` | `reorders` are measured always |
| `dead_letter` | `<id>.dead-letter` | receives with `n=` where the system reports it; the dead-letter channel's drain | dead-letter counts |
| `delay` | `<id>.delay` | `d=` on sends for `per-message` | delivery time minus delay |
| `fan_out: every-group` | `<id>.fan-out` | `sub` or the first receive of each group, `drained` per group | `lost` per group |
| `members: exclusive` | `<id>.members` | receive times, consumers | `group_overlap` |
| `linearizable` | `<id>.linearizable` | sends, receives, empty receives | |
| `retention_ms` | (an excuse in `no-loss`) | send times | `excused.expired` |
| `clock_steps` | (decides undecided pairs) | the controller's clock records | `undecided` counts |

### Format rules: additions only

- **The project format.** `type: messages` is a new value of `checks[].type` in format 1, and
  every key under it is new and optional. The project schema's readers are hydo itself
  (`hydo-schema-compat`, `Readers::Current`), so this widening passes; an older hydo refuses a
  manifest with the new type, naming it, which is the honest answer.
- **Defaults claim nothing.** A claim added later is a new optional key whose default is `none`,
  `allowed` or `any`, so an existing declaration means the same after the addition. A new value of
  an existing key (a new `order` kind) is an addition. The meaning and the default of an existing
  key or value never change, and no value is removed.
- **Unknown keys are refused,** as everywhere in format 1, so a misspelled claim is an error
  before the run, not a property silently left unchecked.
- **The declaration in the console.** The guest prints the resolved declaration in its
  `HYDO-CHECK` line, so a saved console describes itself. A hydo that reads a console whose
  declaration has a claim key it does not know reports the check as an ERROR naming the key,
  never as a pass.
- **Examples.** Each example above becomes a permanent example document with its model in
  `hydo/schema/examples/project/v1/`, as the format's other examples are.
- **The results format** gains only optional fields and one new document type ("Results").

## The history

### Terms

- A **message** has an id `m` the producer chooses, unique per sending intent (the client puts it
  into the body). A producer that retries the same intent reuses `m`. The recorders refuse a send
  of an id already used for another intent, as list-append requires unique elements.
- A **channel** `q` is where messages are sent.
- A **key** `k` is an ordering scope inside a channel; `-` means none.
- A **group** `g` is a set of consumers that share the work of a channel; every group is meant to
  see the channel's messages, and inside a group each message is meant for one member. `-` means
  the channel's one group.
- A **consumer** is the record's process. A process has at most one operation in flight
  (HISTORIES.md), so consumers that work concurrently use different process ids.
- A **delivery** is one appearance of a message in a successful receive. It may carry what the
  system reports about it: an attempt count `n` (a receive count or a redelivered flag as `n>=2`),
  a system id or offset `o`, a handle `h` (whatever the consumer must present to acknowledge), and
  the lease `v` in milliseconds the system says it gave.

### Records

The record line is the existing one, `HYDO-HIST <history> <process> <type> <time> <f> <arg>...`,
with `<type>` one of `invoke`, `ok`, `fail`, `info` (HISTORIES.md "Record format"). This design
adds operations and one item syntax.

An **item** is one token: the message id followed by optional attributes,
`m17;k=g2;x=d17;d=5000;o=81;n=2;h=a91f;v=30000;s=ok`. Attributes: `k` key, `x` deduplication id
(default `m`), `d` requested delay in ms, `o` system id or offset, `n` attempt count, `h` handle,
`v` lease in ms, `s` the item's own outcome inside a batch (`ok`, `fail`, `info`). Values are
encoded with the recorders' `enc` (HISTORIES.md), extended to `;` and `=`.

| operation | invoke | completion | meaning |
|---|---|---|---|
| send | `send <q> <item>...` | the items, with `o` and, for a batch, `s` per item | hand messages to the system |
| recv | `recv <q> <g>` | `recv <q> <g> <item>...` (none: an empty receive) | a receive, poll or fetch |
| ack | `ack <q> <g> <item>...` | the same | acknowledge the deliveries named (by `m`, and `h` where the system has handles) |
| nack | `nack <q> <g> <item>...` (`;r=0`: do not requeue) | the same | give deliveries back before their lease ends; with `r=0` reject them |
| extend | `extend <q> <g> <item;v=<ms>>...` | the same | extend a lease |
| commit | `commit <q> <g> <key>@<offset>...` | the same | acknowledge every delivery of the key up to the offset |
| sub | `sub <q> <g> [from=start\|now]` | the same | join a group or create a subscription |
| drained | (none) | `drained <q> <g>` | written by the drain when it has seen the end of the group's messages |

Push delivery (a callback) has no request: the callback writes `invoke recv` and `ok recv` back
to back when a message arrives.

Example: process 3 sends two messages of key `g2` to channel `orders`, process 5 receives one and
acknowledges it.

```
HYDO-HIST q 3 invoke 12000000000 send orders m17;k=g2 m18;k=g2
HYDO-HIST q 3 ok 12004000000 send orders m17;k=g2;o=81 m18;k=g2;o=82
HYDO-HIST q 5 invoke 12010000000 recv orders -
HYDO-HIST q 5 ok 12013000000 recv orders - m17;k=g2;o=81;n=1;h=a91f;v=30000
HYDO-HIST q 5 invoke 12500000000 ack orders - m17;h=a91f
HYDO-HIST q 5 ok 12502000000 ack orders - m17;h=a91f
```

### Order and time

**Order** is the records' position in the console: a completion printed before an invocation
really happened before it (HISTORIES.md "Ordering, power cuts, determinism"). Real-time
precedence `o1 ≺ o2` means `o1` completed (`ok` or `fail`) before `o2` was invoked, by position.
Positions also order the first boot before the recovery boot.

**Time** is the record's guest time, used only for durations (leases, delays, windows,
latencies) and only between two records of the same boot, because each boot's clock starts
again. The SDK recorders read `CLOCK_MONOTONIC`, which `T=` (a step of `CLOCK_REALTIME`) does not
move. Time is read before the request is sent and after the reply is read, so the true interval
lies inside the recorded one. The shell and Rust recorders read `/proc/uptime`, which has a 10 ms
resolution and rounds down; the checker adds the recorder's resolution to completion times so
that every bound stays sound.

### Unknown outcomes

As in Jepsen and Elle, `info` (and an invocation left open at the end of a boot or at a power
cut) means the operation may have taken effect, at any time after its invocation, or never.
`fail` means it had no effect.

| operation | `info` is read as | consequence |
|---|---|---|
| send | the message may be in the system, accepted at any time after the invocation | no loss is claimed for it; a delivery of it is not a phantom (Jepsen's "recovered"); it precedes no later send, but every send that completed before it was invoked precedes it |
| recv | the system may have delivered messages the client never saw, and started leases on them | a later delivery of such a message may be a redelivery; nothing is claimed from the unseen delivery. With `acknowledgement: on-receive` a message missing at the end is excused as "possibly consumed" when such a receive of its group was invoked after its send was invoked |
| ack, commit | the message may or may not be acknowledged | a later delivery is not a redelivery after acknowledgement; the message counts as possibly processed |
| nack | the lease may have been given back | a redelivery after its invocation is not early |
| extend | the lease may or may not be longer | the lease's lower bound is not extended |
| sub | the group may or may not exist from that point | the group's fan-out duty starts at its first completed `sub` or `recv` |

A batch whose request was answered has per-item outcomes (`s=`). A batch whose request was not
answered is `info` for every item.

### The drain

"Every acknowledged message comes out" holds only if someone reads everything at the end, as
Jepsen's `total-queue` documentation says. The drain is a command of the project (the check's
`final:`), run after the heal, and in the recovery boot after a power cut, as other checks'
read-backs are. It receives from every group until the group is empty, acknowledges what it
receives, records everything, and writes `drained <q> <g>` when it believes the group is empty.
How it decides that is the project's (several empty receives in a row, or the end offset
reached). A group the declaration needs without `drained` makes those claims undecided and the
check an ERROR, as a failed `final:` does today (HYDO-CHECK-FINAL).

### Where the records go

The console is the only channel today that reaches the host at the moment a record is written,
which is what makes records survive a power cut. Its size limits the history: 64 KiB per VM on
hvx (the module's buffer), 4 MiB on the KVM engine and on the Mac (HISTORIES.md). A message's life
is six records (send, receive and acknowledgement, each invoked and completed) in which its item
appears five times. With batches of ten that is about 100 bytes per message, so an hvx run can
record about 600 messages next to its other output, and a KVM or Mac run about 40,000.

- **First:** the console, compact items, clients that batch, and the KVM engine and the Mac as
  the first targets. Messaging bugs show with hundreds of messages.
- **Later:** a history channel through the agent. The recorder hands each record to the agent
  over a socket and waits for its answer; the agent forwards the record to the host (on hvx over
  the host-call port, which has no 64 KiB limit) before answering. The record is then on the host
  before the client continues, as with the console. It is an agent protocol addition and a new
  agent pin, which moves every image's bytes (a re-baseline, AGENTS.md), so it waits for a
  release that re-baselines anyway. The agent's existing `tail` is not enough: it reads a file
  asynchronously, so records written just before a power cut could be lost, and a lost receive or
  acknowledgement record would make a delivered message look lost.

## The properties

Each declared claim is checked separately. "Acknowledged message" means a message with a send
item whose outcome is `ok`. "A group that must receive m" means every group of m's channel whose
first completed `sub` (with `from=now`) or `recv` precedes m's send invocation, and every group
subscribed `from=start`.

### no-loss (`delivery: at-least-once`, `exactly-once`; per group with `fan_out`)

**Claim.** Every acknowledged message is delivered at least once to every group that must
receive it and that was drained.

**Excused** (counted, never a violation): moved to the dead-letter channel and delivered there
(`dead_letter` with `to:`); stopped after its last allowed delivery (`dead_letter` without
`to:`); expired (`retention_ms`, when the drain began more than that after the send completed,
in the same boot); possibly consumed by an unknown receive (`acknowledgement: on-receive`).
Across a power cut an expiry cannot be measured, so a missing message whose retention could have
run out during the off time is undecided.

A system that acknowledges a send before its data is durable keeps this claim without power loss
and breaks it with `C=` or `L=`; that is the finding.

**Witness.** The message, its acknowledged send (process, position, guest time, `o`), the group,
the drain's `drained` record, and the faults active between the send and the drain (from the
control transcript). With offsets the witness also names a delivered message of the same key
with a higher offset, which proves a gap rather than a slow consumer (Jepsen's `lost-write` rule
in `tests/kafka.clj`).

### no-phantom (always)

**Claim.** Every delivered message was sent: some send of it was invoked, and not every send of
it failed. A delivery of a message nobody sent is `unexpected` (Jepsen's `total-queue`); a
delivery of a message whose every send failed is an `aborted delivery` (Elle's G1a for messages).

### at-most-once (`delivery: at-most-once`, `exactly-once`)

**Claim.** Each message is delivered at most once to each group. **Witness:** two deliveries.

### duplicates

**Claim.** `forbidden`: all deliveries and acknowledged sends of a message carry one system id
`o`. `{by, window_ms}`: for two sends `a` and `b` with the same message id or deduplication id
(each `ok` or `info`), if `t_cmp(b) − t_inv(a) < window_ms` (the second surely reached the system
within the window of the first's acceptance; `0` means any time) and their system ids differ, the
claim is violated. Without system ids the claim is undecided: only the system's ids tell a second
stored copy from a redelivery. **Witness:** both sends and both ids.

### after-ack (`after_ack: no-redelivery`)

**Claim.** Once an acknowledgement of a delivery completed `ok` in a group, no receive in that
group invoked after it returns the message.

**Excused:** a send of the same message invoked after the acknowledgement, or with outcome `info`
(its acceptance can be late), unless the redelivery carries the acknowledged delivery's system id
`o`: then it is surely the same stored message. **Witness:** the delivery, its acknowledgement and
the later receive.

### processing (`processing: once`)

**Claim.** For each message and group, at most one acknowledgement completes `ok` across all its
deliveries, and every acknowledged message of a drained group has at least one acknowledgement
(`ok` or `info`). When two consumers both finished a message and the system accepted both
acknowledgements, the work was done twice; a system that refuses the acknowledgement of an
expired lease keeps the claim. **Witness:** two deliveries with their `ok` acknowledgements and
their leases.

### lease (`lease`)

**Claim.** While a delivery's lease holds, the message is not delivered again in that group, to
any consumer. A lease ends at its length after the delivery, later when extended, earlier when
given back.

**Definition.** For a delivery `d1` by receive `r1`, the system started the lease at some time in
`[t_inv(r1), t_cmp(r1)]`. A lower bound of its end is `L(d1) = t_inv(r1) + v`, raised to
`t_inv(e) + v(e)` for every extension `e` of `d1` that completed `ok` before the current bound
(the system surely accepted it while the lease held). The lease surely held until
`min(L(d1), R(d1))`, where `R(d1)` is the earliest invocation of an acknowledgement or nack of
`d1` that did not fail. A second delivery `d2` of the message in the group by receive `r2`
violates the claim when `r1 ≺ r2` and `t_cmp(r2) < min(L(d1), R(d1))`, or, for receives that
overlap in time, when neither order fits the bound (both `t_cmp(r2) − t_inv(r1)` and
`t_cmp(r1) − t_inv(r2)` are below the lease length).

**Witness.** "m41 was delivered to process 7 at 12.410 s while process 3's lease from 12.002 s
held until at least 42.002 s (lease 30 s, no extension, no acknowledgement or nack before
12.410 s)", with the three records.

### order (`order`)

Precedence between messages of one scope (`scope: key`, the default, or `channel`): `m1 → m2`
when a send item of `m1` completed `ok` before a send of `m2` was invoked (`ok` or `info`), or
`m1` comes before `m2` in one batch with `batch: ordered`. A single producer's order is covered,
because a process has one operation in flight.

- **`fifo`:** in each group, for `m1 → m2`, the first delivery of `m2` is not earlier than the
  first delivery of `m1`: a violation is a first delivery of `m2` whose receive completed before
  the receive of `m1`'s first delivery was invoked, or that comes earlier in the same receive.
  Redeliveries are not compared.
- **`fifo-blocking`:** as `fifo`, and each delivery of `m2` comes after an acknowledgement of `m1`
  was invoked (not failed), or after `m1` left for the dead-letter channel.
- **`log`:** two messages never share an offset of a key, one message never has two offsets (both
  as Jepsen's `inconsistent-offsets` and `duplicate`); a consumer's successive receives of a key
  go up in offset (`nonmonotonic`) and skip no offset that holds a message (`skip`), except across
  a `sub` the client records (a rebalance).
- **`causal`:** precedence also includes "received, then sent" (a process completed a receive of
  `m1`, then invoked the send of `m2`), closed transitively; in every group that delivers `m2`,
  `m1` was delivered first. Meant for groups with one member each.

**Witness.** The two messages, the sends that order them, and the deliveries that contradict it.

### dead-letter (`dead_letter`)

**Claim.** A message delivered `after` times without an acknowledgement is not delivered from its
channel again. With `to:` it appears in that channel (after its drain) and never in both after
its move. Recorded attempt counts `n` are at least the deliveries observed so far and never
decrease.

### delay (`delay`)

**Claim.** A message is not delivered before `t_inv(send) + d` (its `d=`, or the declared `ms`): a
violation is a delivery whose receive completed earlier.

### fan-out and members (`fan_out`, `members`)

`fan_out: every-group` is `no-loss` per group that must receive the message. `members: exclusive`
is the `lease` rule between different members of a group when a lease is declared; without a
lease it is the `processing` rule between members (both accepted acknowledgements of one message
by two members of a group is a violation), and overlaps are measured as `group_overlap`.

### linearizable (`linearizable`)

**Claim.** The sends, receives and empty receives can be ordered, consistently with real time, as
the operations of one queue: `unordered` (a multiset, as Knossos's `unordered-queue` model) or
`fifo` (Knossos's `fifo-queue`).

**Complexity.** Checking linearizability is NP-complete in general (Gibbons and Korach, "Testing
shared memories", 1997), and Jepsen does not attempt it for queues: its `queue` checker reduces
the history with every non-failed send assumed accepted (O(n), it finds receives from nowhere),
and `total-queue` compares multisets. When every value is sent once, queue linearizability
reduces to the absence of a small set of patterns (Bouajjani, Emmi, Enea and Hamza, "On reducing
linearizability to state reachability", ICALP 2015; Emmi and Enea, "Sound, complete, and
tractable linearizability monitoring for concurrent collections", POPL 2018, with
polynomial-time monitors). In words: a receive of a value never sent, or sent only after it
returned; a value received twice; two values whose sends are ordered and whose receives are
ordered the other way; an empty receive while some value was surely in the queue for its whole
duration. hydo would implement the patterns as those papers define them (not from this summary),
each in O(n log n) over sorted intervals, report the first found as the witness, and confirm the
witness with the WGL search hydo-history already has, which is small on a witness's few
operations. Messages are unique by construction, which is the condition the reduction needs.

## Time-dependent claims

`lease`, `delay`, `duplicates` with a window and the `retention_ms` excuse compare durations, on
guest time, which under hydo is deterministic.

| situation | effect |
|---|---|
| no clock fault, one boot | decided as defined |
| `T=` (a step of the guest's `CLOCK_REALTIME`) | the records' times do not move. A system that measures durations on a monotonic clock is unaffected; one that stores them as wall-clock time stamps sees the step, and a forward step ends its leases early in guest time. With `clock_steps: undecided` (the default) every pair whose interval contains the step's guest time is counted as undecided; with `strict` it is judged, which reports a system whose durations depend on the wall clock. The step's guest time comes from the controller's `fault.clock_*` record |
| a power cut (`C=`, `cf=`) | guest time starts again in the recovery boot, so a pair with one record in each boot has no duration and is undecided. For `lease` nothing is lost: the first boot's holder is gone, so a recovery-boot delivery cannot overlap its work |
| `P=` of the system | not a clock fault: the system's clock runs on while it is stopped, and on resume its leases may all end at once; that is allowed |
| `K=` of the system | a system that kept leases only in memory may deliver a message again while its first holder still works on it: a `lease` violation, a real finding for a system that declares leases |
| `L=`, `G=` of the system | as `K=` for time; their effect on data is judged by `no-loss`, `after-ack` and `duplicates` |

Order, `no-loss`, `no-phantom`, `at-most-once`, `after-ack` and `processing` use positions only and
are decided across clock steps and power cuts. A claim whose candidate pairs were all undecided,
or that had nothing to judge, passes vacuously and the run warns, as M184's `vacuous_passes` does.

## Measures

Computed for every `messages` check, whatever it claims. Times are guest milliseconds within one
boot; pairs across a power cut are left out and counted.

| measure | definition |
|---|---|
| `sends` | send items invoked, `ok`, `fail`, `info`; `info` items later delivered (`recovered`) |
| `lost` | acknowledged messages not delivered to a group that had to receive them and was drained: the count per group and the first 20 ids |
| `excused` | missing messages excused, by reason: `expired`, `dead-lettered`, `possibly-consumed` |
| `unexpected`, `aborted` | deliveries of messages never sent, or whose every send failed |
| `deliveries` | deliveries in all; messages delivered at least once; the duplicate rate `(deliveries − messages delivered) / messages delivered`; the most deliveries of one message; a histogram of deliveries per message (1, 2, 3, 4 or more) |
| `redeliveries` | the extra deliveries by cause: `after-lease`, `during-lease` (with a declared lease), `after-nack`, `after-ack`, `undecided` |
| `stored_copies` | messages with more than one system id `o` |
| `processing` | messages with more than one `ok` acknowledgement in a group |
| `group_overlap` | messages held by two members of a group at once (surely, by the record times) |
| `reorders` | per scope and group: the number of inverted pairs (`m1 → m2` with `m2` first delivered before `m1`), and the largest reorder distance: for a message, the number of messages that follow it and were first delivered before it; the maximum over messages |
| `latency.first_delivery` | from the completion of the send to the completion of the first delivery: count, and the declared quantiles |
| `latency.final_ack` | from the completion of the send to the completion of the first `ok` acknowledgement |
| `held` | per acknowledged delivery: from the receive's completion to the acknowledgement's completion |
| `invisible` | per delivery that was not acknowledged and came back: from the receive's completion to the next delivery's completion, and its ratio to the declared lease |
| `unseen` | at each workload step and at the end: acknowledged messages not yet delivered to some group that must receive them (a backlog curve, as in Jepsen's Kafka workload) |
| `availability` | per operation and per window: `ok`, `fail` and `info` counts |

**Windows within a run.** The control transcript records when every fault started and was healed.
Each measure is also given per window: `before` the first fault, `during:<fault>` for each fault
token, `after` the heal, and `recovery`. A message belongs to the window of its send's
completion; a delivery or acknowledgement to the window of its own completion.

**Across an exploration.** Each run is classed by the kinds of fault in its schedule (`none`,
`kill:<service>`, `pause:<service>`, `partition`, `net:<kind>`, `cut`, `powerloss:<service>`,
`corrupt:<service>`, `clock`, `disk:lying`, `disk:ioerr`; with the role selectors of
docs/design/jepsen-gaps.md, by role). For each class and measure the exploration reports the
number of runs, the median, the maximum and the schedule of the run with the maximum; for each
claim, the share of runs that violated it and the first minimal schedule.

## Results

Additions to results format version 1 (checked by `hydo-schema-compat` for readers of the
released schema):

- `Run.messages`: one entry per `messages` check, with the resolved declaration, each claim's
  status (`held`, `violated`, `undecided`, with the counts of judged and undecided pairs) and the
  measures, overall and per window. Each measure is an optional field, so a measure added later is
  an addition.
- A violated claim is a finding of the existing kind `history-check` with id `<check>.<claim>` and
  `details` carrying the witness: the records involved (process, operation, position, guest time,
  items), the faults active around them, and one sentence. The verdict line names it:
  `HYDO-ORACLE: VIOLATION [jobs.lease] m41 delivered to process 7 at 12.410 s while process 3's
  lease from 12.002 s held until at least 42.002 s`.
- `Exploration.messages`: the per-class tables.
- A new document type `comparison` (next section).

The explorer's failure signature is the claim id, so it shrinks a lease violation to a minimal
schedule that still violates `lease`, and the report lists one minimal schedule per claim.

## Comparing systems

`hydo compare` runs the same schedules on several systems and reports which claims each kept and
what the measures were:

```sh
hydo compare --system a=img/a.img --system b=img/b.img --schedules schedules.txt \
  [--claims <bundle file>] --out cmp/
hydo compare ... --from hydo-out/explore     # the minimal and sampled schedules of an exploration
```

It writes `comparison.json` (`"type": "comparison"`) and `comparison.md`: claims by system (held
in how many schedules, the first violating schedule) and measures by system and fault class.
Without `--claims` each system is judged by its own declaration; with it, every system is judged
by the same declaration (a bundle file), which answers "which of these systems keeps these
promises". The measures do not depend on claims and are always comparable.

What makes a schedule portable between systems:

- Fault targets by role rather than service name (`K=broker.one@5+2`; the selectors of
  docs/design/jepsen-gaps.md, ranked item 3), each system's manifest mapping roles to its
  services. Until then the systems' manifests use the same service names.
- The workload by command ids (the resolved `W=:...` form, MANIFEST.md "Stable compositions"):
  the systems' projects name their commands alike (`parallel_driver_send`,
  `parallel_driver_consume`).
- Knobs are system-specific and left out; a schedule with a token a system's image lacks is
  refused before the run (M184's names check) and listed as not applicable to that system.

Fingerprints are per image, so a comparison compares verdicts and measures, never fingerprints.
Each system's runs still reproduce on their own.

## Recorders

The SDKs' recorders (HISTORIES.md "Recorders") already write any operation with any arguments,
so a client can record this model today with the generic calls. Each SDK gains helpers that
format items and enforce the message-id rule. The Rust SDK is the reference (AGENTS.md "Language
policy"): its tests write the new shared cases in `sdk/cases/history`, and Go, Python and
JavaScript read them.

```rust
use hydo_assert::history::messages::{Item, Messages};
let q = Messages::new("q");
let op = q.send(3, "orders", &[Item::new("m17").key("g2"), Item::new("m18").key("g2")]);
match client.send(&batch) {
    Ok(ids) => op.ok(&[Item::new("m17").id(&ids[0]), Item::new("m18").id(&ids[1])]),
    Err(e) if e.rejected() => op.fail(),
    Err(_) => op.info(),
}
let r = q.recv(5, "orders", "-");
r.ok(&got.iter().map(|d| Item::new(&d.id).attempt(d.count).handle(&d.handle)).collect::<Vec<_>>());
```

The shell gets `hydo_hist_msg` (an item builder) beside `hydo_hist`. The SDKs' files are inside
the recorded images of node-kv, python-kv and rust-kv, so the helpers ship in a release that
re-records those images anyway (as M163 did).

## Which faults matter most

The faults to draw first for a messaging system:

- a kill or pause of the system while deliveries are leased (`K=`, `P=` with timed forms): leases
  kept only in memory, redelivery storms, stale acknowledgements accepted;
- a power cut right after send acknowledgements (`C=`, `cf=<k>` at a flush): acknowledgement
  before durability;
- a power loss or corruption of one node's storage (`L=`, `G=`): a replica's lost suffix,
  acknowledgements that come back, deduplication state lost;
- partitions between consumers and the system, and between its nodes (`N=`, `O=` on links; the
  shapes of jepsen-gaps.md): acknowledgements that do not arrive, leader changes;
- clock steps across lease lengths (`T=` with `clock_steps: strict`): durations kept as wall-clock
  time stamps.

The explorer needs no messaging-specific strategy: `--strategy process` with `--more-faults` and
`--storage-faults`. The project's consumer should sometimes hold a message longer than its lease
(chosen from `HYDO_SEED`), so that leases end inside the runs and stale acknowledgements happen;
`hydo guide messages` says so.

## Proof plan

Rough costs: S days, M one to two weeks, L more.

1. **The checker on recorded histories** (M). `hydo-history/src/messages.rs`: the records, each
   claim, the measures; unit tests per claim with violating, passing and unknown-outcome
   histories; recorded expected answers (`tests/golden/`); the queue patterns cross-checked
   against a WGL search on small random histories.
2. **The declaration and the plumbing** (M). `type: messages` in `hydo-manifest` with `include:`,
   the refused combinations, the schema and the permanent examples; the resolved declaration in
   the image's plan and the `HYDO-CHECK` line; `hydo check-history`, `hydo run` and the explorer
   reporting the findings; `Run.messages` and `Exploration.messages`; the tables in `report.md`;
   `hydo guide messages` with the example bundles. Check that no other image changes with
   `hydo-bench rebaseline --check`.
3. **The recorders** (S to M). Rust helpers and shared cases, then Go, Python, JavaScript and the
   shell.
4. **A planted-bug example** (M): `hydo/onboard/examples/messages-planted/`, a small message
   server written for the purpose (`plantq`, Rust, a few hundred lines: an append-only log with
   fsync, leases, groups, ordered groups, a deduplication window, a dead-letter channel, delays)
   and a Rust client that records through the SDK, declared with the third example above plus
   `dead_letter` and `delay`. A flag plants one bug at a time, each meant to break exactly one
   claim: `lease-half` (leases end at half their length: `lease`), `ack-before-sync` (sends
   acknowledged before fsync: `no-loss` under `C=` and `L=`), `group-parallel` (an ordered group
   not blocked while a message is out: `order`), `ack-in-memory` (acknowledgements not persisted:
   `after-ack` after `K=`), `dedup-forget` (the deduplication state not persisted: `duplicates`
   after `K=`), `wallclock-lease` (leases on the wall clock: `lease` only with `T=` and
   `clock_steps: strict`). The proof: for each bug `hydo explore` finds a violation of its claim
   and only that claim, shrinks it, and reproduces it twice with one fingerprint; the build
   without a bug passes the same seeds; the measures move as expected (the duplicate rate rises
   under kills of the server, latency under pauses). KVM engine first (no module, 4 MiB console),
   then hvx with small message counts, then the Mac.
5. **A real system: NATS JetStream** (M). Apache-2.0, one static binary, already onboarded (M141,
   `hydo/onboard/examples/nats-jetstream/`, with its own Go client). The project declares the
   claims in its `hydo.yaml` and its client records the model; hydo learns nothing about NATS.
   Its published findings are expected answers: Jepsen's 3.1 (2.10.22 deletes the stream when
   every node stops at once; fixed in 2.10.23) must show as `no-loss` violations on 2.10.22 only,
   and 3.4 (the default `sync_interval` loses acknowledged messages at a power cut) as `no-loss`
   under `C=` with the default and not with `sync_interval: always`.
6. **A second real system declared purely through configuration** (M). Its project writes a
   small client that records the model and a declaration of what the system documents; nothing
   in hydo is added for it. Chosen by licence and size, and preferably already pinned so no new
   binary is needed: Redis Streams of the pinned Redis 7.2.4 (BSD-3-Clause, the version hydo
   already ships in its Redis example; consumer groups, a pending-entries list with delivery
   counts, claims of idle messages, durability by `appendfsync`) fits. Then `hydo compare` of the
   planted server, JetStream and Redis Streams with one bundle, which shows the comparison works
   across systems that share no API.

Steps 1 to 3 come first; step 4 proves the checker against known answers; steps 5 and 6 can then
run in parallel. Any further system, the user's own included, is onboarded as in step 6.

## Left open

- The history channel through the agent (unlimited records on hvx), after the first examples.
- Transactions across messages (consume-transform-produce): a `txn` operation with send and
  commit micro-operations, as Jepsen's Kafka workload has, and Elle's cycle search over them.
- Retention as a claim (messages kept at least so long) rather than an excuse; per-message expiry
  and priorities, measured as loss today unless declared.

## Questions for the user

1. Should a duration-dependent claim judged across a clock step count as a violation by default
   (`clock_steps: strict`), or stay undecided as proposed?
2. Is it acceptable to run the first examples on the KVM engine and the Mac, where the console
   holds about 40,000 messages, and to add the history channel for hvx later?
3. Is Redis Streams a good second real system (step 6), or is another system more useful to you?

## Appendix: optional clients for common APIs

Not part of the design's required steps, and the checker never depends on them. If projects ask
for it, hydo could write a client for a common API, as `hydo sql-gen` does for SQL
(docs/design/workloads.md): a generator that writes composer commands and a guest program that
speaks the API and records histories in this model, nothing more. What a system promises would
still come from the project's declaration. Such a client is a convenience for onboarding, not a
part of checking.
