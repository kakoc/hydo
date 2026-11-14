# Writing the specification: how others do it, what can be automated, and a plan for the broker pilot

Research for the planned pilot in which an agent builds an SQS-like broker in a loop, scored by
hydo. Question: how is the "specification" (the thing that decides whether a run is correct)
written in practice, can it be produced automatically or reused, and what should the pilot use?
Every claim below is from a source that was fetched and read; quotes are short.

## 1. What a specification is, in practice

In industry, the specification of a distributed system is almost never one document. It is
three different artifacts, used together:

1. **A checker over a recorded history** of client operations (invoke time, completion time,
   outcome). It answers "could a correct system have produced this?" This is what Jepsen does.
2. **An executable reference model**: a deliberately simple implementation of the same
   interface (a hash map standing in for a storage engine). Runs of the real system are
   compared with it. This is what AWS did for S3's ShardStore.
3. **A design-level model** in a modelling language (TLA+, P, Quint), checked exhaustively for
   small configurations. It finds protocol bugs before code exists, and with newer tools
   recorded traces of the real code can be checked against it.

hydo already supplies the hard infrastructure underneath all three: deterministic runs, fault
schedules, and a console on which the history can be recorded.

### History checkers (Jepsen and friends)

- **Queues: Jepsen's `total-queue` checker** is exactly the core of a broker spec. Its
  docstring: "What goes in *must* come out. Verifies that every successful enqueue has a
  successful dequeue", and "Queues only obey this property if the history includes draining
  them completely." It sorts values into **lost** (acked enqueue never dequeued),
  **unexpected** (dequeued but never attempted), **duplicated**, and **recovered** (enqueue of
  unknown outcome that came out). Validity is "lost and unexpected both empty"; duplicates are
  reported but allowed (at-least-once).
  [jepsen.checker source](https://raw.githubusercontent.com/jepsen-io/jepsen/main/jepsen/src/jepsen/checker.clj)
- **Elle** checks transactional isolation from histories. It builds a dependency graph from
  what transactions read and wrote and looks for cycles. It claims to find every non-predicate
  Adya anomaly, is "~linear in history length", and has checked 22 million transactions in
  about two minutes. It is also explicitly not complete: it "may fail to identify anomalies
  which were present". [Elle README](https://github.com/jepsen-io/elle)
- **Porcupine** checks linearizability against a sequential model written as code
  (`Init`, `Step(state, input, output) -> (ok, newState)`), with partitioning by key. It is
  about 1,000–10,000× faster than Jepsen's older Knossos.
  [Porcupine](https://github.com/anishathalye/porcupine)
- These checkers keep finding real bugs in brokers. Jepsen's 2024 Bufstream analysis found
  "lost acknowledged writes in healthy clusters", and write loss, aborted reads and torn
  transactions in Kafka's own transaction protocol, which "remain outstanding".
  [Jepsen Bufstream 0.1.0](https://jepsen.io/blog/2024-11-12-bufstream-0.1.0)

**The key technique for allowed nondeterminism:** the outcome of every operation is recorded
as ok, fail, or *unknown* (timeout). The checker only requires guarantees for ok operations,
forbids effects of failed ones, and accepts either result for unknown ones. hydo's
front-door oracle already does this ("told failed" vs "outcome unknown").

### Executable reference models (AWS ShardStore, SOSP 2021)

The closest published precedent for what the pilot needs
([paper](https://jamesbornholt.com/papers/shardstore-sosp21.pdf)):

- The models are "written in the same language as the implementation and embedded in its code
  base". They are small: "1% of the implementation code". For example, "the reference model
  for a log-structured merge tree … is a hash map".
- The models double as mocks in unit tests, "effectively requiring engineers to update the
  reference model specifications themselves".
- Correctness is decomposed, with the right tool for each part:
  - **Functional correctness:** property-based testing of the implementation against the model
    "on random sequences of API operations".
  - **Crashes:** they "augment the reference models to define the specific recent mutations
    that … allow to be lost during a crash".
  - **Concurrency:** stateless model checking for linearizability against the model.
  - **Not covered:** "concurrent crashing executions", for which they "have not found an
    effective automated approach". hydo's deterministic fault runs cover precisely that case.
- Results: the checks "prevented 16 issues from reaching production". Today "18% of the total
  reference model and test harness code has been written by the engineering team", not by
  formal-methods experts.
- They chose not to model failures such as I/O errors exactly: it was "very difficult to
  enforce strict equality for failures". So the comparison must be over allowed outcomes,
  not exact outputs.

### Design-level models

- **TLA+ at AWS** ("Use of Formal Methods at Amazon Web Services", 2014): the specs were
  804–939 lines for S3 and DynamoDB components. For DynamoDB they "found 3 bugs, some
  requiring traces of 35 steps". Engineers adopted it once it was pitched as "exhaustively
  testable pseudo-code". They also note one miss: they "failed to find a liveness bug as we did
  not check liveness".
  [paper](https://lamport.azurewebsites.net/tla/formal-methods-amazon.pdf)
- **P** is "a state machine based programming language for formally modeling" distributed
  systems. AWS uses it in "storage (S3, EBS)" and "databases (DynamoDB, MemoryDB, Aurora)"; it
  helped migrate S3 to strong read-after-write consistency. **PObserve** checks that running
  systems "conform to their formal P specifications" by checking service logs against P
  monitors, in testing and in production. [P](https://github.com/p-org/P)
- **Trace validation for TLA+** (Cirstea, Kuppe, Loillier, Merz, 2024): record the variable
  updates of a real program run, then let TLC check that the trace is a behaviour of the spec.
  It "found discrepancies between spec and implementation in each case" studied. It gives "no
  formal correctness guarantee". [arXiv 2404.16075](https://arxiv.org/abs/2404.16075)
- **Quint** (an alternative syntax for the same logic) has model-based testing: `quint run
  --mbt` generates traces, and Quint Connect replays them against the implementation,
  "failing at the first step where the state diverges from the spec".
  [Quint](https://github.com/quint-co/quint)
- **Public broker specs to reuse:** TLA+ specifications for Kafka data replication and KRaft
  are in [Vanlightly/kafka-tlaplus](https://github.com/Vanlightly/kafka-tlaplus) (folders
  `kafka_data_replication/kraft`, `kraft`, `groupprotocol`, `transactions`).

### Property-based state-machine testing and deterministic simulation

- **Hypothesis rule-based testing** generates whole operation sequences ("rules can be chained
  together"), checks `@invariant`s "after every step", and shrinks a failure to "a very short
  program that will demonstrate the problem".
  [Hypothesis stateful](https://hypothesis.readthedocs.io/en/latest/stateful.html)
  This is a cheap first tier before full hydo runs.
- **FoundationDB:** a "deterministic simulation of an entire FoundationDB cluster within a
  single-threaded process", "tens of thousands of simulations every night". Its main workload
  is a cycle test whose invariant (the ring stays intact) makes isolation failures visible.
  [FoundationDB testing](https://apple.github.io/foundationdb/testing.html)
  This is the same idea as Jepsen's list-append: **design the workload so that violations
  become visible**.
- **TigerBeetle's VOPR:** a deterministic simulator that can "verify linearizability", with
  adjustable fault levels (up to 8–9% storage corruption per replica).
  [TigerBeetle](https://tigerbeetle.com/blog/2023-07-11-we-put-a-distributed-database-in-the-browser/)

### Antithesis's approach to properties

- Users "directly state how your system should ought to behave", for example that an
  application "should always recover after a single node dies". Antithesis offers "an agent
  skill that analyzes your system architecture and generates a basic property catalog".
  [Antithesis properties](https://antithesis.com/docs/properties_assertions/)
- "Sometimes" assertions are "a better version of traditional code coverage metrics". One that
  never fires means either the state is unreachable or the tests are too weak. They also act as
  checkpoints from which a run can be branched.
  [Sometimes assertions](https://antithesis.com/docs/best_practices/sometimes_assertions/)

## 2. Ready-made semantics for queues

A broker is the easy case because the rules are published and short.

**Amazon SQS** (all fetched from AWS docs):

- **Standard queues:** "at-least-once message delivery … more than one copy of a message might
  be delivered, and messages may occasionally arrive out of order".
  [standard queues](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/standard-queues.html)
- **Visibility timeout:**
  - A message that is not deleted before the timeout "becomes visible again". The default is
    30 seconds, adjustable with `ChangeMessageVisibility`, with a maximum of 12 hours from the
    first receive.
  - Even inside the timeout, "there's no absolute guarantee that a message won't be delivered
    more than once" (standard and FIFO alike).
  - FIFO: "When a message with a message group ID is in-flight, subsequent messages in that
    group are not made available until the in-flight message is either deleted or the
    visibility timeout expires."

  [visibility timeout](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-visibility-timeout.html)
- **FIFO deduplication:** "If you retry the `SendMessage` action within the 5-minute
  deduplication interval, Amazon SQS doesn't introduce any duplicates." The dedup id is either
  explicit or a SHA-256 of the body.
  [exactly-once processing](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/FIFO-queues-exactly-once-processing.html)
- **Retrying a receive on FIFO:** `ReceiveRequestAttemptId` lets you "retry the same action …
  to retrieve the same set of messages". It is usable "only for 5 minutes", and only if none of
  the messages were modified. A late delete by a slow consumer "fails with an error".
  [ReceiveMessage](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/APIReference/API_ReceiveMessage.html)
- **Dead-letter queues:** `maxReceiveCount` "is the number of times a consumer can receive a
  message from a source queue before it is moved to a dead-letter queue". On standard queues
  expiry keeps "the original enqueue timestamp"; on FIFO queues "the enqueue timestamp resets".
  [DLQs](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-dead-letter-queues.html)

**Kafka** ([Confluent: delivery semantics](https://docs.confluent.io/kafka/design/delivery-semantics.html)):

- At least once is the default.
- The idempotent producer "guarantees that resending a message will not result in duplicate
  entries in the log".
- A committed message is not lost "as long as least one broker that has a replication of the
  partition with that message remains alive".

**NATS JetStream:**

- **Duplicates:** it ignores duplicates "as indicated by the Nats-Msg-Id header", with a
  default window of 2 minutes.
  [JetStream model](https://docs.nats.io/jetstream/model_deep_dive)
- **Redelivery:** an un-acked message is redelivered after Ack Wait (default 30 s).
- **Double ack:** it "waits for the server to confirm the ack". Synadia stresses that dedup
  "does not make end-to-end processing exactly-once".
  [Synadia](https://www.synadia.com/blog/jetstream-reliable-delivery-dlq-replay)

**The pattern:** every broker documents the same handful of properties:
- an acknowledged send is not lost;
- nothing is invented;
- duplicates are bounded by an id and a time window;
- redelivery happens after an ack or visibility timeout;
- order holds per partition or group;
- a group is locked while a message is in flight;
- a message moves to a dead-letter queue after N receives;
- messages expire after a retention period.

That fits on one page, and §5 shows it can be checked in about 200 lines.

## 3. Avoiding a hand-written spec

| Approach | What it gives | Main limitation |
|---|---|---|
| **Differential testing against an existing implementation** (ElasticMQ, LocalStack, real SQS) | A reference with zero spec writing | Outputs can't be compared exactly, because the semantics allow nondeterminism (duplicates, order on standard queues, which unknown sends took effect). The emulators also deviate: ElasticMQ "implements a subset of the SQS query (REST) interface" and says "a message is delivered twice" can happen ([ElasticMQ](https://github.com/softwaremill/elasticmq)); a LocalStack user reports a randomized visibility timeout and a missed DLQ move ([forum](https://discuss.localstack.cloud/t/sqs-not-sending-messages-to-dead-letter-queue/202)). Best use: as a known-good implementation the checker must accept, not as the oracle. |
| **Spec mining from runs** (Daikon) | Candidate invariants for free | They are "likely" invariants, true only "over the observed executions" ([Daikon](https://plse.cs.washington.edu/daikon/)), and they encode current behaviour, bugs included. Useful as suggestions for a human to accept or reject. |
| **LLM-generated formal specs** | Fast drafts | Weak today. A 2026 study of 30 models on 205 TLA+ specs found at best 26.6% syntactically and 8.6% semantically correct, concluding "current LLMs are not there yet" ([Loyola AI4FM](https://ai4fm.cs.luc.edu/papers/llm-tla-evaluation/), [arXiv 2606.05792](https://arxiv.org/abs/2606.05792)). |
| **LLM-generated properties from API docs** | More promising than formal specs | Vikram et al.: about 2.4 samples to get a valid, sound property-based test, but only "21% of properties extractable from API documentation" were covered by correct tests. They score a property by mutation, using "property mutants" that break it ([arXiv 2307.04346](https://arxiv.org/abs/2307.04346)). A 2026 survey summarizes generated properties as "frequently trivial … or incorrect". |
| **The project's own test suite** | Encodes the authors' intent | Usually written for the happy path and for one machine; rarely asserts behaviour under crashes or partitions. Good as a smoke tier. |

**Conclusion:** full automation of a *trustworthy* spec is not available today. What works is a
**human-approved spec, drafted automatically and validated mechanically**:
- an agent drafts the properties from docs, code and schema;
- a person approves them;
- the draft is then scored by whether it accepts known-good behaviour and rejects planted bugs.

That is the same validation loop the bug benchmark already provides.

## 4. Recommendation for the broker pilot

**The checker** has three layers, cheapest first:

1. **A history checker for the published semantics.** It records each operation with its
   invoke and complete times on hydo's virtual clock, and its outcome (ok / fail / unknown). It
   accepts all allowed nondeterminism and rejects the rest. The prototype in
   `hydo/spec/sqs_checker/` (§5) covers send, receive, delete, visibility, retention, FIFO
   order, group lock and deduplication. Run it per hydo run, and turn its result into the
   `HYDO-ORACLE` line, so the explorer, shrinker and reports work unchanged.
2. **An executable reference model** (ShardStore style): an in-memory broker of about 150
   lines in the same language as the implementation, with the same API.
   - **Without faults:** a single-client run must match it exactly, modulo nondeterminism the
     model declares.
   - **With faults:** a crash model of what may be lost. Under the durable setting that is
     nothing acknowledged.
   - **Hypothesis rule-based tests** against the model, as the fast tier before hydo.
3. **Optional, for the protocol:** a small TLA+ or P model of the replication or deduplication
   design if the broker is replicated. It is checked once, not per candidate.

**Validating the checker before trusting it** (anti-Goodhart):

- **It must accept a known-good implementation:** ElasticMQ, or the reference model itself
  run inside hydo, across the fault schedules.
- **It must reject planted bugs.** Use the mutation score from Vikram et al. and our bug
  benchmark: lost ack, duplicate after delete, group lock broken, dedup window off by one,
  visibility not enforced, DLQ never triggered. The prototype's tests are a first version.
- **The agent never sees the acceptance schedules.** Candidates are fixed on one seed set and
  accepted on a held-out set with a different fault mix.

**What can be automated:**

- **Mostly yes:** drafting the property list from SQS/Kafka/NATS docs (§2), drafting the
  reference model, generating planted-bug mutants, and running the validation.
- **No, a human decides:**
  - which contract the broker promises (strict visibility and final delete, or SQS's best
    effort);
  - the durability promise under each setting;
  - anything domain-specific.

**The hard parts:**

- **Time-based semantics** (visibility, dedup window, retention) need a shared clock between
  client and checker, and conservative bounds from the [t0, t1] windows. hydo's virtual clock
  makes these exact and reproducible, which is an advantage over Jepsen on real clocks. The
  clock-jump fault (M46) also tests what happens when time moves.
- **Liveness** ("is eventually delivered") cannot be decided from a finite history. Make it
  bounded:
  - require a final **drain phase**, as `total-queue` does: after faults heal, keep receiving
    until empty with a long enough deadline;
  - report non-delivery as a liveness failure;
  - add `sometimes` assertions for redelivery, DLQ moves and dedup hits, so a run that never
    exercises them is visibly weak.
- **Unknown outcomes:** a send that timed out may appear zero times, once, or, without dedup,
  more than once. The checker must not count it as lost, nor its appearance as a phantom.
  Jepsen's "recovered" category does exactly this.
- **Exactness vs. allowed duplicates.** If the pilot's broker promises strict visibility, the
  checker's strict mode is right. Checking it against real SQS semantics would wrongly fail
  SQS itself, which documents no absolute guarantee. Keep the contract an explicit `Spec`
  choice.

## 5. The prototype: `hydo/spec/sqs_checker/`

(Removed in M108 with the other research scripts; restore it with
`git checkout 705cf98 -- hydo/spec/sqs_checker`.)

`checker.py` (about 200 lines, no dependencies) checks a JSON-able history against a `Spec`.

**Contracts, as `Spec` presets:**
- `sqs_standard()`: AWS's standard queue as documented;
- `sqs_fifo()`: adds FIFO order, group lock and the 5-minute dedup window;
- `strict()`: also requires visibility and delete to be final.

**What it checks:**
- lost acked messages (with a drain);
- phantom messages;
- redelivery inside visibility (strict);
- redelivery after a completed delete (strict);
- retention;
- FIFO: per-group order of definitely ordered sends, the group lock while a message is in
  flight, and dedup within the window. A send acknowledged as a duplicate counts as delivered
  through its twin.

Every time comparison uses the conservative bound, so a violation is reported only if it holds
for every server moment inside the operations' windows.

`test_checker.py` has 17 tests. **9 allowed behaviours must pass:**
- the happy path;
- redelivery after the timeout;
- duplicates and reordering on standard queues;
- an unknown send that appears, and one that doesn't;
- a failed send;
- FIFO order and the group lock respected;
- another group not blocked;
- a dedup retry delivered once;
- a dedup id reused outside the window.

**8 planted bugs must be rejected:**
- a lost ack;
- a phantom;
- a double delivery inside visibility;
- a message resurrected after delete;
- retention exceeded;
- FIFO reordering;
- a broken group lock;
- broken dedup.

Run: `python3 -m unittest hydo/spec/sqs_checker/test_checker.py` (all pass).

**Not covered yet:**
- `ChangeMessageVisibility`;
- DLQ moves after `maxReceiveCount`;
- `ReceiveRequestAttemptId`;
- the 12-hour visibility cap;
- batch operations;
- liveness beyond the drain rule.

## 6. When a business cannot write a spec: a practical ladder

Each rung needs less domain knowledge than the next, and every rung is useful on its own:

1. **Generic properties that hold for almost any system:**
   - an acknowledged write is never lost under the durable setting;
   - an operation reported as failed had no effect;
   - an idempotent retry has no duplicate effect;
   - no crash, panic or 5xx under faults it should tolerate;
   - recovery after a single failure;
   - caches never disagree with the source of truth.

   These are the onboarding skill's `invariant-patterns.md` and already found every real
   bug in our stack.
2. **Rules mined from the schema and code:** uniqueness, foreign keys, NOT NULL, check
   constraints, state-machine enums ("an order goes created → paid → shipped, never back").
   These are re-checked as queries after recovery. Daikon-style mining over traces can suggest
   more numeric relations ("balance ≥ 0"); a human accepts or rejects each one.
3. **Published semantics of the components used:** a queue's semantics (§2), a database's
   isolation level (checked with Elle), a cache's staleness bound.
4. **A reference model for the core domain:** a ShardStore-style simple implementation of the
   critical part only (the ledger, the order book). Small, maintained by the same team, and
   used as the unit-test mock so it stays current.
5. **A design model:** TLA+ or P for the parts that are protocols (replication, leases,
   exactly-once handoffs).

Agents can draft every rung. The bug benchmark (planted bugs must be caught) and the held-out
schedules are what make the draft trustworthy without an expert re-reading it line by line.

## Open risks

- **Checker completeness is the ceiling.** Elle itself "may fail to identify anomalies"; a
  hand-made checker will too. Mutation scoring measures this but cannot prove it.
- **Sampling, not proof.** hydo explores; for the protocol design, a model checker is still
  the only way to cover all interleavings of a small configuration.
- **Generated specs are not reliable yet** (8.6% semantically correct TLA+ in the 2026 study),
  so human approval of the property list stays in the loop.
- **Emulators as oracles disagree with the real service on edge cases** (ElasticMQ subset,
  LocalStack reports). Use them as must-accept examples, not ground truth.
- **Strict vs. documented semantics** must be chosen explicitly, or the loop will optimize
  toward whichever is easier.
