# Strategies that found bugs: which checks and faults to choose

This page lists the testing techniques that found bugs with hydo, with the evidence for each.
Read it when you plan the checks and the faults of a new project. Start with the table, then
read the entries that it names.

Every number comes from runs that the project made on real systems. The examples and their
results pages are in hydo's source repository, under `hydo/onboard/examples/<name>/`; a release
does not ship them. A number that the runs do not give is left out. "Blind" means that `hydo
explore` drew the faults without a hint of where the bug is. A finding keeps the label of its
results page: new as far as the search found, known, by design, or a deployment setting.

## Start here

| system | its promise | the techniques that found most | where they found bugs |
|---|---|---|---|
| database or key-value store | an acknowledged write survives; replicas agree | power cuts of the whole guest; clock steps and the clock audit; one node's storage faults; failovers by the client with a late write; replicas compared after a restart | NATS, Valkey, Dragonfly, SurrealDB, libSQL, Meilisearch, VictoriaMetrics, ClickHouse, Prometheus, Kvrocks, Typesense |
| object store | an acknowledged object stays as written; a listing shows what exists | a listing compared with a read of each key; kills and pauses of one node; clock steps | Garage |
| log or stream with partitions | no loss, one offset per record, no aborted read | the messages check; reactions at console events; one node's storage faults; a client that leaves transactions open, and power cuts and clock steps | Redpanda, NATS, Pulsar |
| job queue or workflow engine | an acknowledged job runs, once, at its time | kills of the process that holds the work; clock steps and the clock audit; the messages check | Hatchet, River, Temporal, Saleor, Restate, NSQ |
| distributed query or compute engine with schedulers and workers | an accepted query ends; a completed query's result stays readable | production's topology and features; checks of how each unknown outcome ends; pauses of a worker past its failure detector; kills of the worker that holds the results | a user's query engine |
| ledger or billing system | the books match the acknowledged operations; a limit holds | the generic rule beside domain rules; concurrent clients without faults; value properties | Blnk, Lago, Formance Ledger |
| service that calls outside APIs | a payment or a mail takes effect once | protocol faults on fakes and emulators; a kill while an answer is late | web-shop, checkout, openapi-payouts, Saleor |
| device updater | the device boots a complete slot of an allowed version | reactions at the update's events; reboots and added disks; a scan of a reaction's delay | SWUpdate, Mender |
| web app | a saved object stays saved; the page shows every error | the page walk with a page rule; power cuts of the whole guest; protocol faults | Vikunja, Memos, web-shop |
| library function | its contract holds for every valid input | function fuzzing | strsim, httparse, num-traits, Qdrant |
| flaky test | the test passes on every run | a pause of one thread; turn lengths and forced preemption points | Qdrant, nats-server, etcd |
| cluster that upgrades in place | every member starts on the new version | upgrades | etcd |

These findings hold for every type of system:

- Run the system in production's shape: its replica counts, its cluster sizes, and its features
  turned on (the entry "Production's topology").
- Write the generic rule first. It found more than any other rule in Lago, Formance Ledger, and
  Blnk.
- Check how each operation ends that the client records as unknown (the entry "How each unknown
  outcome ends").
- Explore with `--strategy process` first and `--strategy swarm` last (the entry "The
  explorer's strategy").
- Run each durability setting at its durable value. A loss under a relaxed setting is the
  setting's documented cost. `hydo explore --configs` explores every value and labels such a
  loss (the entry "Settings that trade durability").
- Do not stop at one clean exploration. A user's query engine passed 20 seeds and then failed 2
  ways in 60 seeds, with production's shape and checks of the unknown outcomes.

## The system's shape

### Production's topology

- **What it is.** The `hydo/` folder runs each service as many times as production does, each
  cluster at production's size, and the features that production turns on. Only the memory
  sizes shrink to fit the guest.
- **What to write.** Read the deployment manifests: the Helm values, the Kustomize overlays, and
  the platform's environment files. List each service's replica count, each cluster's size, and
  each feature that production turns on, for example its caches. Then give `hydo.yaml` one
  service per replica (`scheduler_1`, `scheduler_2`), a `groups:` entry per cluster, and the same
  feature settings. Shrink heaps, cache sizes, and buffer pools, never the counts. `hydo k8s`
  gives a StatefulSet one service per replica, but a Deployment one service only, so add the
  other replicas by hand (`hydo guide workflow`, "From Kubernetes manifests").
- **When it fits.** Any system with replicas, a cluster of its own, or features behind flags.
- **What it found.** A user's onboarding of a distributed query engine (2026-11-20, the Mac
  engine). The first round ran one control-plane node, one scheduler, and the caches off. It
  explored 20 seeds and found nothing. The second round ran production's shape: two schedulers,
  a control plane of three Raft nodes, and the result replication and the caches on. It
  explored 60 seeds with the checks of the entry "How each unknown outcome ends" and found two
  bugs. Each shrank to a small schedule and replayed with one fingerprint:
  - A pause of one worker for 8 s, past the failure detector's timeout, left a query running
    forever (the entry "Pauses of a service past its failure detector").
  - A kill of the worker that held a completed query's results, after a pause of one scheduler,
    left the query completed and its results unreadable.
- **What it misses.** A shape that does not fit in the guest's 8 GiB at most. The second round
  changed the shape, the seeds, and the checks at once, so its runs do not say which part each
  bug needed.
- **Cost.** Memory. With ten services in a guest of 6,144 MiB, the Mac ran one worker at about
  50 s a schedule, so 60 seeds took about an hour. Services of one image share one root with
  `root.share_images: true` (`hydo guide services`).

### The features behind a flag

- **What it is.** A feature that production turns on and a test setup leaves off, such as a
  cache, runs code that no fault reaches while it is off. Turn each such feature on.
- **What to write.** The production value of each flag in the service's `env:`. For an object
  store, run one whose answers the cache relies on. A cache that keeps its manifests with
  conditional writes, create-if-absent (`If-None-Match`) and update-if-the-ETag-matches
  (`If-Match`), needs a store that refuses a write whose condition fails.
- **When it fits.** A system with caches, result replication, or other features that its
  deployment manifests turn on.
- **What it found.** In the user's query engine, the caches of fragments, entries, and
  predicates write their manifests to the object store with conditional writes. In the first
  round the caches were off, so that code never ran. The two bugs of the second round came with
  the caches on, and no bug was tied to the caches alone. In Prometheus, the out-of-order window
  (`out_of_order_time_window`, off by default) gave finding 1. A snapshot lacks the samples of
  the out-of-order head. Turning on a feature also puts a test-only setting beside it. Prometheus
  needed blocks of one minute (`--storage.tsdb.min-block-duration=1m`) so that a run of
  4 minutes sees its compactions.
- **What it misses.** hydo's S3 emulator (`emulators:` with `use: s3`, `rclone serve s3`) was not
  checked for conditional writes. If it ignores the two headers, every race of two writes
  succeeds, and the system's handling of a conflict never runs. The user ran the object store
  MinIO as a service of its own instead. Since 0.5.0 the emulator answers both headers as S3
  does. GreptimeDB's write cache and manifest cache were on in its example, and the manifest
  cache gave its finding 1.
- **Cost.** The feature's memory and its setup.

## Checks

### The generic rule over the client's acknowledged operations

- **What it is.** One check recomputes the business result from the operations that the client
  saw acknowledged, and compares it with the system's own totals. It knows nothing of the domain.
- **What to write.** Each workload command records an acknowledged operation and what it
  changes. A `final_` check in `hydo/checks/` sums the records and compares the sums with the
  system's. `hydo guide composer` gives the files:

  ```sh
  printf '%s\n' "$id" >> "$HYDO_ACKED"; printf '%s ack\n' "$id" >> "$HYDO_OUTCOMES"
  ```

- **When it fits.** A ledger, a billing system, a payment flow: any system whose result is a sum
  of client operations. The promise is that faults do not change the business result.
- **What it found.**
  - Lago ("The short answers"): it failed in all 11 runs of the first exploration
    that broke a business result. The billing rules failed in 8 of them.
  - Lago (finding 8): it alone found a subscription invoiced twice, with
    `units:sub4:api_calls: reported 24, acknowledged 12`, in seed 7 of exploration F.
  - Blnk (finding 3): it alone found acknowledged commits of holds lost with Redis
    without an append-only file.
  - Formance Ledger: only the rules that read the client's records saw the
    duplicates of retries without keys and the payments lost on a lying disk. The rules over the
    ledger's own state found nothing in 177 runs with faults.
- **What it misses.** It cannot judge a decision that the system makes itself. Blnk's draws past
  their overdraft limit passed it in every run. It names the total that is short, not the
  mechanism. Lago's billing rules named the mechanism when they failed.
- **Cost.** The client must record each operation in a form that the check can replay. The check
  was 156 lines of Go in Formance and 380 in Blnk. It runs on every engine.

### How each unknown outcome ends

- **What it is.** The client records an operation as unknown when it did not see the end: a
  query still running at the client's timeout, or a result read that failed. No check judges
  whether an unknown operation was applied, because either answer is correct. So an operation
  that never ends is a broken promise that nobody checks. For each type of unknown outcome,
  write the `eventually_` check that says how it must end.
- **What to write.** Let the workload record each operation under the id that the system gave
  it, so that a check can ask the system about it. Then write two checks (`hydo guide checks`):
  - An `eventually_` check that every accepted operation reaches a final state. Set `within=`
    longer than the system's own timeout for that operation.
  - An `eventually_` check that the result of each operation that the client saw complete is
    still readable after the heal.

  ```sh
  #!/bin/sh
  # hydo: within=600 after=heal poll=10
  # hydo-msg: every accepted query ends as completed or failed
  for id in $(awk '{o[$1] = $2} END {for (i in o) if (o[i] == "unknown") print i}' "$HYDO_OUTCOMES"); do
    state=$(/bin/client status "$id") || exit 2
    case $state in completed|failed) ;; *) echo "query $id is still $state"; exit 1 ;; esac
  done
  ```

  For the operations that the system acknowledged, the claim `acked_outcome` of a `type:
  messages` check states the first rule without a hand-written check (`hydo guide messages`,
  "Read back the final states"). It also keeps the rule for unknown outcomes: such an operation
  may end in any state, so the claim never counts it as a failure.

- **When it fits.** A system that accepts an operation and finishes it later: a query engine, a
  job runner, a workflow engine, a batch scheduler.
- **What it found.** The user's query engine (the entry "Production's topology"). The first
  round's workload recorded a stuck query and an unreadable result as unknown, so no run could
  fail on them. With the two checks, the second round found both bugs: a query that stays
  running after a worker's pause, and a completed query whose results are gone after a kill.
  The claim `acked_outcome` on Meilisearch's task queue broke on two acknowledged additions
  after a power cut: both failed with an internal error, and one of them had its documents
  stored. A kill after an index deletion's `202` left the deletion `failed` with
  `index_not_found` although the index was gone, an error the declaration named as the
  client's; the recorded effect broke the claim.
  Pulsar (findings 1 to 3): a final check over a subscription that nothing else reads judged
  each transaction of `$HYDO_OUTCOMES` all or nothing, the unknown ones too. It broke in the
  first run without faults: the Go client's aborted transactions were delivered
  (pulsar-client-go#1498, known). The client then left 1 transaction in 8 open, as a crashed
  client does. A clock step back of 600 s inside the transaction's timeout kept it open, and its
  partitions held back every later message. After a power cut such a transaction stayed open
  until a transactional client connected. The `eventually_` check that no partition has an open
  transaction after the heal judges the end of each one.
- **What it misses.** In an image built before the fingerprint epoch 6, `$HYDO_OUTCOMES` is
  empty in the recovery boot after a power cut unless `guest.outcomes_after_cut: true` is set
  (`hydo guide composer`). A check that asks about every
  operation on every poll starts many processes. In the user's first version, the run ended as
  `ERROR check: exit budget exhausted` while queries were stuck. Stop at the first operation
  that has not ended, as the example does.
- **Cost.** One status call per operation and poll. It runs on every engine.

### Domain rules from the project's sources

- **What it is.** A domain rule states one promise of the system in its own terms, often as a
  `.sql` check: a query that must return no rows. hydo proposes the evidence and validates each
  rule on runs without faults.
- **What to write.** `hydo rules scan` lists the schema, the state columns, the API and the
  documented promises. Write the candidates into `hydo/rules.yaml`, then run
  `hydo rules check hydo/rules.yaml --mutate` (`hydo guide rules`).
- **When it fits.** A system that enforces a rule across processes, such as an API, a queue, and a
  worker.
- **What it found.**
  - Blnk: the overdraft rule alone found finding 1, draws that ignore their
    limit. The bulk rule found finding 2, an atomic bulk that keeps its first transfer.
  - Saleor: `refund_requests_end`, `gift_card_balance_is_its_ledger`, and
    `placed_orders_are_paid` caught both new bugs, saleor#20022 and saleor#20023.
  - The rule suggestion: on Blnk, Lago, and listmonk, 23 of 25 proposed rules
    held on clean runs. On the examples' saved regressions they caught six findings.
- **What it misses.** A rule that one database transaction enforces rarely fails: 7 of
  Formance's 8 domain rules and 5 of Blnk's 7 never failed. The scan matches words. It read none
  of Saleor's 25 `models.py` files, so 14 of the 17 rules came from reading the code. Blnk's
  atomic-bulk bug was caught only by a rule written after the miss.
- **Cost.** A SQL rule was 5 to 21 lines in Formance and Blnk. `hydo rules check` runs the
  workload without faults on three composition seeds.

### Value properties (`// hydo: keep`)

- **What it is.** A comment line says that a recorded value must not change between two
  functions. A run that breaks it fails, also when every business rule holds.
- **What to write.** Record the value with `key` and `record` lines next to a function, and add
  a `keep` line (`hydo guide values`, section 4):

  ```go
  // hydo: keep overdraft_limit from blnk.setTransactionMetadata to model.canProcessTransaction
  // hydo: key transaction.Reference
  // hydo: record transaction.OverdraftLimit as overdraft_limit at entry
  ```

- **When it fits.** A value that passes through a queue, a cache, or a copy before a decision
  uses it. Blnk's overdraft limit is such a value.
- **What it found.** Blnk (the results page's section "The limit as a property
  that a run can break"): the property broke in 9 of 30 blind schedules, first at seed 4. The
  overdraft rule broke in 1, at seed 21. The property fails the run without faults, where every
  business rule held, and the fixed build holds it. Each break names the first changing call:
  the worker's coalesced copy had the limit 0.0.
- **What it misses.** A property names one value. A comparison of two values, or of a value with
  a constant, is not built.
- **Cost.** The Mac and KVM engines stop at hardware breakpoints; hvx has none for it. On Blnk a
  run took 5.6 to 5.9 s of engine time with the plan and 5.7 to 5.8 s without it, with one
  fingerprint.

### The messages check's claims

- **What it is.** A `type: messages` check judges the history that the client records against
  the promises that the project declares (`hydo guide messages`).
- **What to write.** The claims of the Redpanda example. For transactions, add an `eventually`
  check that no transaction stays open after the heal, as Redpanda's `tx.no_hanging`:

  ```yaml
  claims:
    delivery: at-least-once
    duplicates: forbidden
    order: {kind: log, scope: key}
    fan_out: every-group
    after_ack: no-redelivery
  ```

- **When it fits.** A log, a stream, a queue, or a job runner. The promises are no loss, no
  duplicate, an order per key, and no redelivery after an acknowledgement.
- **What it found.**
  - Redpanda (finding 3): the no-loss claim broke for a committed transaction
    that only one of its two partitions shows.
  - Redpanda (finding 4, on KVM): the duplicates and order claims broke when a produce answer
    named an earlier transaction's offset.
  - Hatchet (finding 1): `tasks.no-loss` broke with the rule `acked` for the
    messages that a crashed engine had read (hatchet#5527).
  - Redpanda (finding 2): seed 9's hang was an error of the drain, which hydo does not shrink.
    With the `eventually` check, seed 37 reached the same cause as `VIOLATION [tx.no_hanging]`.
  - ClickHouse (finding 1): each replica was a group of `fan_out: every-group`, and the drain
    recorded one delivery per stored copy of a block. `fan-out` broke with the rule `converge`
    when a power cut left rows on one replica only, while `no-loss` held. `at-most-once` held in
    every schedule: no retried insert was stored twice.
  - Pulsar: `after_ack: no-redelivery` broke at the first power cut, by design. A broker keeps a
    subscription's acknowledgements in memory and writes them at most once a second. The
    client's documentation says that a receipt does not keep a message from coming again. A
    failover subscription served by one consumer process at a time kept `order: {kind: fifo,
    scope: key}` judgeable; a shared one with prefetching consumers does not keep it.
  - NSQ (finding 1 and the known nsqio/nsq#1386): `acked_outcome` with `effect: recorded`, where
    the effect is a consumer's `FIN`, broke for messages that a consumer had received once and
    that a kill of `nsqd` lost. `no-loss` counted them as delivered. NSQ reports no state per
    message, so the drain wrote the final state from the consumers' own record of each `FIN`.
- **What it misses.** The drain's output stays in the guest, also when it fails. A drain that
  stops before the leases end and the recovery finishes reports false losses. Three of four
  failed explorations of a user's queue came from the plan.
- **Cost.** A drain that reads every message back (`hydo guide messages`, "Write the drain"). It
  runs on every engine.

### An effects file beside a durable execution's journal

- **What it is.** Each journaled step of a handler appends one line with a nonce to a file and
  syncs it: the outside system that the step changes. A check compares the journal's result of
  each step with the lines, so it sees a step that ran again after its result was journaled.
- **What to write.** A `beg` line when the step's closure starts and a result line, both with one
  nonce, and the handler's state with each step's nonce (the Restate example's `service/`). The
  check pairs them. A step whose closure starts after a later step started ran again. A later
  result line whose closure started after the journaled one ended is a re-run too. A closure
  that an abandoned attempt finishes after the journaled one, started before it, is the
  at-least-once of a step that was not journaled yet.
- **When it fits.** A durable execution engine or a workflow engine with journaled steps.
- **What it found.** Restate: no step ran again after its result was journaled, in 40 blind
  schedules and about 120 directed runs with kills, power cuts, and pauses.
- **What it misses.** The file is on the guest's disk, so a lying disk or disk I/O errors break
  it with the system. A step that an abandoned attempt runs at the same time is undecided without
  the start lines.
- **Cost.** One synced write per step. It runs on every engine.

### History checks (list-append and registers)

- **What it is.** The client records each operation's call and result. After the run, hydo
  checks the history for isolation anomalies or for linearizability.
- **What to write.** A check file `list_append_<name>` with `# hydo: history=log
  consistency=strict-serializable` (`hydo guide checks`). For PostgreSQL, MySQL, and libSQL,
  `hydo sql-gen` writes the workloads and the checks (`hydo guide workloads`).
- **When it fits.** A database with transactions or a store with compare-and-set.
- **What it found.**
  - libSQL's sqld (the results page, findings 1 and 3): with its default durability, a power cut
    lost acknowledged appends (`lost-append`). With `strong` set through the administration API,
    4 of 100 schedules still lost appends, because sqld forgets the setting at a restart.
  - YugabyteDB: its default WAL setting lost acknowledged writes at a power cut.
    Each loss was found by another generated check.
- **What it misses.** A level without real time, such as `serializable`, cannot see a lost write.
  In SurrealDB, 167 schedules showed no anomaly at strong snapshot isolation.
- **Cost.** `hydo sql-gen` writes it for the three SQL dialects. The check runs on the host.

### Error logs as findings

- **What it is.** Every error-level line of a service is a finding until you accept its kind in
  `hydo/known-errors.yaml` (`hydo guide error-logs`).
- **What to write.** One line in `hydo.yaml`: `errors: true`.
- **When it fits.** A service whose client retries, so that an error never reaches a rule.
- **What it found.** Formance Ledger (finding 1): the only signal of the ledger
  bug, a `500` when two transactions first use one account in two assets. It came as a new
  error kind at seed 11 of an exploration, after 70 schedules without a failure. It is reported
  as formancehq/ledger#2542. In Blnk the error logs showed finding 2's failed rollback beside
  the rules.
- **What it misses.** It found a bug alone in one of three ledgers. Every kind needs a review:
  15 entries in Blnk and 31 kinds in Formance. An id in a log line splits one kind into many
  (`normalize:` fixes it). On hvx the scan reads every service's whole log through the agent.
  A run of a verbose system can pass the 16 MiB of lines that hvx keeps per boot. In
  GreptimeDB's first exploration, with Kafka and five GreptimeDB services, such a run waited for
  its wall limit, so the example turned the scan off.
- **Cost.** One line, and the review of each new kind.

### Concurrent clients without faults

- **What it is.** Several copies of a workload command run at once, so the system's own races
  show without any fault.
- **What to write.** Name the command `parallel_driver_<name>` in `hydo/workload/`
  (`hydo guide composer`). Let the copies choose the same object sometimes, such as one order or
  one account.
- **When it fits.** A system that checks a limit or a state across processes.
- **What it found.**
  - Blnk (finding 1): five draws of 30.00 against a limit of 50.00 all apply. The value
    property's failure shrank to a schedule without a fault. It runs only the draw and the repay
    commands.
  - Formance Ledger (finding 1): two first uses of one account at once. Natively 196 of 200
    pairs failed.
  - Medusa (finding 1): two refunds of one order at once duplicate its items, found blind at
    seed 2, and again at seed 1 of a later exploration.
- **What it misses.** A race between two objects that the drivers never choose together.
- **Cost.** The drivers only. It runs on every engine.

### A snapshot or a backup read back at once

- **What it is.** A workload command asks the system for a snapshot, a dump or a backup, and
  reads it back at once with the system's own tool. The snapshot must hold every operation that
  was acknowledged before the request. A final check reads every snapshot again, also after a
  power cut.
- **What to write.** An `anytime_` command that records the snapshot's name in `$HYDO_ACKED`
  and reads it with the tool inside the service's image (`hydo-oci <service> <tool> ...`), and a
  `final_` check over the recorded names. Mix every type of write that the system keeps apart:
  the Prometheus client writes one in-order and one out-of-order series in each request.
- **When it fits.** A database or a store that offers snapshots or dumps for backups.
- **What it found.**
  - Prometheus (finding 1): in a run without faults, every snapshot with the head lacked up to
    207 samples. They were the out-of-order samples that the server had not yet compacted into a
    block. The in-order samples were all there.
  - Meilisearch (finding 3): a dump that succeeded was an empty file after a power cut.
- **What it misses.** A workload of one type of write: the Prometheus snapshots held every
  in-order sample. A tool that cannot read the snapshot as it is: `promtool tsdb dump` refuses a
  directory without a `wal` folder, so the client makes an empty one first.
- **Cost.** One read of the snapshot per command. It runs on every engine.

### A listing compared with a read of each key

- **What it is.** A final check lists the store's keys, a bucket or a range, and reads each key
  that the workload wrote on its own. Every key that a read finds must be listed, and no key that
  a read does not find. A read of one key and a listing take different code paths in a
  replicated store.
- **What to write.** A `final_` check that lists first, then reads each key, and prints each key
  that differs on an earlier line than its count. Do not read a key before the listing: a read
  can repair the stale replica. The Garage example's `gac check list` is one to copy.
- **When it fits.** A replicated store with quorum reads, an object store, or any store whose
  scans merge the replicas' answers.
- **What it found.**
  - Garage (finding 2): after one node of three was killed, paused or lost its power, a
    listing named keys deleted while it was down, and GET answered 404 for each. Each node
    filters its own copy before the coordinator merges them. The blind explorations ended with it
    in 52 of 200 schedules, from seed 2 on; the checks of each acknowledged write held in all of
    them. The same check of the multipart uploads found an aborted upload listed.
- **What it misses.** A stale replica that anti-entropy repairs before the check runs. In Garage
  the repair came within 10 minutes, after the run's end, so the check saw it.
- **Cost.** One listing and one read per key at the end. It runs on every engine.

### Every replica compared with the leader after a restart

- **What it is.** After the heal, an `eventually_` check reads every document of every node and
  compares each follower with the leader. The generic rule reads the leader alone, so a follower
  that applied the log otherwise passes it. A kill of one follower makes it replay its log at
  the next start.
- **What to write.** An `eventually_` check that waits for a healthy leader and writes a barrier
  through it. It then compares the id and version of each node's documents with the leader's. Give
  the workload the operations that the system runs in several steps, and writes of the same
  documents after them. A delete by a filter with a small batch size is one. The Typesense
  example's `tsc check converge` is one to copy.
- **When it fits.** A replicated store whose nodes apply a log, a Raft or a primary-backup
  cluster, and whose clients read from any node.
- **What it found.** Typesense (finding 1): a kill of one follower, `K=ts3@40+20`, left that
  follower without three acknowledged documents for good. The replay applied the writes after a
  delete by a filter before the delete's later batches, which then deleted the new versions.
  Blind in 3 of 60 schedules without a step back of the clock. The failing schedules passed
  on an image whose deletes run in one batch.
- **What it misses.** A difference that the system repairs before the check runs, and one that
  needs a node to become the leader.
- **Cost.** One export of every node at the end. It runs on every engine.

## Faults

### Power cuts of the whole guest (`C=`)

- **What it is.** Every service loses its power before a workload step. Only data that reached
  the disk survives, and a recovery boot runs the checks.
- **What to write.** `disk=cut | run C=12`. `hydo explore` can also run one schedule per cut
  point (`hydo guide faults`, "Exploring").
- **When it fits.** Any system that acknowledges a write before it syncs it.
- **What it found.**
  - NATS JetStream (Jepsen's 3.4): with the default `sync_interval`, all 30 cut
    points lost acknowledged messages on 2.10.23 and 2.12.1. With `sync_interval: always`, none
    did.
  - Vikunja (finding 1): 5 of 40 blind schedules lost tasks that Vikunja had
    answered as saved. The shrunk walk typed one task, and `C=4` lost it.
  - Valkey (finding 2, known valkey#5354): a power cut and the restart lose a hash's expiry.
  - Dragonfly (known dragonfly#8646): six failures of the first exploration had a
    cut and no backward clock step.
  - Mender and SWUpdate: `disk=cut | run C=4` gives each its finding 1.
  - Meilisearch (finding 3): `disk=cut | run C=30` left all 5 dumps of the run at
    0 bytes, while each dump task said `succeeded`. In the first exploration 9 of 25 schedules
    failed so; a power loss of the service alone (`L=meilisearch@42+1`) does it too.
  - ClickHouse (finding 1, known #53143): with `fsync_after_insert = 1` and the fsync after a
    merge and a fetch off, `disk=cut | run C=40` lost merged parts on both replicas. Replication
    then stopped for good with rows on one replica. In the first exploration seeds 1 and 5 did it
    blind, and the power cut of seed 8 passed. With every fsync setting on, no cut did.
  - Garage (finding 3): a cluster formed with `garage node connect` saves each node's peers once
    a minute. A cut in the cluster's first minute left every node with no peer, and the cluster
    never answered again. A check that the cluster answers within 120 s of the recovery boot's
    start (`eventually_available`) caught it; without one the run ended in an ERROR of a timed
    out check.
  - Kvrocks (finding 1): a node that has just rejoined by a full synchronization writes the
    master's checkpoint without a sync. A cut in the next seconds left its SST files empty, and
    it stopped at every start. Blind, a cut at the end of the workload did it, shrunk to
    `disk=cut | run C=40` with the composition. One node's power loss (`L=`) did it too. The
    checkpoint held SST files only after the client ran `FLUSHMEMTABLE` on the master. A small
    data set fits in the memtables, and its checkpoint is a WAL, which RocksDB's open replays
    into a synced file.
  - Pulsar (finding 3): a cut restarts every broker, and a broker loads a transaction
    coordinator only when a transactional client connects to it. The recovery boot ran none.
    A transaction that a crashed client left open before the cut was never aborted. Its
    partitions delivered nothing after it. Blind in 3 schedules of two explorations, shrunk by
    hand to `C=16` and the composition. An `eventually_` check that no partition has an open
    transaction after the heal shows the wait even when no acknowledged message is behind it.
    The recovery boot runs only the drain and the checks. A system whose recovery waits for a
    client of some kind meets no such client there, and then the wait is the finding.
- **What it misses.** hydo's S3 emulator writes its objects without an fsync, so a cut of the
  guest loses objects that it acknowledged. In GreptimeDB a query of the recovery boot read an
  SST object shorter than its manifest says. A system whose data lives in object storage needs
  one service's power loss (`L=`) instead. A relaxed durability setting loses writes by
  design: make the setting a build parameter and run its durable value, or every value with `hydo explore --configs`. A lying disk or disk I/O errors break the disk's own
  promise. In Valkey's swarm exploration all 28 failures had a disk fault. Use `--durable-only`
  once the first exploration shows them.
- **Cost.** One recovery boot per cut. In Redpanda most of a 3 to 14 minute schedule went to
  recovery boots and drains. It runs on every engine.

### Power cuts at the k-th disk flush (`cf=`)

- **What it is.** The disk loses its power at one exact flush, so the cut falls inside a commit.
- **What to write.** `disk=cut cf=271 | run T=8:15768000`. `hydo explore --more-faults` draws
  them.
- **When it fits.** A system that commits in several steps, each with its own flush.
- **What it found.**
  - Saleor (finding 4, saleor#20023): seed 5 of the exploration charged a gift
    card without a record. It shrank to `cf=270 T=8:15768000`.
  - The bug benchmark's order service: 12 of 282 flush points failed with durable
    settings and an honest disk. No other fault found that bug.
  - YugabyteDB: the lost writes of its default WAL setting shrank to one cut at
    the fourth flush.
- **What it misses.** The point moves with the image's bytes. Saleor's `cf=270` stopped
  reproducing when its image was rebuilt, and `cf=271` gives the finding now. Restate held at
  all 49 flush points from `cf=37` to `cf=4000`: it syncs its log before it answers, as its
  documentation says. Prometheus held at 19 points from `cf=60` to `cf=1140` and at 11 drawn
  ones, with blocks of one minute, so the cuts fell around head compactions and WAL
  checkpoints. Its rule asked only for the samples of a block that the server had listed.
- **Cost.** hvx only.

### One node's storage faults (`G=` and `L=`)

- **What it is.** `G=` damages one file of a service's storage directory: flipped bits, zeroed
  bytes, or a truncation. `L=` cuts the power of one service while the others run on.
- **What to write.** Give the service `storage:` with `size_mib`, then `G=n1@1+2:zero,len=4096,
  file=tx.snapshot` or `L=n2@10+2`. `hydo explore --storage-faults` draws both, and
  `--corrupt-several` damages several nodes at once.
- **When it fits.** A replicated store or log. The promise is that one node's damage loses no
  acknowledged data and does not stop the cluster.
- **What it found.**
  - Redpanda (finding 1): seed 6 found it blind. A zeroed or bit-flipped
    `tx.snapshot` of one partition stops the whole node at every start.
  - SurrealKV in SurrealDB: 7 times blind in 167 schedules. A damaged value of
    the value log drops every later acknowledged commit. The saved regression is
    `G=surrealdb@40+1:trunc,file=vlog`.
  - NATS JetStream: Jepsen's 3.2, 3.3, and 3.5 in one VM. With two
    nodes' blocks zeroed and the third node killed, 2,456 of 24,446 acknowledged messages were
    lost.
  - VictoriaMetrics cluster: one node's power loss lost nothing alone. With a backward clock
    step before it, the power loss lost a batch that vminsert had stored on that node only
    (finding 3). Damaged files stopped vmstorage at its start in 11 of 30 schedules, and a
    zeroed range returned another value from vmselect.
  - GreptimeDB (finding 1): seed 13 of 20 cut the datanode's cached copy of a manifest delta.
    The datanode then stopped at every start, while the object store kept the intact delta. The
    copy is a cache, so the datanode could read the object again; with the default uncompressed
    manifests it does not notice the short copy. Six power losses of a datanode (`L=`, also
    `:strict`) lost cached copies whole and broke nothing.
- **What it misses.** NATS's 3.3 and 3.5 need faults in a narrow order on chosen nodes, and the
  explorer did not draw them; they were written by hand. `G=` picks its file and offset from its
  seed, so one token damages other files on another image. The explorer never draws
  `L=...:strict`. On one Iggy server, 7 of 20 schedules damaged a segment, WAL or frontier file
  that Iggy detects. Each fenced its partition, as Iggy documents, and each ended in an ERROR
  because the drain could not read that partition. None lost data silently. In Prometheus `G=`
  also picks files of the snapshots, which hard-link the blocks' files: a flipped bit of one
  block's `index` broke the read of every later snapshot. A power loss of the server alone loses
  what its WAL had not synced, as a power cut does. So the rules must know the run of the server
  that acknowledged a write (its start time), or they call that loss a bug.
- **Cost.** A file system per service. The explorer shrinks detected damage too. In SurrealDB the
  shrink of seed 3 took an hour. hvx and the Mac engine ran it.

### Kills of the process that holds acknowledged work

- **What it is.** `K=<svc>@<step>+<down>` kills a service's process tree and starts it again.
  Aim it at the consumer between the acknowledgement and the completion.
- **What to write.** `disk=none | run K=hatchet_engine_1@6+3`. `--cluster-faults` aims kills at
  a group's members.
- **When it fits.** A queue, a job runner, or a worker that takes work from a broker. The promise
  is that acknowledged work is done.
- **What it found.**
  - Hatchet (hatchet#5527): `K=hatchet_engine_1@6+3` loses acknowledged runs on
    every run. Of 12 directed kills of an engine, 5 broke a rule.
  - River (river#1602): `K=postgres@6+1` gives two leaders on every run. Blind, it
    came at seed 14.
  - Saleor (saleor#20022): `H=payments.refund@1-12:delay=6000 K=worker@6+1`
    loses a refund request with its Celery task on every run. Each fault alone passes.
  - Lago (known lago-api#6901): a swarm schedule that killed and paused Redis
    during a cancellation.
  - A user's query engine: a kill of the worker that held a completed query's results came after
    a pause of one scheduler. The query stayed completed, and every read of its results failed.
    Only the check that a completed result stays readable saw it (the entry "How each unknown outcome
    ends").
  - NSQ (finding 1, new): a kill of the broker itself lost 6 messages that it had answered `OK`.
    The broker was `nsqd` with every message on disk, killed about 0.8 s after a topic's first
    message. A disk
    queue writes its metadata only at its first sync, up to 2 s after its first message. Without
    that file the restarted queue starts empty. Blind in the trial at seed 2, shrunk to
    `K=nsqd@4+1`. The same kill 2.8 s after the first message lost nothing. So aim a kill at a
    new topic, a new channel, or a queue just emptied, as well as at a busy one.
- **What it misses.** A kill comes through the guest agent, so the service's threads run on
  until the agent runs. Meilisearch's index swap that runs twice (finding 1) needs
  the server stopped between two LMDB commits. Reactions that killed it at 18 delays after a
  swap, an edit, or an addition all passed. A reaction's `kill-now-<svc>` kills before the
  service runs on, and broke it at 13 and 14 ms after the swap's `202` (section "Reactions at
  console events"). A 15 s pause of a Hatchet engine broke nothing, and a kill at the same step
  lost runs. A kill starts the process with a new identity, so it hides state that the system
  keeps by a worker's identity (the entry "Pauses of a service past its failure detector"). The
  restart of a `K=` fault ignores the service's restart policy (River, seed 17).
- **Cost.** It runs on every engine.

### Pauses of a service past its failure detector

- **What it is.** `P=<svc>@<step>+<n>` stops a service's processes for `n` workload steps, and
  the processes go on afterwards with the same process ids and the same identity. A pause
  longer than the timeout of a health check, a heartbeat, or a lease makes the peers declare the
  service dead, and then the service comes back as itself. State that the peers keep by the
  service's identity then has to clear itself. A kill hides such state, because the restarted
  process registers with a new identity.
- **What to write.** Read the failure detector's timeout from the system's configuration. Keep
  `pause` in `faults:` and explore with `--new-faults`, which draws pauses by steps. For a
  directed run, give the pause enough steps to outlast the timeout, or a length in
  milliseconds: `P=worker_2@28~0:12000` for a timeout of 8 s (`hydo guide faults`).
- **When it fits.** A scheduler or a coordinator that tracks its workers by heartbeats or leases,
  and workers that keep their identity across a stop.
- **What it found.** The user's query engine (the entry "Production's topology"): a pause of one
  worker for 8 s, past the failure detector's timeout. A scheduler marked the worker dead and
  never cleared the mark, because it keeps the mark by the worker's id, which changes only at a
  restart. Queries stayed running forever. The exploration of 60 seeds with `--strategy process`
  found it blind, and the shrinker reduced a schedule of 4 faults to `P=worker_2@28+2` alone.
- **What it misses.** A pause shorter than the timeout changes nothing that the peers see. A
  pause longer than the lease can find a system that counts its lease on the monotonic clock.
  In GreptimeDB a pause of a datanode for 25 s made the metasrv fail its regions over
  (`P=datanode0@10+25`). Afterwards the datanode refused the writes that reached it, because its
  lease had run out. So did 11 s pauses inside a write's WAL append. A
  15 s pause of a Hatchet engine broke nothing, and a kill at the same step lost runs (the entry
  "Kills of the process that holds acknowledged work"). The pause needs the check that every
  accepted operation ends. Without it, a stuck query was an unknown outcome, and the run passed.
- **Cost.** It runs on every engine.

### Clock steps and the clock audit

- **What it is.** `T=<step>:<seconds>` steps the guest's wall clock, as an NTP step does.
  `hydo rules scan --kind clock` lists the code that measures a duration with the wall clock and
  proposes three schedules for each function where it is most sure.
- **What to write.** `disk=none | run K=valkey3@2+3 T=10:-300`. Keep `clock` in `faults:`, and
  run the audit's schedules with `hydo explore --schedule` (`hydo guide faults`).
- **When it fits.** A system with leases, timeouts, throttles, replication delays, or snapshots
  ordered by time.
- **What it found.**
  - Valkey (valkey#5406): a backward step delays a full sync by its size. Blind in
    4 of 237 schedules. The audit's schedule reproduced it on the Mac engine.
  - Temporal (temporal#13099): after a backward step, one crash of PostgreSQL
    takes the server down for the step's length.
  - Dragonfly: a restart after a backward step loads an older snapshot. Blind in
    25 of 120 schedules, from the fourth on, shrunk to one step of 18 s back.
  - River (finding 2): a forward step longer than the 15 s lease gives two
    leaders.
  - Lago (finding 8): a step to 2 s before the billing tick, `T=3:@:10-2s`, with
    lost database answers, invoiced a subscription twice.
  - Restate (finding 1): a step back of 300 s and then a restart of the server hold
    accepted invocations until the wall clock reaches their creation time. Blind at seed 14 of 20,
    shrunk to `P=svc@28+2 T=30:-300 L=restate@30+1`. Either fault without the step passes.
  - VictoriaMetrics cluster (findings 1 to 3): a step back of 20 days deletes the
    current month's partition on both replicas within a minute (`T=20:-1728000`). A step back of
    an hour keeps new samples in memory for an hour, so a crash of both nodes loses 360 samples
    instead of 60. The same step leaves a batch with one copy, and one node's power loss then
    loses it. Blind in 16 of e2's 40 schedules; the shrink was by hand. The watchers run every
    minute, so the regressions use rounds of 1 s to outlast the first tick.
  - Prometheus (finding 2, known prometheus#8312 with a new cause): the audit's second place,
    the scrape loop's alignment (`scrape/scrape.go`), is the cause. After `T=20:-300` every scrape
    of the agent and of the server carried the time of the last scrape before the step. The
    server dropped 81,585 of its own samples as duplicates, although its out-of-order window
    covered the step. A check that two scrapes a second apart carry two timestamps showed it;
    the checks of acknowledged writes did not, because the client stamps its own samples.
  - Garage (finding 1, fixed on `main-v2` after v2.5.0): after a step back, CopyObject onto a
    key that exists or was deleted answers 200 and changes nothing. The copy's version takes the
    clock alone, while every other write takes the newest version's time plus 1. Blind
    in 23 of 200 schedules, shrunk to one step, `T=17:-162000`. A read through another
    node right after each acknowledged write showed it at once.
  - Kvrocks (finding 2): a replica acknowledges only when the wall clock has moved a second past
    its last acknowledgement. After a step back, `WAIT 2` on an idle master answered 0 for the
    step's length, while both replicas held every write. Blind in 9 of 40 schedules of the mode
    where the master's reply alone acknowledges a write, shrunk to `T=17:-648000` alone. With a
    `WAIT` after every write it never showed, because each `WAIT` forces an acknowledgement. A
    check that `WAIT` answers after the heal (`eventually_wait`) beside the check that the data
    agrees told the two apart.
  - Pulsar (finding 2): the transaction coordinator times a transaction out on the wall clock
    (`TransactionTimeoutTrackerImpl`, `Clock.systemUTC()`). A client that leaves a transaction
    open, and a step back of 600 s within its timeout of 10 s, keep it open for 600 s. Its
    partitions hold back every later message, so 18 acknowledged messages were not delivered by
    the drain (`T=32:-600`). The audit named the new coordinator's deadline scan
    (`TxnMetadataStore`) as its place 26, not the timeout tracker of the coordinator that runs.
  - NSQ (known nsqio/nsq#1124): after a step back of 1,265 s every publish to an existing topic
    waits until the clock passes the last message id's time. `NewGUID` returns
    `ErrTimeBackwards`, and `GenerateID` tries again every millisecond. The audit ranked that
    place second, and a blind exploration found it at seed 4.
  - Typesense (findings 2 and 3): the clocks are in two libraries that the server builds in.
    h2o's event loop fires no timer while the wall clock is behind its last run. So after a step
    back of an hour an export of more than 100 documents ends after its first batch on every
    node (`T=20:-3600`). A delete by a filter of more than one batch stops part way.
    braft's election timer runs at wall-clock times. A pause of the leader 10 s after a step
    back of 120 s left the cluster without a leader for 68 s (`T=20:-120
    P=nodes.primary@30+25`). Blind in 5 of the first 30 schedules for the export, each shrunk to
    one step back of 150 s or more. A check that the client samples every node's Raft state once
    a second (`final_available`) showed the missing election; no check of the data did.
- **What it misses.** Many clock effects are by design. Examples are Temporal's delayed timers,
  and River's runs of every periodic job that a forward step skipped. The audit reads text. It
  found no place in Redpanda's 2,694 C++ files, whose transaction expiry uses Seastar's wall
  clock. In Meilisearch's 574 Rust files it found one place, a metric. Steps of
  30 s to a year broke nothing there in 50 blind schedules. In VictoriaMetrics it named 21 functions
  and none of the three findings' places, which read a cached clock (`fasttime`) or
  `time.Now().UnixMilli()`. In Restate it found 8 places that read
  `SystemTime::now`, and none was the finding's: the scheduler reads the wall clock through the
  project's own `WallClock`. Its first place, a retry's `next_retry_at`, is only reported. A
  step of a year lets retention delete the evidence (Redpanda seed 25, River's cleaner). In Iggy
  the audit found one place, in a connector: the server reads the wall clock through its own
  `IggyTimestamp`. A step back of a day there stopped a partition's message expiry for a day,
  because the cleaner deletes the oldest segments first by timestamp. That follows the
  documented rule, so it is a clock effect and no bug. In GreptimeDB's 1,833 files the audit
  named 4 functions, none of the metasrv's: its failure detectors, leases, and region
  migration read the wall clock through the project's own `current_time_millis()`. Steps from
  30 days back to a year forward, alone and in the blind schedules that drew them, broke nothing.
  The audit looks for durations only. Garage orders an object's versions by wall-clock
  timestamps, a last-writer-wins order, and the audit named only the request signatures' date
  checks there, not the copy of Garage's finding 1. In Kvrocks's 255 C++ files it named
  none: the server reads the wall clock through its own `util::GetTimeStamp`, and finding 2's
  place was found by reading the replication code. In Typesense it named 14 functions, search
  cutoffs and rate limits, and none of the two findings' places. Those are in h2o and braft,
  whose sources are not in the project.
- **Cost.** The shrinker spends hours on clock steps; stop an exploration once its failures are
  understood. It runs on every engine.

### Failovers by the client, with a late write

- **What it is.** The client fails a master over to a replica, as an operator's tool such as
  Redis Sentinel does, during the run. After the promotion it sends one more write to the old
  master, as a client does that has not learned of the failover yet. The old master's history
  then differs from the new master's, so it must rejoin by a full synchronization, the path of
  a store's replication that the faults reach least.
- **What to write.** A `serial_driver_` command that promotes a replica and records the new
  master in a synced file on the data disk. It writes once to the old master and records that
  write's outcome. Then it points every node at the new master and makes the roles last across
  restarts. Give
  the group a `primary:` command that prints the recorded master, and an event after the
  failover for reactions. The Kvrocks example's `kvc failover` is one to copy.
- **When it fits.** A store with a master and replicas whose failover is outside the store:
  Redis-compatible stores, databases with an external failover manager.
- **What it found.** Kvrocks (finding 1): the old master and every replica that had taken the
  late write rejoined by a full synchronization. A power cut or one node's power loss in the
  next seconds left that node unable to start. Blind in 10 of 60 schedules, and in 14 of 30
  cuts aimed after the failover's event.
- **What it misses.** The failover runs between workload steps, so a fault inside its own steps
  comes only from a reaction at its events. The client's failover is not Sentinel's: it has no
  quorum and no election.
- **Cost.** A synced file for the master's name, and a check that the replicas agree with the
  master after the heal. It runs on every engine.

### Cluster faults and group selectors

- **What it is.** A manifest's `groups:` names a cluster's members, with a `primary:` command.
  The tokens then select members: the leader, a majority, or two halves of the group.
- **What to write.** `K=nodes.primary@5+1`, `N=nodes.halves@3-9:loss,100%`, and
  `hydo explore --cluster-faults` (`hydo guide faults`, "Faults on a cluster").
- **When it fits.** A cluster with a leader.
- **What it found.**
  - NATS JetStream 2.10.22: 11 failures in 30 schedules with the option against 9
    without it. Two had no power cut, for example `N=nodes.isolate@2-10:loss,100% T=11:-18`,
    which lost 5 of 34 acknowledged messages.
  - Redpanda (finding 3): the explorer shrank seed 24 to `K=nodes.primary@66+1 P=n2@42+11`.
- **What it misses.** With `network: bridge` the explorer draws links only between services that
  list each other in `depends_on`.
- **Cost.** `network: bridge` for partitions.

### Reactions at console events (`A=`)

- **What it is.** A fault comes at the moment the client or the system writes an assertion record
  to the console, not at a step boundary.
- **What to write.** `A=rp.txn_add@20+3:kill-n1` or `A=mender.commit_started@3+7:cut`. Give the
  client an event before each operation that matters. `hydo explore --reactive fault
  --react-events <id>,...` draws them. To kill a service between two commits, write
  `kill-now-<svc>`: a relay in the guest kills it at the record's moment, or `ms` after it,
  before any of its threads runs again. `--kill-now` draws it.
- **When it fits.** A narrow window inside one operation: a transaction that adds partitions, an
  update's commit.
- **What it found.**
  - Redpanda (finding 3): `A=rp.txn_add@20+<ms>:kill-<node>` broke the claim in 5 of 9 runs, each
    node at 0, 3, and 8 ms, with no other fault.
  - Mender (finding 2): blind at seed 10 of 24, as `A=mender.commit_started:cut`.
  - Meilisearch (findings 1 and 2): `A=meili.add_acked@20+40:cut` cuts 40 ms
    after an addition's `202`. It empties the payloads of the additions not yet processed. It
    also catches an index swap between its two commits, so the restart runs the swap again. A
    step boundary (`C=30`) loses no payload. Blind, seed 26 of the second exploration lost a
    payload, shrunk to `A=meili.add_acked:cut`.
  - Meilisearch (finding 1 again): `A=meili.swap_acked@20+14:kill-now-meilisearch`
    runs the swap twice on every run on the Mac engine, and `+13` too. On hvx
    `A=meili.swap_acked@30+<ms>:kill-now-meilisearch` does it at 34 to 40 ms. The plain kill at
    the same delays passes on both. Blind, `hydo explore --strategy process --reactive fault
    --kill-now` ran the swap twice at 6 of 360 seeds on the Mac engine, the first at seed 101.
    The shrink left one reaction, `A=meili.edit_acked:kill-now-meilisearch`.
  - SWUpdate: two of the five minimal schedules of its hvx exploration carry a
    reaction.
- **What it misses.** Restate held under cuts and kills at six events of the client and the
  service. Prometheus held under `kill-now-prometheus` 5 to 40 ms after the client saw a new
  block, when the WAL checkpoint follows, and under cuts at the same delays. One of them is a cut at the start of a step's closure, just after the step before it
  was journaled. On the bug benchmark the drawn reactions found no defect
  sooner. The sum of the runs to each defect's first detection grew from 154 to between 181
  and 187. A new event in the client changes the image, and with it the timing of every
  schedule.
- **Cost.** The events in the client. It runs on every engine.

### A scan of a reaction's delay

- **What it is.** One reaction runs with a list of delays after its event, one run per delay, to
  map the window where a fault breaks a promise.
- **What to write.** `hydo explore --reactive-at mender.commit_started@3`. It tries every
  delay from 0 to 50 ms, each with `kill-now-<svc>`, `kill-<svc>` and a power cut, one schedule
  per seed, and shrinks each failure (`--react-delays` and `--react-actions` change the grid).
  The random draws of `--reactive` take a delay from six fixed values only.
- **When it fits.** A window of a few milliseconds after an event, whose place differs between
  engines.
- **What it found.**
  - Mender on hvx (the results page, "On hvx"): the commit that rolls back a permanent update
    came at 64 ms and at 69 to 87 ms after the event. On the Mac engine `+7` failed and `+8`
    passed.
  - Redpanda (finding 3): delays of 0, 3, and 8 ms on each node gave 5 failures of 9.
  - Meilisearch (finding 1): no `kill-<svc>` delay after a swap broke it in hydo (1 to
    25 ms). The same scan in Docker, `pkill -9` 0 to 45 ms after the swap in steps of 3 ms, ran
    the swap twice at 0, 9, 12, and 15 ms. `kill-now-meilisearch` at 0 to 50 ms ran it twice at
    13 and 14 ms on the Mac engine.
  - Meilisearch again, on an image built at the fingerprint epoch 5, where the saved `+14`
    passed. `hydo explore --reactive-at meili.swap_acked@20` ran the swap twice first at seed
    29 of 153, a `kill-meilisearch` 9 ms after the `202`. `kill-now-meilisearch` did it at 11
    and 12 ms.
  - Garage, nothing: 48 power cuts of the whole guest 0 to 30 ms after an acknowledged
    PutObject, overwrite and DeleteObject on three nodes with `metadata_fsync` on lost no write.
    LMDB may undo its last transaction at a crash there, but Garage commits other transactions
    within milliseconds. The kills of one node in the same grid showed finding 2, a listing of
    deleted keys.
  - Kvrocks (finding 1): a scan of seconds, not milliseconds. `--reactive-at
    kvrocks.failover_done --react-delays 500..25000/2500 --react-actions cut` cut the guest 0.5 s
    to 23 s after a failover, and 14 of 30 schedules left a node that did not start again. A
    grid with `--seeds` runs one point per seed, so the whole grid three times needs as many
    seeds as three grids have points.
  - NSQ (findings 1 and 2): a kill of `nsqd` 0 or 1,000 ms after a consumer took a message lost
    every message of the topic. At 2,200 ms it lost only the message in flight. The first two
    delays fall before the channel queue's first metadata sync, finding 1, which is new. The third
    falls after it, finding 2, the known nsqio/nsq#1386. The trial's grid shrank its failures to
    `+0` and took them for the in-flight loss. Separate the causes with delays on both sides of
    the system's sync interval, and read the files after the restart.
  - SeaweedFS, nothing: 60 power cuts and power losses of the filer or of one volume server
    lost no write with the fsync rule on. They came 0 to 40 ms after an acknowledged write, a
    deletion, or a compaction of the volumes. 65 kills of each master, one volume server, and
    the filer, 0 to 3 s into writes of 4 MiB or more, lost none either. The client wrote a
    console event at the start of each large write, and before and after each compaction that
    it ordered. So the grid has moments inside the work, not only after its answer.
- **What it misses.** The window moves with the program's bytes. A shrink moves a delay towards
  0, which can be the window of another cause. A second build of
  `mender-update` moved it to 65, 70, and 75 ms.
- **Cost.** One run per delay and action: 153 runs for one service and the 51 default delays.

### Protocol faults on fakes and emulators (`H=`)

- **What it is.** A fake or an emulator stands for an outside service, and `H=` delays, drops,
  resets, or fails its answers (`hydo guide outside-services`).
- **What to write.** `H=api.place_order@0-1:drop` or
  `H=stripe.payment_intent@3-4:status=500,effect=yes`. Use `emulators:` for Stripe, SMTP, and S3,
  and `hydo fake from-openapi` for a service with an OpenAPI description.
- **When it fits.** Payments, mail, webhooks: any call that must take effect once.
- **What it found.**
  - web-shop: with `--protocol-cover` the double order came at seed 3 and the
    stuck spinner at seed 4. The random draws first lost the order's answer at seed 120.
  - openapi-payouts: with `--protocol-cover` the retry bug came at seed 3 instead of seed 28.
  - checkout: the planted double charge failed 5 of 200 blind schedules, and none
    of the first 40.
  - Saleor (finding 1): the lost refund request needs a delayed refund answer and a kill of the
    worker. Each fault alone passes.
- **What it misses.** A fake answers only what it was written to answer. Medusa's first fake
  could not send Stripe's signed webhooks, so its finding 2 was partly the fake's. In
  GreptimeDB, S3 errors, delays, drops and resets on every route of the S3 emulator broke
  nothing. They made DDL statements end without an answer and take effect later, which the
  check must allow. GreptimeDB's production path makes no conditional writes: only its
  experimental object-store WAL does, so `put_if_absent` faults met no request.
- **Cost.** Fast runs: checkout ran 200 schedules in 242 s on hvx with 4 workers.

### Pauses of the workload or of one thread

- **What it is.** `P=workload/<thread>@<step>~<ms>:<ms>` stops the named threads of the test for
  some guest time, while its other threads run on. `P=workload@...` stops the whole test.
- **What to write.** `disk=none | run P=workload/tests::wal_reco@0~4433:500`. `hydo explore
  --pause-thread <name>` draws them, with `--pause-min` and `--pause-lengths`.
- **When it fits.** A flaky test with a race between its own thread and a background worker.
- **What it found.**
  - Qdrant's flaky test 10200: the pause fails it on every hvx run, and the
    fixed test passes. 2,604 native runs never failed. The failing starts are about 1 ms wide.
  - etcd's flaky test 22796: a 60 ms pause of the workload failed it at starts of
    0 to 7 ms on the Mac. On hvx it failed at most starts from 151 to 163 ms.
- **What it misses.** Blind starts from 0 to 5,000 ms found none of Qdrant's windows in 300
  schedules. Find the moment first, then give `--pause-min` and `--pause-max` around it.
- **Cost.** On the Mac and KVM engines it needs an image built at the fingerprint epoch 3 or
  later.

### Turn lengths and forced preemption points

- **What it is.** hydo changes where threads switch: the turn length of several vCPUs (`q=`),
  forced preemption points (`pct=`, `pctn=`), or shorter and inserted ticks on hvx (`pp=`,
  `xt=`).
- **What to write.** `hydo explore --vcpus 2 --turns`, or `hydo explore --timing --preempt`.
  To find the turn length where the verdict changes, read the section "Find where the verdict
  changes" of `hydo guide flaky-tests`.
- **When it fits.** A flaky test whose failure depends on thread timing.
- **What it found.**
  - nats-server's flaky test 8363: on KVM with 2 vCPUs the search found the
    boundaries in 20 runs: `q=15` fails and `q=16` passes, `q=39` passes and `q=40` fails.
  - Qdrant (epoch 2, KVM): 4 of 400 schedules failed. `pctn=40` failed and `pctn=39` passed.
  - etcd 22796 with a CPU limit in the guest: 38 of 100 schedules failed, each
    with `pp=` or `xt=`. None of the 51 schedules without them failed.
- **What it misses.** On KVM and the Mac these tokens count VM exits. A window without an exit
  is out of their reach. A new fingerprint epoch moves the timing: at epoch 3 Qdrant's `pctn=40`
  passes.
- **Cost.** Several vCPUs need hvx or KVM.

### Upgrades (`U=`)

- **What it is.** `U=<target>@<step>:<version>` switches a service to another version during the
  run, on the same data. A list or a group's selector gives a rolling upgrade.
- **What to write.** `services[].versions` in the manifest, then `disk=none | run
  U=etcd1@5:3.6.0`. `hydo explore --upgrades` draws them.
- **When it fits.** A cluster that upgrades in place.
- **What it found.** etcd: one token reaches issue 19557, "too many learner
  members in cluster". Blind, 24 schedules drew `U=` in 11, and two of them were the bug. In
  SurrealDB, the experimental upgrade from 3.x held under kills and power cuts.
- **What it misses.** Versions of an image service are refused, and the explorer draws one `U=`
  per schedule.
- **Cost.** The image carries every version.

### Reboots and added disks

- **What it is.** `guest.reboot: true` lets a guest program ask for a real reboot.
  `guest.disks` adds disks, each with its own write cache, so a device's storage chips can be
  modelled.
- **What to write.** A guest program prints the console line `HYDO-REBOOT`. The sections
  "Reboots" and "Added disks" of `hydo guide manifest` give the keys. Add reactions at the
  update's events.
- **When it fits.** A device updater with two root slots and a boot loader's environment.
- **What it found.**
  - SWUpdate: a slot that is never flushed boots its old contents after
    a power cut. With real reboots, 20 schedules gave 7 failures, at 82 runs an hour.
  - Mender (finding 1): 24 schedules gave 9 failures in three signatures.
  - RAUC held in 80 schedules on the Mac engine and 58 on hvx.
- **What it misses.** A window between the install and the reboot exists only when the client
  waits between them. SWUpdate's first client rebooted at once, and 25 schedules found nothing.
- **Cost.** Added disks need the Mac or KVM engine. A device model needs its own disk images and
  a boot loader model.

### The page walk and its invariants

- **What it is.** `browser.explore` lets hydo walk through a web app's pages from a seed. Five
  invariants watch every page: uncaught errors, spinners that stay, unshown failed requests,
  broken texts, and failing tests.
- **What to write.** `browser.explore` in `hydo.yaml` (`hydo guide web-apps`), one page rule
  over the database, `watch.failures_of` for the writes, and `avoid:` for busy controls.
- **When it fits.** A web app whose server keeps the data.
- **What it found.**
  - web-shop: both planted bugs blind in 20 schedules, without a test.
  - Vikunja: finding 1 failed 5 of 40 schedules, of which 19 had a power cut.
    Finding 2, an unhandled rejection, came at seed 9 under disk errors.
  - Memos: a share link's copy button throws over plain HTTP (memos#6575), one
    failure in seeds 21 to 60.
- **What it misses.** Memos's first 20 walks spent 216 actions in the editor without saving.
  A later walker reached neither Memos finding in seeds 21 to 60. A walk that
  a power cut ends is not judged.
- **Cost.** About 40 s a run on hvx; `network: bridge`.

## Timing and engines

### A second engine's timing

- **What it is.** The same schedule on another engine runs with other timing, so it reaches
  other windows.
- **What to write.** Build the image for the engine, then `hydo run --backend kvm "<schedule>"`.
- **When it fits.** A timing-sensitive finding, or a schedule that passes on the first engine.
- **What it found.** Redpanda's finding 4 came only on KVM, from `A=rp.txn_add@20:kill-n3` on
  three hosts. The hvx explorations never showed it. web-shop's seed 8 walk fails on hvx and
  passes on KVM.
- **What it misses.** Fingerprints differ between engines, and on KVM between AMD and Intel.
  Compare verdicts, not fingerprints.
- **Cost.** An image per architecture: the Mac engine runs arm64 guests.

### The explorer's strategy

- **What it is.** `--strategy` chooses how `hydo explore` draws faults: `process` gives 2 to 4
  fault kinds in staggered windows, and `swarm` switches whole categories on and off per run.
- **What to write.** `hydo explore --seeds 1-20 --strategy process` first, and `--strategy
  swarm --new-faults` last. The process strategy draws pauses, clock steps and one-way losses
  itself, so `--new-faults` adds nothing to it.
- **When it fits.** Every project.
- **What it found.** On the bug benchmark, out of 24 possible
  detections, random made 19, swarm 21, and process 24. Swarm finds more kinds of failure,
  many of them disk faults.
- **What it misses.** Swarm draws lying disks and I/O errors, whose failures are the disk's.
- **Cost.** Add `--more-faults` on hvx. With `--backend kvm` and on a Mac the explorer refuses it.

## Narrow and confirm a finding

### A forced order in a native script

- **What it is.** After hydo shows the cause, a script under Docker forces the order of the
  steps, with locks, an administration call, or a fixed step order. The bug then shows on every
  try.
- **What to write.** A script in the example's `native/` folder that runs the project's own
  images and holds each step until the next one may run.
- **When it fits.** A cause that is a timing or environment effect, which the code alone does
  not show.
- **What it found.**
  - Redpanda finding 3 (`native/half-commit.sh`): 10 of 10 commits lost their record in the
    program's step order. Two racing producers without it lost a record in 5 of 205 commits.
  - Redpanda finding 4 (`native/stale-offset.sh`): with a move of the leader, 20 of 20 rounds;
    without it, 0 of 20.
  - Meilisearch finding 1 (`native/swap-twice.sh`): without a forced order, a SIGKILL after a
    swap and 16 additions ran the swap twice in 4 of 48 rounds.
  - Saleor finding 4: two row locks hold the gift card charge at the point after the balance
    commit.
- **What it misses.** Without a forced order the native rates are low. Hatchet lost runs in 4
  of 8 crashes, and River gave two leaders in 5 of 30.
- **Cost.** About an hour. Skip it when the cause is plain in the code.

### Directed workloads for a narrow trigger

- **What it is.** A workload command builds the trigger that a blind exploration does not reach.
- **What to write.** One command that makes the trigger with ordinary client requests, such as a
  `singleton_driver_` command (`hydo guide composer`).
- **When it fits.** A known issue whose trigger is a coincidence, such as two expiries on one
  nanosecond.
- **What it found.** TigerBeetle's issue 3960: 150 blind schedules gave no tie.
  The `tie/` workload sets the guest's clock with `date -s`, and 0.16.29, 0.16.45, and 0.17.9
  stop on every run.
- **What it misses.** It needs the cause first, so it confirms a finding and does not find one.
- **Cost.** One command and one image.

### Version pairs

- **What it is.** One exploration runs on a release with a known bug and on the release with its
  fix. The pair checks that the setup can reach the bug.
- **What to write.** A build parameter for the version, then the same seeds on both images.
  `hydo versions` replays a finding on a list of versions (`hydo guide workflow`).
- **When it fits.** A project with a known bug that a fault reaches, before the hunt for new ones.
- **What it found.**
  - etcd 3.5.5 (issue 14685): 19 of 100 schedules failed, and 0 on 3.5.6.
  - NATS 2.10.22: 14 of 50 schedules lost the stream, and none on 2.10.23.
  - `hydo versions` replayed Formance's account race. It is present from v2.4.0
    and absent in v2.3.24.
- **What it misses.** YugabyteDB's known TA-30772 was not reproduced on either release.
- **Cost.** One image per version.

### Two versions on the same schedules

- **What it is.** `hydo compare-versions` runs each schedule on version A and on version B. It
  reports where the client's outcomes, the stored state, or the verdicts differ (`hydo guide
  workflow`, "Does a new version behave the same").
- **What to write.** A build parameter or two git revisions, and a `compare:` command that
  prints each service's state in a fixed order.
- **When it fits.** An upgrade, a dependency bump, a refactor, or a changed setting, when the
  team wants to know what else changed.
- **What it found.**
  - Redis `appendfsync always` against `everysec`: 4 of 10 drawn schedules
    differ. One shrank to `disk=ioerr:1 | run`: with `always` Redis stops at the first failed
    write, and with `everysec` it acknowledged all 20 writes.
  - Formance Ledger v2.3.24 against v2.4.0: `disk=none | run` differs. Only
    v2.4.0 logs the account race of its `UpsertAccounts` rewrite, and one operation needed a
    retry.
  - Two builds of the same Redis source, and Redis `hz 1` against `hz 10` on four schedules,
    gave no difference.
- **What it misses.** A workload that picks its next operation from the order of earlier
  answers makes other choices on another version. In the Formance run, one revert chose
  another transaction.
- **Cost.** Two runs per schedule, and a shrink of up to 20 candidates on both versions.

### Settings that trade durability (`hydo explore --configs`)

- **What it is.** One exploration per value of a setting that the system reads at start, with
  the same seeds. The table says which values fail under which faults. A failure that appears
  only with values whose documentation names the trade-off is labeled by design.
- **What to write.** The setting as a build parameter and its values under `configs:`, with a
  `tradeoff:` note per value that the documentation says weakens a promise. Then `hydo explore
  --configs` (`hydo guide workflow`, "Which settings break a promise").
- **When it fits.** A store or a log whose durability or consistency depends on a setting: an
  fsync policy, a sync interval, a write cache, a journal mode.
- **What it found.**
  - Redis's `appendfsync` (the Mac engine, seeds 1-20): `always` passed all 20
    schedules. `everysec` and `no` lost acknowledged writes in all 5 schedules with a power cut,
    shrunk to `disk=cut | run C=1`, and the table labeled it the documented trade-off.
  - NATS 2.12.1's `sync_interval` (the same seeds): `always` passed all 20. The
    default of two minutes and 10 s lost acknowledged messages in all 5 power-cut schedules
    (Jepsen's 3.4), labeled by design. Seeds 1-6 drew no power cut, so start with 20 seeds.
  - Iggy 0.10.0's topic option `durability` (hvx, seeds 1-12, kills and power cuts):
    `persisted` passed all 12 schedules. Iggy's default `replicated` lost acknowledged messages
    in 7. A kill of the server alone did it, shrunk to `K=iggy@15+1`, and so did a power cut
    alone, `C=30`. The table labeled both the documented trade-off. Iggy's README says that
    `replicated` acknowledges before the copy reaches the disk, and its issue 4631 measured the
    loss after a `kill -9`.
  - Garage v2.5.0's `metadata_fsync` and `data_fsync` (the Mac engine, seeds 1-20,
    `--durable-only`): with both on, no power cut lost a write. With Garage's default, both off,
    `disk=cut | run C=1` lost an acknowledged PutObject on a cluster of three nodes, labeled the
    documented trade-off. Both values also showed Garage's finding 2, a listing fault that needs
    no power cut.
  - ClickHouse's fsync settings (hvx, seeds 1-8): with every fsync setting on, all 8 schedules
    passed. With the defaults, 3 of the 4 power-cut schedules failed. Two lost a block
    acknowledged with `insert_quorum = 2`, and the shrink of one kept the cut and the quorum
    inserts alone. With `fsync_after_insert` alone, 3 of 4 reached finding 1, the replication
    that stops for good. That is a known bug, which a relaxed value only reaches.
  - SeaweedFS 4.55's path rule `fsync` of the filer (hvx, seeds 1-20, `--durable-only`, three
    masters and three volume servers with two copies of each volume): with the rule on
    `/buckets/`, no schedule failed. Without it, SeaweedFS's default, 5 of 20 power cuts of the
    guest lost acknowledged writes of the filer and of S3. The shrink gave `disk=cut | run C=1`,
    and the table labeled it the documented trade-off. The two copies do not help when one power cut
    takes both. `hydo save` wrote the regression with `# params: fsync=false`, and `hydo repro
    --regressions` replays it on that build only.
  - Before it, each such finding came from one setting tried by hand. Examples are the write
    caching of Redpanda, the SQLite mode `synchronous=NORMAL` of Vikunja, and the durability
    default of libSQL.
- **What it misses.** A setting that the system changes at run time is a `knobs:` entry (`R=`),
  which the explorer draws inside one image. A failure with another cause that appears only with a relaxed
  value is labeled by design too, so read its check before you dismiss it. ClickHouse's finding 1
  appears only with a relaxed value, and it is a known bug. With `--no-shrink` the table lists no
  class of failure and says that no combination failed, so explore with shrinks.
- **Cost.** One image and one exploration per combination. With more than 12 combinations, a
  pairwise set keeps the count near the product of the two largest settings' values.

## Functions

### Function contracts in a whole-system run

- **What it is.** `// hydo: requires` and `ensures` comments next to a function are checked at
  every call while the system runs under faults (`hydo guide manifest`, "Function contracts").
- **What to write.** The comments, and `contracts:` in `hydo.yaml` with the program and its
  sources:

  ```rust
  // hydo: ensures req_id > old(l.last_req) || l.balance == old(l.balance)
  ```

- **When it fits.** A Rust or Go service with a function that moves money and can state its
  result.
- **What it found.** The contracts-ledger example's planted double debit breaks `ensures` only
  under a pause or a lost answer. On Blnk, 24 blind schedules found nothing that the rules
  missed. The overdraft contract broke in the rule's runs and named the call.
- **What it misses.** No contract states Blnk's atomic bulk.
- **Cost.** The Mac and KVM engines only. A run with the plan has the fingerprint of the run
  without it.

### Error paths that the faults reached (`error_paths: true`)

- **What it is.** hydo records which error-handling places of a program ran in each run: Go's
  `if err != nil` bodies, Rust's `Err` returns, `?`, `unwrap()` and panics, and retry, recovery
  and rollback functions (`hydo guide error-paths`). It finds no bug by itself. It says which
  error handling an exploration tested.
- **What to write.** `error_paths: true` in the program's `contracts:` entry. Then explore with
  faults and with `--no-faults`, and compare:

  ```sh
  hydo report --paths hydo-out/faults --against hydo-out/clean
  ```

- **When it fits.** After an exploration that found nothing, or before one, to choose the faults
  that reach the error handling that no run tested.
- **What it found.** Blnk (the results page's section "Which error paths the faults reached"): of
  1,326 places, 73 ran in 12 schedules with faults and 27 without them. The kills of PostgreSQL
  reached 17 places that no other schedule reached. 1,253 places in 606 functions never ran.
- **What it misses.** A line without an instruction of its own, such as a Rust `?` whose error
  needs no conversion. It says that a branch ran, not that the branch did the right thing.
- **Cost.** The Mac engine only. A run with the plan has the fingerprint of the run without it.
  The engine searched 1,536 MiB of guest RAM in 10 ms per run and stopped once per place that
  ran.

### Function fuzzing (`hydo fuzz`)

- **What it is.** `hydo fuzz` tests each contracted Rust function natively, with inputs from its
  `requires` and `input` lines, and judges each call by `ensures` (`hydo guide fuzz`).
- **What to write.** The contract's comments, then `hydo fuzz path/to/crate --fn find`. Add
  `--vm` to make the calls inside hydo's VM under faults.
- **When it fits.** A library function over integers, strings, bytes, lists, or enumerations,
  such as a parser, an encoder, or a distance. Also a trait's method on a
  type that `with Self` names, and a method of a type that a `build` line constructs, such as
  bytes' `Bytes`. Inside the VM: a function that writes files, measures a timeout, or waits for a
  thread that it starts.
- **What it found.**
  - contracts-ledger: the planted bug from the contract alone, on 96 of 1,000
    inputs.
  - Qdrant's `decompress_sequence` panics on a header that `compress_sequence`
    never writes.
  - memchr with a planted bug: its own contract broke on 10 of 1,000 calls.
  - num-integer's `gcd` gives the known rust-num/num-integer#55.
  - strsim's `sorensen_dice` divides by byte lengths. httparse's
    `parse_chunk_size` accepts `\r\n` as size 0. num-traits' `abs_sub` panics in a debug build.
  - A contract states httparse's chunk size rule on the
    parser's `Status` result. num-traits' `Signed` methods and byteorder's `read_uint` break
    on the trait methods themselves, with no wrapper function.
  - Inside the VM with `hydo fuzz --vm` (on hvx and the Mac engine), a
    save by rename without a sync lost its text at every power cut. redb's commits held.
  - A fault inside one call: a planted wait whose deadline reads `SystemTime`
    broke under a forward clock step drawn 5 ms after the call's start. A planted count that
    waits for its worker with a timeout broke under a pause drawn 2 ms after it. rs-snowflake 0.6.0's
    `real_time_generate` gave a smaller id after a backward step of 30 s. Each twin held, and
    so did crossbeam-channel's `recv_timeout`.
- **What it misses.** Generic structures, `char`, maps, and constructors that return an
  `Option` are not fuzzed. None of the candidates was posted. Inside the VM, one
  call per run gets a fault inside it, so a function of time or threads needs `--fn` and
  `--faults`.
- **Cost.** 1 to 15 s per build and run, with no VM. Inside the VM, 76 to 251 s for 16 to 24
  runs.

## Measured without a gain

Each of these was measured against the plain exploration. The first three are options of `hydo
explore`. The last two were experiments of the first explorer, before it was written in Rust.

- The steered search (`--steer`): on KVM over five seed ranges it found Qdrant's
  failure after a median of 118 schedules, and the plain exploration after 72. On hvx neither
  found it.
- The state search (`--search state`): it found the benchmark's d7 and d10 in 2 of
  5 seed ranges, and the plain exploration in 3 of 5. Neither found etcd 14685's second symptom.
- Reactions drawn on the bug benchmark: no defect came sooner (the entry on
  reactions).
- Coverage-guided exploration: random schedules found an old bug of the
  order service at runs 1 and 13. The guided search found it at run 14, and once not within 30.
- Branch points from assertions: 53 features and 6 signatures. Random schedules
  gave 59 features and 11 signatures.

## Add an entry

After an onboarding, add its evidence here, in the same shape: what it is, what to write, when it
fits, what it found, what it misses, and the cost. Every number needs its source in the
example's results page or in the project's history, but the entry says it in words, because a
release ships neither file. `docs/ONBOARDING-PLAYBOOK.md`, section 8, asks each retro for it.
