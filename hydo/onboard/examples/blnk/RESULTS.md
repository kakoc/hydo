# Business rules for a ledger with a queue, Blnk under hydo

This onboarding is the third experiment in finding business bugs with hydo. A business bug
breaks a rule of the system's domain, for example a payment that goes past its credit limit. The
first two experiments were Formance Ledger (`../formance-ledger/RESULTS.md`, Milestone 215) and
Lago (`../lago/RESULTS.md`, Milestone 218). This one is Blnk, an open source ledger in Go on
PostgreSQL and Redis. Blnk saves each transaction as `QUEUED`, and a separate worker process
applies it later through a Redis queue. A transaction can also be an inflight hold, which
reserves money until a commit or a void.

The experiment reused what worked twice. It gave Blnk two kinds of rules and recorded which kind
found what:

1. The domain's own rules, written as SQL queries over Blnk's tables by someone who knows them.
2. One generic rule: faults must not change the business result. The client records each
   operation it saw acknowledged, and a check computes every balance from those records alone.

Error logs as findings (`errors: true`) ran beside both kinds, without a rule.

## The short answers

- **What was found?** Three things that change money, and two of them are Blnk bugs:
  1. **A Blnk bug, not known upstream.** A draw with an `overdraft_limit` ignores its limit
     when the worker processes it together with others. Five draws of
     30.00 sent at once against a limit of 50.00 all apply, and the balance ends at -150.00.
     Queue recovery applies a recovered draw without the limit too. Under hydo it needs no
     fault: concurrent draws are enough. Natively, 20 of 20 such bursts went past the limit, and
     0 of 20 with a two-line fix.
  2. **A Blnk bug, not known upstream.** An atomic bulk in the default queue mode keeps its
     first transfer when a later transfer fails before it is queued, and the answer says that
     nothing was applied. A client that gives up on a slow request triggers it, and so does a
     reused reference. Natively, 20 of 20 such bulks applied their first transfer, and 0 of 20
     with the fix.
  3. **A deployment setting.** Redis without an append-only file, as Blnk's own
     `docker-compose.yaml` runs it, loses an acknowledged commit or void of a hold when Redis is
     killed. Nothing recovers it, and the money stays held. With `redis_aof=yes` the same
     schedule passes.

  A fourth finding is a clock effect: after a backward clock step of an hour, transfers waiting
  for a lock that a killed worker left were rejected.
- **Which rule found what?** Each kind found a bug that the other kind did not see:
  - Finding 1 broke only a domain rule, the overdraft rule. The generic rule cannot know
    whether Blnk should have refused a draw.
  - Finding 2 broke the domain rule that an atomic bulk applies whole or not at all. It also
    broke the generic rule (one operation, half applied), and it showed in the error logs.
  - Finding 3 broke only the generic rule. Every domain rule held, because a hold whose commit
    never arrives is a valid, open hold.
- **What did the error logs find?** No bug alone. They showed finding 2's failed rollback, and
  they named Blnk's reactions to every fault.
- **What did hydo lack?** Four things, listed in the last section but one.

## The version

The latest release, v0.15.8 (2026-11-02), has four bugs that upstream issues 413, 418, 421 and
422 describe, all fixed on `main` since. For example, v0.15.8 answers `201 QUEUED` before it
saves the transaction (issue 418), so a crash of the API server loses acknowledged transfers.
An experiment on v0.15.8 would find those again. This example therefore pins `main` at
`67d88333` (2026-11-11), the newest commit when the onboarding started. `build.sh` builds it from
source.

## The project

| file | what it is |
|---|---|
| `build.sh` | builds `bin/blnk-<arch>` from `67d88333`, `bin/blnk-fixed-<arch>` with the two patches, and `bin/blnkc-<arch>` from `client/`: static Linux programs for amd64 and arm64, checked against six sha256 pins (on a Mac) |
| `client/main.go` | `blnkc`, the test client: every workload operation, the wait for the queue, and the generic rule (standard library only) |
| `fix-overdraft-limit.patch`, `fix-atomic-bulk.patch` | the tested fixes of findings 1 and 2, against `67d88333` |
| `hydo/hydo.yaml` | PostgreSQL 16 and Redis 7.2.4 as Blnk's `docker-compose.yaml` runs them, `blnk start` after `blnk migrate up`, and `blnk workers`, each in its own network namespace |
| `hydo/workload/` | `first_init` funds the accounts; eight `parallel_driver_` commands, one per operation (below); `eventually_1_settle` and `eventually_2_settle` wait for the queue (`helper_settle.sh`) |
| `hydo/checks/` | the rules (below): 9 SQL files over PostgreSQL and 1 program |
| `hydo/known-errors.yaml` | the reviewed error kinds |
| `hydo/regressions/` | the minimal schedules of the findings |
| `hydo/fetch-images.sh` | PostgreSQL 16.16 and Redis 7.2.4 by digest for both architectures |

```sh
sh build.sh                     # needs Go 1.27.3 and network once; prints "ok" for the six pins
cd hydo && sh fetch-images.sh
hydo build hydo.yaml --arch arm64 --set arch=arm64     # on a Mac; on Linux: hydo build hydo.yaml
hydo run "disk=none | run"
hydo explore --seeds 1-20 --strategy process --new-faults --errors-shrink 0
```

The deviations from Blnk's `docker-compose.yaml`:

- PostgreSQL is `postgres:16` there, pinned here as 16.16 in its Alpine variant.
- The worker starts after the API server, whose setup runs the migrations. In Compose it waits
  only for PostgreSQL and Redis and restarts until the tables exist. The worker also names
  PostgreSQL and Redis in `depends_on`, so that hydo has network links to aim faults at.
- The search, tracing and monitoring profiles (Typesense, Jaeger, Prometheus) are left out.
- The build parameter `redis_aof` (default `no`, as in Blnk's file) turns on Redis's
  append-only file with `appendfsync always`.

## The workload

All commands run `blnkc op <kind>`. Each operation has a reference of its own (`t17` for the
transfer of composer entry 17), and the client retries a lost answer with the same reference.
Blnk keeps references unique and answers a reused one with `409 TXN_DUPLICATE_REFERENCE`. A
retry answered that way counts as acknowledged, since an earlier attempt was saved.

The accounts are balances with indicators:

- four users with 5,000.00 each;
- two shops with 20.00 each;
- two credit lines;
- the merchant that holds are for;
- one empty account.

 Amounts are in
cents of USD.

| command | operation | its result, as the client knows it |
|---|---|---|
| `deposit` | `@world` to a user, or to a shop as a sale | applies |
| `transfer` | a user to another user, 0.01 to 5.00 | applies: users never run short |
| `spend` | a shop to a user, 3.00 to 9.00 | applies or is rejected: shops run short |
| `draw` | a credit line to a user, 10.00 to 30.00, with `allow_overdraft` and `overdraft_limit` 50.00 | applies or is rejected |
| `repay` | a user to a credit line | applies |
| `overdraw` | the empty account to a user, 1.00 | is rejected |
| `hold` | an inflight hold from a user for the merchant; a partial commit; then a commit of the rest or a void, each with its own reference | applies, then each step applies |
| `bulk` | two transfers in one atomic bulk; one time in three the second comes from the empty account | both apply, or neither does |

`eventually_1_settle` runs after the faults heal. It waits until every saved transaction has a
final state and every acknowledged commit or void has its transaction. When transactions stay
`QUEUED`, it runs Blnk's documented queue recovery (`POST /transactions/recover`, threshold 2
minutes) up to four times. Blnk runs the same recovery by itself every 30 minutes for
transactions that are 2 hours old, and rejects a transaction after 3 failed attempts. Both the
threshold and asynq's retries go by Blnk's clock, so the wait first computes how long they need
after a backward clock step. `eventually_2_settle` repeats the wait, so that the two commands
cover a step of an hour back.

## The rules

The rules come from Blnk's documentation: the pages on the transaction lifecycle, inflight
commits and voids, overdrafts, queue recovery, and the error codes. The domain rules are numbered
1.1 to 1.7.

| id | rule | file |
|---|---|---|
| 1.1 | every transaction balances: per currency the balances sum to zero, credits equal debits, and held amounts sum to zero | `final_balanced.sql` |
| 1.2 | a balance is the sum of its applied transactions, and its held amounts are the sum of its open holds | `final_balance_is_sum.sql`, `final_inflight_is_sum.sql` |
| 1.3 | a queued transaction ends exactly once: one copy that is `APPLIED`, `INFLIGHT` or `REJECTED` | `final_queued_once.sql` |
| 1.4 | a hold never commits more than it held, is voided at most once, and its void releases exactly what was still held | `final_holds.sql` |
| 1.5 | a transaction with a reference is recorded once, also after a client's retry | `final_reference_once.sql` |
| 1.6 | overdraft rules hold: users, shops, the merchant and the empty account never below zero, counting held amounts; a credit line never below -50.00 | `always_no_overdraft.sql` (every 2 s), `final_no_overdraft_after.sql` |
| 1.7 | an atomic bulk applies whole or not at all | `final_bulk_atomic.sql` |
| 2 | faults do not change the business result: the client's acknowledged operations, and the unanswered ones at most once, give every balance in PostgreSQL and through the API | `final_business_result` (`blnkc check model`) |

Rule 2 knows nothing of Blnk's tables. It reads Blnk's transactions only to learn which
operations took effect, by their references. It then adds up the client's own amounts and
compares the credit, the debit, and the held volumes of every balance. It also counts an
operation that took effect in part, such as a bulk with one of its two transfers. And it counts an
operation whose result the client knew in advance and did not get. For example, a transfer between users must apply, and a
transfer from the empty account must not.

Each SQL rule was checked against a deliberate break. A `hydo inspect` session at the end of an
hvx run ran each query after one change, inside a transaction that was rolled back, with Blnk's
triggers off:

- a balance changed by one cent (rules 1.1 and 1.2), or a held amount changed by one cent (1.2);
- a `QUEUED` row without its copy (1.3);
- a commit larger than its hold (1.4);
- two applied rows for one client reference (1.5);
- a shop below zero (1.6);
- the copy of a bulk's second transfer deleted (1.7).

Each query returned the broken rows, and without the change only rule 1.6 returned a row: that
run was finding 1.

## Without faults, and the regressions

The final image held every rule in `disk=none | run`, twice on each engine with one fingerprint:

| engine | image | fp | ctl |
|---|---|---|---|
| the Mac engine, this tree's hydo | `explore-ob-blnk-arm64.cpio.gz`, sha256 `c414207f…` | `c8645a2ab6f1b90f` | `eeef348591487783` |
| hvx, the published 0.3.1 | `explore-ob-blnk-disk.img`, sha256 `b2413e32…` | `35f37b7eb11da9ed` | `da6934ddfe37eb54` |

The run starts 58 commands, and the client sees 76 operations acknowledged: a hold and its two
steps count as three. `hydo repro --regressions` gives 3 of 3 as expected on both engines. Each regression
expects a violation:

| regression | finding | Mac fp | hvx fp |
|---|---|---|---|
| `overdraft-limit-concurrent-draws` | 1 | `d355ad5b8c80d6dc` | `55dd2d8e07509138` |
| `bulk-partial-after-redis-pause` | 2 | `17d0b70c0a9d3401` | `d29b5349b6de6685` |
| `commit-lost-with-redis` | 3 | `eea2761315f9d472` | `fc95545652d3e657` |

`hydo build hydo.yaml --set blnk=blnk-fixed` builds the image with both patches.

## What was found

A finding is a Blnk bug only when its cause is in Blnk's code or a documented promise is broken.
Each finding below gives the schedule, the cause in three steps, and the decision.

### 1. Draws processed together ignore their overdraft limit (a Blnk bug)

Found by rule 1.6 alone, on hvx, in the swarm exploration's seed 103: `disk=none
dseed=d58b34f948600a42 | run K=redis@11+3 P=server@15~389:24650 T=13:300 W=6b0f433c,p6,q2,r800`.
The credit line `@c2`, whose draws all carry `overdraft_limit` 50.00, ended at -119.37. A look
inside the run with `hydo inspect exec` and `psql` showed two ways in. Three draws that stayed
`QUEUED` while Redis was killed were applied later by the queue recovery. Three draws sent within
one second were applied together. The explorer shrank it to one fault, a pause of
the API server for 24.65 s: `disk=none dseed=d58b34f948600a42 | run P=server@15~0:24650
W=6b0f433c,p6,q2,r800,oparallel_driver_bulk+parallel_driver_deposit+parallel_driver_repay`. The
draws sent during the pause are saved together when the server resumes, and the worker coalesces
them. It gave `8d9703b80bc52159` on both runs. That schedule depends on timing: on the final
image it passes. The timing exploration `h2` found the bug again in seed 202 and shrank it to no
fault at all, only the composition with its concurrent draws: `disk=none dseed=62b1aba8e728944c |
run W=e48d5d25,p3,q1,r200,oparallel_driver_bulk+parallel_driver_deposit+parallel_driver_hold`
(`ff9181ad711b201c` twice). It is saved as
`hydo/regressions/overdraft-limit-concurrent-draws.schedule`, and it fails on the final image on
both engines (below). The Mac explorations `e2` and `e4` never hit the bug.

The cause, in three steps:

1. Blnk does not save `overdraft_limit`. `blnk.transactions` has no column for it, and
   `setTransactionMetadata` (`transaction_queue.go`) writes only `allow_overdraft` into
   `meta_data`. The task in Redis keeps the limit, so a draw that the worker takes alone is
   checked correctly.
2. The worker coalesces queued transactions of one source (`enable_coalescing`, on by default).
   `buildQueuedCoalescingBatchForScope` loads the other transactions of the batch from
   PostgreSQL, and `restoreTransactionFlagsFromMetadata` restores `inflight`, `atomic` and
   `allow_overdraft` only. Such a draw has `AllowOverdraft: true` and `OverdraftLimit: 0`. The
   queue recovery (`processStuckTransaction`) loads and restores a transaction the same way.
3. `canProcessTransaction` (`model/model.go`) skips every balance check when `AllowOverdraft`
   is true and `OverdraftLimit` is 0, which is the documented unlimited overdraft.

**Native, without hydo.** PostgreSQL 16 and Redis 7.2.4 in Docker, and Blnk built from
`67d88333` on the Mac. Each burst sends 5 draws of 30.00 at once with `curl`, to a new balance,
with `allow_overdraft` and `overdraft_limit` 50. At most one may apply.

| Blnk | bursts that ended below -50.00 | lowest balance |
|---|---|---|
| `67d88333` | 20 of 20 | -150.00 |
| `67d88333` with `fix-overdraft-limit.patch` | 0 of 20 | -30.00 |

The same draws one second apart apply once and are rejected four times with `transaction
exceeds overdraft limit`, on both builds. The patch saves the limit in `meta_data` next to
`allow_overdraft` and restores it with the other flags.

**Decision: a Blnk bug.** The overdrafts page says that with `overdraft_limit` a transaction
"succeeds only if the resulting negative balance stays within the limit". No upstream issue or
pull request names it. A report is drafted, not posted.

### 2. An atomic bulk keeps its first transfer when a later one fails before it is queued (a Blnk bug)

Found by rule 1.7, by rule 2 (one operation applied in part), and by the error logs. It failed
17 of the 85 explored schedules: 6 of 20 in `e2`, 7 of 20 in `e4`, 2 of 25 in `h1`, and 2 of 20
in `h2`.
The explorer shrank `e2` seed 1 to one fault: `disk=none dseed=4127cc1347fae6ba | run P=redis@9+4
W=3e71cf9e,p5,q3,r800`, a pause of Redis for 4 rounds. It gave `57b98e291741b056` on both runs
and is saved as `hydo/regressions/bulk-partial-after-redis-pause.schedule`. `e4` seed 42 shrank to
a pause of the API server alone (`P=server@22+8`): the client gave up on two bulks, and their
requests went on with a cancelled context. The error logs showed every run of it as new kinds,
for example `failed to rollback batch transactions ... Failed to retrieve refundable
transactions`.

The cause, in three steps:

1. `processBulkTransactions` (`transaction_bulk.go`) queues the items one at a time.
   `QueueTransaction` saves each item as `QUEUED` and enqueues it at once, so item 1 waits for the
   worker while item 2 is still being queued.
2. Item 2 fails before it is queued: a Redis timeout, a request context that is cancelled because
   the client gave up, or a reused reference. The synchronous failure branch of
   `CreateBulkTransactions` calls `rollbackBatchTransactions`, which refunds only items that are
   already `APPLIED`, with the same cancelled context. Item 1 is still `QUEUED`, and nothing marks
   the batch failed: only `RejectTransaction` calls `markQueuedAtomicBatchFailed`.
3. The worker takes item 1, finds no failed mark in `WithQueuedAtomicBatch`, and applies it. The
   answer said `Nothing from this batch had been applied, so nothing was refunded`. A client that
   retried got `TXN_DUPLICATE_REFERENCE` for item 1 and cannot tell that half of the bulk went
   through.

**Native, without hydo.** The same setup as finding 1. A balance gets 100.00, and one transfer
uses the reference `used`. Then an atomic bulk sends 50.00 to a new balance (a new reference) and
1.00 with the reference `used`. The answer is `409 TXN_DUPLICATE_REFERENCE` with "Nothing from
this batch had been applied".

| Blnk | bulks whose first transfer was applied afterwards |
|---|---|
| `67d88333` | 20 of 20 |
| `67d88333` with `fix-atomic-bulk.patch` | 0 of 20; the first transfer ends `REJECTED` with `another transaction in this atomic batch was rejected` |

The patch fails the batch instead of refunding only what is applied. It marks the batch failed and
takes the batch lock that the worker holds while it applies an item, both in Redis, and both
best effort. It records every item still `QUEUED` as `REJECTED` in PostgreSQL, so the worker finds the
outcome reference used and drops the task. Then it refunds what was applied, on a context that the
client's cancellation does not stop. A first version wrote only the mark in Redis. It fixed the
reused reference and the client's timeout, but not a pause of Redis, where the mark itself cannot
be written. Under hydo, the 6 failing schedules of `e2` hold rule 1.7 on the image with the
patches (`--set blnk=blnk-fixed`).

**Decision: a Blnk bug.** The bulk page promises that with `atomic` "either all transactions
succeed or all fail". Upstream issue 413 (fixed on 2026-11-10) covered an item that the worker
rejects. Open pull request 430 covers an item that the queue recovery applies. Neither covers an
item that fails before it reaches the queue. A report is drafted, not posted.

### 3. Redis without an append-only file loses acknowledged commits and voids of holds (a deployment setting)

Found by rule 2 alone, in seeds 10, 19 and 20 of `e2`, seed 45 of `e4`, and seeds 114, 118 and
119 of `h1`, after a kill of Redis or a power cut. The explorer shrank `e2` seed 10 to
`disk=none dseed=c054734a6f769762 | run K=redis@21+1 N=worker-*@14-20:delay,100ms
W=a36fcf62,p5,q1,r800`: Redis killed for one round. It gave `ce4a735a2fa0bcad` on both runs, and
the rule reported `acknowledged h15a~C913 of hold h15~u4,m,1940 is not in Blnk` and one more. It
is saved as `hydo/regressions/commit-lost-with-redis.schedule`.

1. `PUT /transactions/inflight/<id>` with `"status": "commit"` answers 200 and puts the commit
   into Redis as an asynq task (`EnqueueInflightAction`). The commit page says that "`queued:
   true` means the commit is waiting on a worker".
2. Redis runs with its defaults, as in Blnk's `docker-compose.yaml`: no append-only file, and a
   snapshot only after 60 s with 10,000 writes or longer. A kill loses every task written since
   the last snapshot.
3. The queue recovery looks for transactions that are `QUEUED` in PostgreSQL. A commit or a void
   exists only as the Redis task, so nothing recovers it, and the hold keeps the money.

The hand schedule of seed 10 with its first faults (`K=redis@21+9 N=worker-*@14-20:delay,402ms
P=worker@2+3 T=7:-86400`) ran on hvx on both images: two commits lost with the default
(`5708f8d23793c1c3`), and OK with `--set redis_aof=yes` (`54890e4e54506b25`). Every domain rule
held in these runs: a hold whose commit never comes is a valid open hold.

**Decision: a deployment setting, not counted as a Blnk bug.** Upstream issue 336 (closed by its
reporter) describes the same loss for the expiry of holds, and its conclusion was that the
queue's durability is Redis's durability. For an operator the result is still money held after
an acknowledged commit, with no error anywhere. A queued transaction survives the same fault,
because PostgreSQL has its `QUEUED` row and the recovery finds it.

### 4. A backward clock step makes the queue wait, and rejects transfers behind a dead worker's lock (a clock effect)

Found by rule 2 in `e4` seed 54 (`K=worker@3+1 P=redis@14+6 T=27:-3600`), on the final image,
whose wait for the queue covers an hour back. Two repayments from users, who never run short,
ended `REJECTED` with `failed to acquire lock: failed to acquire all locks within the wait
timeout`.

1. The worker was killed while it held balance locks in Redis. Blnk's locks have a lifetime of 5
   minutes (`lock_duration`), so another worker could take them after that.
2. The clock stepped an hour back. The worker's log shows the transfers waiting on the same
   destination's lock for 11 minutes of Blnk's clock, over 5 retries. Redis keeps a key's expiry
   as a time on the clock, which explains the wait; the lock's expiry was not read in Redis.
3. After its fifth retry (`max_retry_attempts`) the worker rejects the transfer.

In `h2` seed 210, a step of a day back left transactions `QUEUED` for longer than the wait
covers. The recovery takes them only when they are 2 minutes old by Blnk's clock.

**Decision: a clock effect, not a Blnk bug.** No Blnk promise covers a clock that runs back an
hour. A short step does happen when a machine's clock is corrected, and then a lock left by a
crash lasts that much longer.

### Not found

Under every fault of the explorations, apart from the cases of findings 1 to 4:

- Every acknowledged transfer, deposit, spend, draw and repayment was in Blnk once, and every
  unanswered one at most once. A client's retry never made a second transaction: rule 1.5 held
  in every run. In 17 of the 20 schedules of `e2`, Blnk answered a retry with
  `TXN_DUPLICATE_REFERENCE`, as documented.
- Every transaction balanced, and every balance and held amount was the sum of its transactions.
- Every queued transaction ended exactly once after the documented recovery.
- No hold committed more than it held, and every void released what was left.
- No user, shop, or merchant went below zero. The overdraft rule failed only through finding 1.

### Mistakes in this workload and its checks

- **A wait that skipped bulk items.** The first `blnkc pending` counted only `QUEUED` rows
  without a parent, and an item of a bulk has the bulk as its parent. In `e2` seed 16 a bulk item
  whose enqueue failed stayed `QUEUED`, the settle step never ran the recovery, and rules 1.3
  and 1.7 failed. With the fixed count, the same schedule passes.
- **A rolled-back transfer counted as no transfer.** The first model check left out a bulk item
  that was applied and then refunded. Both moves stay in the volumes, so it reported 4 volumes
  that differ on the first hvx run without faults.
- **A wait that ignored the clock.** The queue recovery takes a transaction only after 2 minutes
  as `QUEUED`, by Blnk's clock. asynq retries a task at a time on the same clock.
  After a backward clock step both come later, by the step. The first wait gave the recovery a
  fixed 130 s, so 3 schedules (`h2` seeds 208 and 214, `e4` seed 46) reported transactions that
  never reached a final state, and one commit "lost" that was waiting in asynq's retry set until
  the clock passed 00:00:33 again. The wait now computes both times and is split over two
  commands of at most an hour each; with it, `h2` seed 208 and `e4` seed 46 pass.
- **A bulk without its second transfer counted as half applied.** When the second transfer of a
  bulk was never saved and the first was rejected, the first `final_bulk_atomic.sql` still
  reported the bulk. It now accepts a bulk whose saved transfers have no effect.
- **A bulk retry taken as an acknowledgement.** A retried bulk answered `TXN_DUPLICATE_REFERENCE`
  for its first transfer proves only that the first transfer was saved, not that the bulk
  succeeded. The client now counts it as unanswered.

## Which rule found what, and what it cost

"Rules written" counts the files in `hydo/checks/` for the rule, and their lines without comments.
The client (operations, records, retries, the wait, the check) is 1,001 lines of Go, and the
operations and records part, about 600 lines, is shared by every rule. Every rule ran in every
run of the explorations below.

| rule | findings | rules written |
|---|---|---|
| every transaction balances (1.1) | none | 1 SQL file, 21 lines |
| a balance is the sum of its transactions and holds (1.2) | none | 2 SQL files: 11 lines, 15 lines |
| a queued transaction ends exactly once (1.3) | none; one false failure from the wait's mistake, fixed | 1 SQL file, 8 lines |
| holds (1.4) | none | 1 SQL file, 13 lines |
| a reference is recorded once (1.5) | none | 1 SQL file, 14 lines |
| overdraft rules (1.6) | finding 1, alone | 2 SQL files, 5 lines each |
| atomic bulks (1.7) | finding 2 | 1 SQL file, 14 lines |
| faults do not change the business result (2) | findings 2, 3 and 4; findings 3 and 4 alone | 1 program: 196 lines of Go for the check and 184 for the records and the rows, and 4 lines of shell |
| error logs as findings (`errors: true`, no rule written) | finding 2's lines, beside the rules | 1 line in `hydo.yaml`, 15 reviewed entries |

The wait for the queue is not a rule, but every rule depends on it. Without it, a transaction
still `QUEUED` at the end looks lost. It is `helper_settle.sh` (48 lines of shell), its two
commands, and `blnkc pending` (17 lines of Go).

## The three experiments side by side

| | Formance Ledger (M215) | Lago (M218) | Blnk (this page) |
|---|---|---|---|
| the system | a double-entry ledger in Go on PostgreSQL; each request is one database transaction | usage billing in Rails with a Sidekiq worker and a clock process; PostgreSQL and Redis | a ledger in Go: an API process saves and a worker applies through a Redis queue; PostgreSQL holds the books |
| bugs in the system's code | 1, new: two first uses of one account in two assets fail one with `500` | 1, known (lago-api 6901): a subscription cancelled while Redis is down is never invoiced | 2, new: the overdraft limit of coalesced or recovered draws; the atomic bulk that keeps its first transfer |
| found by the domain rules | not the bug; the client mistake and the losses only by rule 1.5, which reads the client's records | 6901, and the documented losses | both bugs: the overdraft rule found finding 1 alone, the bulk rule found finding 2 |
| found by the generic rule | the client mistake (retries without keys) and the losses on a lying disk | everything the domain rules found, and usage dropped after a clock step | finding 2, and finding 3 alone |
| found by the error logs | the bug, as the only signal | nothing alone | finding 2's failed rollback, beside the rules |
| domain rules that never failed | 7 of 8 | 5 of 7 | 5 of 7 |

What this says about business bugs, after three systems:

- A domain rule finds a bug where the system enforces the rule across processes. Blnk checks an
  overdraft limit in the worker, on a transaction that has passed through PostgreSQL and Redis.
  It keeps a bulk atomic across the API, Redis, and the worker. Faults and concurrency
  break both. Formance enforces its rules inside one database transaction, and none of its
  rules over the ledger's state ever failed.
- The generic rule is the only one that sees a lost acknowledgement. Formance's lost payments,
  Lago's dropped usage and Blnk's lost commits were all valid states by every domain rule.
- The generic rule cannot judge a decision the system makes itself. Whether a draw is within its
  limit is Blnk's decision, so the generic rule passed every run of finding 1. A rule that
  states the limit was needed.
- The error logs found a bug alone once in three systems, at the cost of reviewing every new
  kind: 15 entries here, 31 for Formance.

## The explorations

Every exploration used one worker. The Mac ran this tree's hydo, and the bare-metal machine ran
hvx with the published 0.3.1. Every failure was run again and gave the same verdict. The first
failure of each signature was shrunk, and its minimal schedule ran twice with one fingerprint.

| name | engine | strategy, seeds | schedules | failures, by cause |
|---|---|---|---|---|
| `e1` | Mac | `process --new-faults`, 1-20 | 4, then stopped | the first model check's mistake with refunded bulk items; not counted |
| `e2` | Mac | `process --new-faults`, 1-20 | 20 (962 s) | 9: finding 2 (6), finding 3 (2), the wait that skipped bulk items (1) |
| `h1` | hvx | `swarm --new-faults`, 101-125 | 25 (2,502 s) | 15: finding 1 (1), finding 2 (2), finding 3 (3), PostgreSQL did not start after disk I/O errors (4), the API server did not restart after them (1), a lying disk (4) |
| `e3` | Mac | `swarm --new-faults`, 21-40 | 2, then stopped | 2 disk classes; the shrinks of disk faults took the time, so the swarm ran on hvx only |
| `h2` | hvx | `process --new-faults --timing --preempt`, 201-220 | 20 (2,165 s) | 8: finding 1 (1, shrunk to no fault), finding 2 (2), finding 3 (2), the wait that ignored the clock (2), a day's clock step back (1, finding 4) |
| `e4` | Mac | `process --new-faults`, 41-60 | 20 (744 s) | 10: finding 2 (7), finding 3 (1), the wait that ignored the clock (1), finding 4 (1) |

`e2`, `h1` and `h2` ran before the last fixes of the wait and the bulk rule; their failures were
run again on the final image where the table says so. The directed runs came on top:

- the schedules of findings 1 to 3 on the image with the patches, 13 runs on both engines;
- finding 3's schedule with Redis's append-only file, on hvx;
- the deliberate breaks of the SQL rules, in one `hydo inspect` session.

No run lost an acknowledged transfer to PostgreSQL with the honest disk. The disk faults that
broke PostgreSQL itself (I/O errors, a lying disk) are faults a team would turn off or accept.

## What hydo lacked

In the order in which they cost time. Each is an open item in `AGENTS.md`.

1. **A failing `.sql` check's verdict carries no rows.** Every violation of a SQL rule read
   `db: 2 rows where none may be`: in the verdict, in `result.json`, and in the report. It was so
   on hvx with the published 0.3.1, and on the Mac with this tree's hydo. Milestone 192 appends the first rows
   to that line, but here they never reached it. Each explanation needed a `hydo inspect` session
   with `psql` at the end of the run, 30 s to 5 minutes each.
2. **Network links come only from `depends_on`.** Blnk's worker talks to PostgreSQL and Redis,
   but Blnk's Compose file names only the API server's dependencies. Without `depends_on` on the
   worker, the fault space had no link from the worker to PostgreSQL or Redis, so no network
   fault could aim there. The example names them by hand.
3. **The Linux release builds a `network: bridge` project only after `hydo setup`.** On the
   bare-metal machine, `hydo build` of the unpacked 0.3.1 release stopped with `HYDO_IPROUTE2 not
   found at /nix/store/...-iproute2-6.8.0`. The release's `guest/store.tar` holds that path, and
   `hydo setup --offline` unpacks it, but the message does not name `hydo setup`.
4. **Error kinds split by an operation's name.** Blnk's lines name the transaction's reference
   (`reference d5_q has already been used`), and the template keeps the letter. So each type
   of operation made its own error kind. `known-errors.yaml` uses regular expressions for these.

One more was in the machine's setup and is not an open item: `/dev/hvx` on the bare-metal
machine allowed only root that day, so every hvx run went through `sudo` with `HOME` kept.

## Time

About four hours of wall time, from the first native run to the last exploration. The first
hour went to reading Blnk's code and documentation, the native probes that found findings 1 and
2 before any hydo run, and the client. A run without faults takes 5 s on either engine; a run
whose wait for the queue covers an hour of Blnk's clock takes up to 2 minutes on hvx.

Finding 1 was first seen outside hydo, while the rules were being written: in the code of
`restoreTransactionFlagsFromMetadata`, and then in a native probe of five draws at once. The
explorations found it blind through the overdraft rule. Findings 2, 3 and 4 were first seen in
explorations; finding 2 was then read in the code and reproduced natively.
