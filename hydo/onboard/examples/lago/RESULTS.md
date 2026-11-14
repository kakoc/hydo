# Lago: results

This onboarding is an experiment in finding business bugs, not only infrastructure bugs. The
project is **Lago v1.57.0**, open-source usage-based billing: a Rails API, a Sidekiq worker and
a clock process (clockwork) on PostgreSQL 15 and Redis 7. A customer's integration sends usage
events to the API, and Lago counts them per subscription. It bills them in one of two ways. A
charge "in arrears" is on the invoice at the end of the billing period. A charge "paid in
advance" is billed at once, on an invoice of its own for each event. The system was given two
kinds of business rules, and this page records which kind found what:

1. **Billing invariants.** Rules about Lago's own books, written as SQL queries or short shell
   checks by someone who knows Lago's tables.
2. **"Faults must not change the business result."** One generic rule. The workload records
   every operation the client saw acknowledged, with the total it adds to and the amount. The
   check compares those records alone with what Lago reports per total. It knows nothing about
   billing.

Everything ran on **this Mac with the Mac engine**: an M2 Pro, arm64 guests, and hydo 0.3.1 built
in this tree with the published 0.3.1 release files. The bare-metal machine was busy with
another agent's measurement campaign for the whole session, so no run used hvx.

## The short answers

- **Did a rule find a bug in Lago's code?** One, and it is known upstream: **a subscription
  that is cancelled while Redis is down is terminated but never invoiced** (lago-api issue
  6901, open). Lago commits the termination to PostgreSQL, cannot enqueue the job that bills it,
  and answers 500. The client's retry gets 404, and nothing bills that period later. The
  exploration found it without being told, in a swarm schedule that killed and paused Redis. It
  reproduces with Docker and without hydo in five requests (finding 1). The re-run after
  Milestone 225 found a second bug, with lost database answers and a clock step to the billing
  tick: **a subscription whose cancellation is still uncommitted at the hourly billing run is
  invoiced twice for one period**. No upstream issue describes it. It reproduces with Docker and
  without hydo when the database's answers to the termination are held for 16 minutes across
  the billing run (finding 8).
- **What else did hydo find that a Lago operator should know?** None of these is counted as a
  Lago bug:
  1. A power cut loses acknowledged usage that is charged in advance. The event is in
     PostgreSQL, but the job that charges it was in Redis or in the worker, and nothing makes it
     again. Lago's own `docker-compose.yml` runs Redis without an append-only file. With one,
     some fees still go missing, because Sidekiq's open-source edition loses the jobs that a
     worker had fetched. Lago's worker says so in its log at every start.
  2. A Redis outage in the first seconds of a worker's life stops Sidekiq's scheduler thread
     for good. Every retry and every delayed job of that worker waits forever, so fees whose
     first attempt met a busy lock are never made. The cause is in Sidekiq 7.3.10, which Lago
     uses, not in Lago.
  3. When the clock does not reach a billing day, that period is never invoiced: a forward step
     of one day here, an outage of the clock process in production. This is Lago issue 354,
     open since 2024.
  4. Lago orders usage, terminations, and subscription starts by its own wall clock. After a
     backward clock step of 30 s or more, usage that Lago had already acknowledged can fall
     after a termination or before a subscription's start, and it is never billed.
- **Which rules found more?** The generic rule. In the first exploration it failed in all
  11 runs that broke a business result, and the billing invariants failed in 8 of them. The 3
  others were usage dropped by a termination after a clock step, which no Lago-specific rule
  covered. In the second exploration both kinds failed in the same 3 runs. The billing
  invariants named the mechanism whenever they failed ("an acknowledged event has no fee", "a
  subscription has no November invoice"). The generic rule said only which total was short, and
  by how much.
- **What did the consistency rules find?** Nothing. An invoice's total always equals the sum of
  its fees. No subscription got two periodic invoices for one period (the rule does not count
  the termination invoice of finding 8), no event was charged twice, no
  invoice stayed in a state that is not final, and no acknowledged event was missing from
  PostgreSQL. Those rules held in every run.

## The system under test

| file | what it is |
|---|---|
| `docker-compose.yaml` | Lago's own `docker-compose.yml` of v1.57.0, cut down to the billing core (below) |
| `hydo/hydo.yaml` | from `hydo compose ../docker-compose.yaml --out hydo.yaml --images-dir images`; hand changes are marked `hand:`. `hydo.generated.yaml` is the raw import |
| `hydo/workload/`, `hydo/checks/` | the commands and checks below; `helper_sql.conf` from `hydo sql-gen --dialect postgres --endpoint 'db=postgres://lago:changeme@10.89.0.11:5432/lago?sslmode=disable' --workloads none` |
| `hydo/fetch-images.sh`, `hydo/hydo-compose.lock.json` | from `hydo compose`: the three images by digest |

```sh
cd hydo && sh fetch-images.sh            # the three images by digest (network, once)
hydo build hydo.yaml                     # about 4 minutes; the root takes 680 MiB of guest RAM
hydo run "disk=none | run"
hydo explore --seeds 1-20 --strategy process --new-faults --workers 1
hydo build hydo.yaml --set redis_aof=yes # the variant with Redis's append-only file
```

**The cut.** Lago's file runs eight services. This example keeps six:

- `db`: `getlago/postgres-partman:15.0-alpine`;
- `redis`: `redis:7.4.7-alpine` (Lago's file says `redis:7-alpine`);
- `migrate`: the one-shot migration;
- `api`, `api-worker` (one Sidekiq worker for every queue) and `api-clock`.

It leaves out the web front end (`front`, a user interface) and the PDF renderer (`pdf`,
Gotenberg; `LAGO_DISABLE_PDF_GENERATION=true`). The full stack would have fit. The guest has
4,096 MiB, and `root.share_images: true` makes the four services of the API image share one
unpacked tree (680 MiB of guest RAM instead of 2 GiB). The two services were left out because no
rule needs them.

**What Lago's file reads from the shell** is written out in `docker-compose.yaml`. The
organization and its API key are created by the migration (`LAGO_CREATE_ORG=true`).
`LAGO_RSA_PRIVATE_KEY` is a throwaway key made for this example with
`openssl genrsa 2048 | base64`.

**The open-source edition only.** In Lago's open-source edition, these are premium features:
manual payments, credit notes made by hand, customer time zones, and the invoice grace period.
The API answers 403 without a license that Lago's license server confirms. The rule "a payment
or credit note never exceeds the invoice" therefore could not be exercised. The open-source
edition makes credit notes only when a subscription that was paid in advance ends, and this plan
is billed in arrears.

**Hand changes** besides the workload and the checks:

- `guest.ram_mib: 4096` (the importer guessed 1,792 MiB) and `root.share_images: true`;
- `guest.clock_start` at 2026-11-30 23:55 UTC, so that every run starts five minutes before the
  end of the billing period, `clock_after_cut: 60` and `outcomes_after_cut: true`;
- the workload folder mounted into the API image, so the client runs with the image's Ruby;
- the build parameter `redis_aof` (default `no`, as in Lago's file);
- since hydo's fixes of the gaps below (Milestone 225, section "The re-run without the
  workarounds"): `test.first_records: true`, `job: true` on `migrate`, and `clock_points:`.

## The workload

All commands use one Ruby client, `helper_client.rb`, run with the API image's own Ruby
(`hydo-oci api ruby /hydo/workload/helper_client.rb ...`). It speaks Lago's public REST API with
the organization's API key, as a customer's billing integration does.

- `first_seed` (inside the snapshot): two billable metrics that sum a `units` property,
  `api_calls` and `transfers`. A monthly plan "usage" with a base fee of 10 USD, `api_calls` in
  arrears at 0.10 USD a unit, and `transfers` paid in advance at 1 USD a unit. Four customers,
  each with a subscription (`sub1` to `sub4`) that started on 2026-11-01, billed on the calendar
  month. Two November events of `api_calls` per subscription.
- `parallel_driver_event`: one event for `sub1` to `sub3`, with a transaction id of its own.
  The client retries a lost answer up to three times with the same id. Lago documents
  `transaction_id` as the deduplication key. An answer `value_already_exist` therefore means
  that an earlier attempt was stored, and the client counts it as acknowledged.
- `parallel_driver_batch`: three events in one `POST /api/v1/events/batch`, retried the same way.
- `anytime_redeliver`: an at-least-once producer sends one earlier event again, with the same
  id, also while the first delivery may still be in flight.
- `serial_driver_terminate`: the customer `cust4` cancels, and `sub4` ends now. Lago bills it on a
  termination invoice. No event goes to `sub4` during the run.
- `eventually_1_billing_tick`: after the faults, the clock moves forward to 2 s before Lago's
  next billing tick: minute 10 of the hour, and at the earliest 2026-12-01 00:10. Lago's clock
  service then starts the biller. A run lasts less than a minute of guest time, so this is the
  step that waiting would make. Since Milestone 225 the command only moves the clock. The two
  checks `checks/eventually_november_billed` and `checks/eventually_transfers_charged` wait for
  the billing, from the end of this command (`after=commands`).

Until Milestone 225 the command also waited up to 150 s for the November invoices, and a second
command, `eventually_2_transfers_charged`, waited up to 120 s for the fees. The findings below
name those commands as the runs reported them.

Every operation is recorded as `<op>,<key>,<amount> ack|failed|unknown`, for example
`e17,units:sub1:transfers,3 ack`. The key is the total that the operation adds to. A timeout,
a lost answer, or a server error is `unknown`. A refusal (HTTP 400, 404, or 422 other than the
duplicate) is `failed`.

## The rules

| rule | kind | the rule | how it is read |
|---|---|---|---|
| `final_acked_events_stored` | billing | every acknowledged event is stored once under its transaction id | the client's ids against `events` (psql) |
| `eventually_transfers_charged` (until Milestone 225 the command `eventually_2_transfers_charged`) | billing | every acknowledged event of the charge paid in advance gets its fee within 120 s of the billing tick | the client's ids against `fees.pay_in_advance_event_transaction_id` |
| `final_one_fee_per_transfer.sql` | billing | an event is charged in advance at most once | SQL over `fees` |
| `final_invoice_total_is_sum_of_fees.sql` | billing | a finalized invoice's fees amount is the sum of its fees, and its total is that minus coupons and credits plus taxes | SQL over `invoices` and `fees` |
| `final_one_invoice_per_period.sql` | billing | a subscription is never invoiced twice for one billing period | SQL over `invoice_subscriptions` |
| `eventually_november_billed` (until Milestone 225 inside `eventually_1_billing_tick`), `final_november_invoiced.sql` | billing | every subscription's November usage is on a finalized invoice (periodic, or the termination invoice) within 150 s of the billing tick | SQL over `invoice_subscriptions` and `invoices` |
| `final_invoices_final.sql` | billing | every invoice ends finalized, voided, or closed | SQL over `invoices` |
| `final_business_result` | generic | every total Lago reports equals the acknowledged operations. Acknowledged amounts must be there once, unanswered ones at most once, and refused ones not at all | the outcome lines against `helper_client.rb report` |

The generic rule is `workload/helper_ledger.awk` (45 lines). It reads nothing but the outcome
lines and a list of `<key> <amount>` lines. For every key, the reported total minus the
acknowledged sum must be the sum of some set of unanswered operations. The only part that knows
Lago is `report` in the client (35 lines). It adds up, per subscription, the units of every fee
on every invoice that is not voided, plus the current period's usage of `api_calls`
(`GET /customers/{id}/current_usage`).

## Without faults

The final image (`explore-ob-lago-arm64.cpio.gz`, sha256 `af496f66fa150a12…`; data disk
`2d0069a698397d61…`), `disk=none | run`, twice. Both runs gave OK `afa9548209205e53` (ctl
`74fc1e065d6b1653`). The image before the three regressions were added (`00e71899…`, the one
exploration B and the hand schedules ran on) gave `3527dee6c263cb64` twice.

`hydo repro --regressions` on the final image: 3 of 3 as expected, each a violation.
`terminate-while-redis-down` (finding 1) gives `a0ef6fa0e0e59be6`, `power-cut-loses-queued-fees`
(finding 2) `d10bc4a5dfc8771c`, and `skipped-billing-day` (finding 4) `18d87f84267b2019`. Each ran 18 composed commands and the 2 eventually commands, with 22
acknowledged operations of the run and the 8 of the seed. Every rule held. A run takes about
30 s of engine time. A worker's boot to its snapshot takes 60 s of wall time, for Lago's 769
migrations, three Rails boots, and the seed.

## What was found

Each finding below gives the schedule, the cause in three steps, and the decision: a bug in
Lago's code, a known or documented behaviour, or a mistake in this workload or its checks. A
finding is a Lago bug only when the cause is in Lago's code or a documented promise is broken.
Only finding 1 qualifies.

### 1. A subscription cancelled while Redis is down is never invoiced (a Lago bug, known upstream: lago-api issue 6901)

Confirmed upstream on v1.57.0, with the steps below and the uniqueness-lock detail: https://github.com/getlago/lago-api/issues/6901#issuecomment-6864157424 (2026-11-13).

Found by `eventually_1_billing_tick`, `final_november_invoiced` and `final_business_result` in
the second exploration's seed 36: `disk=none dseed=3fbd7e58fbfa188e | run K=redis@4+4
N=db-api@6-9:delay,20ms,reorder,29% P=redis@0~10:18788 W=b6bdf82a,p1,q2,r750`. It ends with
`150 s after the tick: 1 subscriptions without a November invoice`, and the generic rule
reports `units:sub4:api_calls: reported 0, acknowledged 12`. The hand schedule
`disk=none | run K=redis@1+4` is the minimal form: Redis is down while the default composition
cancels `sub4` in round 2. It gave `30be47e1ed697d48` twice and is saved as
`hydo/regressions/terminate-while-redis-down.schedule`.

1. `Subscriptions::TerminateService` marks the subscription terminated inside
   `subscription.with_lock`, and the transaction commits.
2. `BillSubscriptionJob.perform_after_commit` then raises: the job's uniqueness lock needs
   Redis (`Redlock::LockAcquisitionError ... RedisClient::CannotConnectError`). The API answers
   500 with the termination already saved.
3. The client's retry gets 404 `subscription_not_found`. The periodic biller selects only
   active subscriptions, and nothing looks for a terminated subscription without its
   termination invoice. That period's base fee and usage are never billed.

The same five requests reproduce it with Docker and without hydo, on Lago's own
`docker-compose.yml` (front end and PDF renderer left out). Two subscriptions each get one event
of 12 units. `sub2` is terminated with Redis up: 200, and a finalized termination invoice. Then
`docker compose stop redis`, and `DELETE /api/v1/subscriptions/sub1` answers 500 after 6 s.
After `docker compose start redis`, the retry answers 404, `sub1` is `terminated`, and
`GET /api/v1/invoices?external_customer_id=cust1` stays `[]`, also 60 s later.

**Decision: a Lago bug.** Lago's documented default (`on_termination_invoice: generate`) is that
a terminated subscription gets its final invoice. Lago-api issue 6901 (opened 2026-10-26, on
v1.55.0) reports the same cause, so no new issue is needed. A comment that confirms it on v1.57.0
with these steps is drafted, not posted. Lago fixed the same pattern for events in lago-api pull
request 6319 (an event whose job cannot be enqueued is deleted), not for terminations.

### 2. A power cut loses acknowledged usage that is paid in advance (documented limit)

Found by `eventually_2_transfers_charged` and `final_business_result` in 4 of 20 schedules of
the first exploration (seeds 6, 8, 17, 20), each a power cut `C=10` with `disk=cut`. Seed 8 is the
plainest: `disk=cut dseed=629bfb2777cb6c08 | run C=10 P=api_worker@5+5 T=0:300
W=340bcd65,p1,q1,r3000` ends with `4 of 9 acknowledged transfers have no fee 120 s after the
faults`. The four events are in PostgreSQL. In the recovery boot, Redis's queues, schedule, retry
and dead sets are empty, and the worker's log never names them.

1. The API commits the event to PostgreSQL, pushes `Events::PostProcessJob` to Redis, and
   answers 200. The fee is made later, by that job and two more (`Events::PayInAdvanceJob`,
   `Invoices::CreatePayInAdvanceChargeJob`).
2. A power cut loses the jobs that Redis held only in memory: Lago's `docker-compose.yml` runs
   Redis without an append-only file. It also loses the jobs that a worker had already taken:
   Sidekiq's open-source fetch removes a job from Redis when a worker starts it.
3. Nothing in Lago looks for events without their post-processing, so the fee is never made. A
   charge in arrears would still be counted, because the periodic invoice reads the events
   table; a charge paid in advance is not billed again at the end of the period.

The same four schedules on the variant with Redis's append-only file (`--set redis_aof=yes`,
`appendfsync always`) separate the two causes. Seed 8, whose worker was paused while the jobs
waited in Redis, then passes. Seeds 6 and 17 still lose fees (8 of 17 and 3 of 8): those jobs
were in the worker. In seed 20 the worker did not come back in the recovery boot; that was not
analysed.

**Decision: a documented limit, not counted as a bug.** At every start, Lago's worker logs
`Sidekiq Pro is not installed. Reliability features will not be available.` The pull request
that made the API delete an event whose job it could not enqueue (lago-api 6319, in 1.57.0) says
that it "narrows the window rather than closing it: a process killed between commit and cleanup
still strands the event". Redis's durability is a deployment setting. For an operator, the
result is still lost revenue: an acknowledged event that is never charged, without an error
anywhere.

### 3. A Redis outage early in a worker's life stops Sidekiq's scheduler (a Sidekiq bug)

Found by `eventually_2_transfers_charged` in the hand schedule `disk=none | run T=0:900
N=redis@1-4:loss,100%` (Redis unreachable from every service for rounds 1 to 3): `2 of 11
acknowledged transfers have no fee 120 s after the faults`. `N=redis@1-5:loss,100%` and
`P=redis@1+4` without the clock step fail the same way and were not looked into further. The
two events without a fee are in PostgreSQL. Their
`Invoices::CreatePayInAdvanceChargeJob` ran and failed with `BaseLockService::FailedToAcquireLock`
(`Failed to acquire lock customer-...-credit_note`): concurrent jobs of one customer take one
lock, and the job retries with `retry_on ... wait: random_lock_retry_delay` (0 to 16 s). The
retries are in Sidekiq's `schedule` set, due at 01:10:05 and 01:10:09 of the guest's clock,
and they are still there at 01:14:58. The worker's log has `#<Thread:...@sidekiq.scheduler ...> terminated with
exception`, from `Sidekiq::Scheduled::Poller#cleanup`, called by `#initial_wait`.

1. Sidekiq's scheduler thread first sleeps 5 to 15 s, then calls `cleanup`, which talks to
   Redis. In hydo's snapshot the worker is a few seconds old, so the fault meets that first
   `cleanup`.
2. `cleanup` runs in the `ensure` of `initial_wait`, outside the loop's `rescue`, so a Redis
   error ends the thread. Nothing starts it again.
3. Every retry and every delayed job of that worker then waits in Redis for good. The worker
   still runs new jobs, so nothing looks broken.

**Decision: a bug in Sidekiq, not in Lago.** `lib/sidekiq/scheduled.rb` has the same code in
Sidekiq 7.3.10, which Lago v1.57.0 uses, and on Sidekiq's main branch. In production it needs a
worker that starts while Redis is unreachable, for example after a Redis failover. No issue was
drafted: it belongs to Sidekiq, and it was not checked outside hydo.

### 4. A billing day that the clock skips is never invoiced (known upstream: Lago issue 354)

Found by `eventually_1_billing_tick` in the hand schedule `disk=none | run T=0:86400`. The run
ends with `150 s after the tick: 3 subscriptions without a November invoice`. The fourth
subscription was terminated, so its November usage is on the termination invoice.

1. The clock steps from 2026-11-30 23:55 to 2026-12-01 23:55, so no hourly tick of December 1
   runs.
2. The biller (`Subscriptions::OrganizationBillingService`) selects a monthly calendar
   subscription only when today is the first of the month
   (`DATE_PART('day', :today) = 1`).
3. On December 2 and later, nothing selects November again, and it is never invoiced.

**Decision: known upstream.** Lago issue 354 (open since 2024) describes the same with a clock
process that breaks: "invoice A is not resent". A maintainer answered that a missed date needs a
billing process run by hand. In production the cause is not a clock step but a clock process,
a worker, or Redis that is down for the whole billing day.

### 5. A job pushed just after a backward clock step never runs (clock step and Sidekiq's fetch)

Found by `eventually_2_transfers_charged` in seed 3, shrunk to `disk=none
dseed=1717ad91294d20d2 | run T=4:-2700 W=:p2,r3000,...` (a clock step of 45 minutes back, no
other fault, the same fingerprint `c2d6eb3be2941fb3` twice). The event `e9` was acknowledged 3 s
after the step. The API's log has the enqueue of its `Events::PostProcessJob`, but the worker's
log never starts that job, and nothing is left in Redis. Jobs pushed 6 s and 12 s later ran.
At the end, Redis's client list shows that 4 of the worker's 10 fetch connections were opened
right after the step; the other 6 were opened at the start.

1. The clock steps back 45 minutes while every Sidekiq thread waits in `BRPOP` with a 2 s timeout.
2. The threads' clients give up by their own timers and open new connections.
3. The job pushed next is most likely handed to a call that its client had abandoned, so it is
   lost. This run did not show which connection received it.

**Decision: an environment fault, not a Lago bug.** The loss is in the interplay of a backward
clock step, Redis, and Sidekiq's open-source fetch. The consequence in Lago is the same as in
finding 2: an acknowledged transfer that is never charged.

### 6. After a backward clock step, a termination can drop usage that Lago already accepted (clock artifact)

Found by `final_business_result` alone in seeds 13, 15 and 19 (a step back of 30 s or 1 hour).
Seed 13 shrank to a step of 112 s: `disk=none dseed=17cff017249f463e | run T=0:-112 W=:p4,...`,
`1 totals lack acknowledged units` (`units:sub4:api_calls: reported 0, acknowledged 12`). In
seeds 4 and 7 a step back of 30 days ended `sub4` before November's last day, and
`eventually_1_billing_tick` failed too.

1. Lago stamps the seed events of `sub4` at 23:55:06 with its own clock.
2. The clock steps back 112 s, and the customer cancels: Lago records the termination at
   23:53:2x, before the events.
3. Lago bills only the events inside a subscription's dates, so the 12 acknowledged units are
   never billed.

**Decision: a clock artifact, not a bug.** Lago orders events, terminations, and subscription
starts by its wall clock, and no Lago promise covers a clock that runs backwards. A
step back of 30 days put events before the subscription's start (seed 18: `14 of 14
acknowledged transfers have no fee`). `eventually_2_transfers_charged` now names that case
apart: `... have no fee: Lago stamped them before their subscription started`. Short steps back
do happen when a machine's clock is corrected. A billing system that bills by its own receipt
time can drop acknowledged usage after one.

### 7. After a power cut with the clock 30 days behind, the API does not start (environment)

Seed 5, shrunk to `disk=cut dseed=978180e49ee2ae24 | run C=10 T=8:-2592000 W=:p1,...`, ends
`VIOLATION service cannot recover (committed data unrecoverable) (api)`. The diagnosis names the
API's setup in the recovery boot, which runs Lago's `migrate.sh`, as Lago's file does at every
start. Rails refuses it: `ActiveRecord::InvalidMigrationTimestampError: Invalid timestamp
20261102160958 for migration file: add_billable_metrics_cursor_index. Timestamp must be in form
YYYYMMDDHHMMSS, and less than 20261101235620`. The clock was on 2026-10-31, before the release's
own migrations were written. This is not a Lago bug.

### Mistakes in this workload and its checks

- **A November rule that counted the wrong invoices.** The first `final_november_invoiced`
  accepted any finalized invoice whose period covered November. The invoices of charges paid in
  advance have such periods too, even for an event of December 1. A hand run with
  `T=0:300` showed it: the generic rule reported 12 units missing per subscription while the
  November rule held, because the biller had not run yet. The rule now accepts only periodic
  and termination invoices.
- **A billing tick that ignored backward steps.** The first `eventually_1_billing_tick` moved the
  clock to the next minute 10. After a step back of an hour or more, that was still November 30,
  and November could not be billed (the first exploration's seed 4, stopped). The tick now moves
  the clock to December 1 00:09:58 at the earliest.
- **Seed operations outside the records.** The seed's events were first written to
  `$HYDO_OUTCOMES` during provisioning, which hydo does not carry into the run, and the generic
  rule reported them as counted twice. They are now in `/data/seed/` (see "What hydo lacked").

### Not found

No invoice whose total differs from its fees. No period invoiced twice, also when the hand
schedules started the biller in the middle of the drivers, kills and power cuts (below). No event
charged twice in advance, also with redeliveries of the same transaction id while the first
delivery was in flight. No invoice left in `generating` or `draft`. No acknowledged event missing
from PostgreSQL. A double count of usage would have failed the generic rule, and none occurred.

## The explorations

Every exploration used `--workers 1`, because another agent ran VMs on this Mac at the same
time. Every failure was run again and gave the same verdict; the first failure of each
signature was shrunk and its minimal schedule run twice with one fingerprint.

| exploration | image | schedules | wall | failures, by cause |
|---|---|---|---|---|
| A: `--strategy process --new-faults --seeds 1-20` | the first complete one (its tick and checks before the fixes in "Mistakes") | 20 (18.7 runs an hour, with 12 re-runs and 306 shrink runs) | 66 min | 12: power cut 4 (finding 2), clock step back before a termination 5 (finding 6), clock step back 1 (finding 5), clock before the subscription's start 1, clock behind the migrations 1 (finding 7) |
| B: `--strategy swarm --new-faults --seeds 21-40` | the final one | 20 (18.3 runs an hour) | 66 min | 8: Redis killed and paused during a cancellation 1 (finding 1), skipped billing day 1 (finding 4), clock step back before a termination 1, disk I/O errors that PostgreSQL cannot restart after 4 (`ERROR restart after the faults (db)`), a lying disk with a power cut 1 (the worker does not start in the recovery boot; not analysed) |
| C: 16 hand schedules | the final one | 16 | 25 min | 3: Redis killed or cut off while the drivers ran (finding 3); none in the 13 others |

The hand schedules of C aim at the billing boundary. `T=0:900` moves the clock from
2026-11-30 23:55 to 00:10 of December 1 before round 0, so Lago's clock starts the hourly biller
while the drivers send events. Each schedule adds one fault on the billing path: the worker killed
or paused, Redis killed or cut off, PostgreSQL killed, paused or cut off, the clock service
killed, or a power cut in rounds 2 and 4. `T=0:300` puts the run on December 1 before the biller,
so `sub4` is cancelled on its billing day. No schedule of C invoiced a period twice, left an
invoice in a state that is not final, or broke an invoice's total. Finding 4 came from one more
hand schedule, `T=0:86400`.

Two more variants ran the power-cut schedules of A: the final image and the build with Redis's
append-only file (finding 2).

## Rules, runs, findings, and human effort

Human effort is counted as the number of rules written, with their lines. The client
(`helper_client.rb`, 232 lines), the commands (115 lines) and `hydo.yaml`'s hand changes are
shared by both kinds. The runs are A, B and C above: 56 schedules, plus the directed runs.

| rule | kind | written | runs it failed in (A / B / C) | findings it reported |
|---|---|---|---|---|
| `final_acked_events_stored` | billing | 15 lines of shell | 0 / 0 / 0 | none |
| `eventually_2_transfers_charged` | billing | 29 lines of shell | 7 / 1 / 3 | 2, 3, 5, and the clock before a subscription's start |
| `final_one_fee_per_transfer.sql` | billing | 6 lines of SQL | 0 / 0 / 0 | none |
| `final_invoice_total_is_sum_of_fees.sql` | billing | 11 lines of SQL | 0 / 0 / 0 | none |
| `final_one_invoice_per_period.sql` | billing | 9 lines of SQL | 0 / 0 / 0 | none |
| `eventually_1_billing_tick` and `final_november_invoiced.sql` | billing | 37 lines of shell, 8 of SQL | 2 / 3 / 0 | 1, 4, and 6 when the termination fell before November 30 |
| `final_invoices_final.sql` | billing | 4 lines of SQL | 0 / 0 / 0 | none |
| `final_business_result` | generic | 14 lines of shell, 45 of awk, and the 35-line `report` in the client | 11 / 3 / 3 | 1, 2, 3, 4, 5, 6, and the clock before a subscription's start |

So: the 7 billing rules (119 lines) reported findings 1 to 5 and the clock before a
subscription's start. They reported finding 6 only when the termination fell before November 30,
in 3 of its 6 runs. The one generic rule (94 lines) reported all of them, finding 6 in all 6
runs. Five of the seven billing rules never failed. Two rules did the work, and both read the money:
"every acknowledged transfer is charged" and "every subscription's period is invoiced". The
generic rule cost the least thought to write. The only domain knowledge in it is how to ask Lago
for a total. Its weakness is diagnosis: "12 acknowledged units missing from `units:sub4:api_calls`"
does not say whether the event, the fee, or the invoice is missing.

## What hydo lacked

In the order in which they cost time. Each was an open item in `AGENTS.md`; Milestone 225 added
the first six, and the next section shows the example without the workarounds they needed.

1. **`eventually_` commands ran after the `eventually_` checks.** The first design moved the
   clock to the billing tick in an `eventually_` command and polled the November invoices in an
   `eventually_` check. The check polled for its whole 180 s from the heal, and failed, before
   the command ran. The fix here was to wait inside the command itself, which made the command
   the rule. Now a check with `after=commands` counts from the end of the `eventually_` commands,
   which run first.
2. **A seed's records were not carried into the run.** The seed's events are acknowledged
   operations too, but `$HYDO_ACKED` and `$HYDO_OUTCOMES` of the `first_` commands were not the
   run's files. So the generic rule saw Lago's totals as counting the seed's units twice. The
   seed wrote its own records to `/data/seed/`, and the checks read both files. Now
   `test.first_records: true` keeps the seed's lines in the run's files.
3. **A check's output beyond its last line was lost.** The generic rule's useful output is the
   list of totals that differ. Only the last line reached the verdict and `result.json`. Each
   explanation needed a replay with `hydo inspect` until the check also wrote its lines to
   `/dev/console`. Now a failing check's last 20 lines are in `result.json`
   (`details.output`), in `hydo report`, and in `report.md`.
4. **One signature, several causes, one shrink.** The explorer shrank the first failure of each
   signature. "Transfers have no fee" had three causes (a power cut, a clock step back, a clock
   before the subscription's start); only the clock step was shrunk. The check gives the clock
   case its own last line. Now `hydo explore --shrink-causes N` shrinks up to N failures of a
   signature whose fault kinds differ. The two clock causes have the same fault kinds, so the
   check keeps its own last line for the second one.
5. **The importer's idle job service was a fault target.** `hydo compose` kept the one-shot
   `migrate` service as a service that sleeps. The explorer drew pauses, kills and link faults
   on it in 6 of the first exploration's 20 schedules, where they test nothing. Now it has
   `job: true`, and no schedule of the re-run names it.
6. **Clock steps could not aim at a time.** The explorer stepped the clock by fixed amounts (30 s,
   5 minutes, 1 hour, 1 day, 30 days, a year). Billing depends on times of day: Lago bills at
   minute 10 of the hour, on the first of the month. The schedules that start the biller in the
   middle of the faults were written by hand, with a step of 900 s. Now `T=<at>:@:10-2s` steps to
   2 s before the next tick, and `clock_points:` makes the explorer draw such steps.
7. **The explorer's identity test reads git-ignored files.** It lists every file under an
   example's `hydo/` folder, also `hydo-out/`, `.hydo/` and `images/`. With this example's local
   explorations in place, its golden could not be re-recorded until they were moved away. This
   is open item 93, left to another change.

## The re-run without the workarounds (Milestone 225)

The same image setup on the Mac engine, with hydo built in this tree after Milestone 225 and the
published 0.3.1 release files. The final image is `explore-ob-lago-arm64.cpio.gz`, sha256
`9621369557acf427…`; the data disk is unchanged (`2d0069a698397d61…`). What changed:

- `first_seed` records into `$HYDO_ACKED` and `$HYDO_OUTCOMES` (`test.first_records: true`).
  The data disk's `seed/` directory and the checks' second file are gone.
- `eventually_1_billing_tick` only moves the clock to the tick. The two waits are now checks
  that count from the end of that command (`after=commands`): `checks/eventually_november_billed`
  (150 s) and `checks/eventually_transfers_charged` (120 s; it was the command
  `eventually_2_transfers_charged`). Each check makes one attempt per poll instead of looping.
- `final_business_result` no longer copies its lines to `/dev/console`; on a failure it prints
  what Lago reported before the totals that differ.
- `migrate` has `job: true`, and `clock_points: ["@:10-2s", "@month+9m58s"]` lets the explorer
  step to 2 s before an hourly tick and to the month's first tick.

| run | schedule | result |
|---|---|---|
| no faults, twice | `disk=none \| run` | OK `833ba34749c6a408` both times (ctl `16f00393795a0108`); the seed's 8 events are in the run's acknowledged ids; the `commands` anchor came 0.05 s after the heal's, and the two checks held 5 s and 15 s after it |
| the saved regressions | `hydo repro --regressions` | 3 of 3 as expected: `power-cut-loses-queued-fees` `baf7c25493b0d851` (4 of 9 transfers without a fee), `skipped-billing-day` `3acb64e49408afc0`, `terminate-while-redis-down` `e9603e35d6daf000` |
| lago-api 6901, twice | `disk=none \| run K=redis@1+4` | VIOLATION `e9603e35d6daf000` both times: `1 subscriptions without a November invoice` |
| 6901 at the billing tick | `disk=none \| run T=0:@:10-2s K=redis@1+4` | VIOLATION `07cacde684ea81b4`, the same bug; the log says `CLOCK JUMP of 892s to @:10-2s (2026-12-01T00:09:58Z)` |
| Redis cut off at the tick | `disk=none \| run T=0:@:10-2s N=redis@1-4:loss,100%` | VIOLATION `381e3364588a72e9`: 1 of 11 transfers without a fee (finding 3's pattern) |
| the billing day skipped | `disk=none \| run T=0:@day+23h55m` | VIOLATION `3fbb11ed436b62ef`: 3 subscriptions without a November invoice (finding 4) |

The output of the failing checks now explains a run without a replay. For lago-api 6901,
`result.json` lists the generic rule's totals that differ:
`units:sub4:api_calls: reported 0, acknowledged 12`, and transfers of `sub1` and `sub3` that
were still missing when the 120 s ran out (`3 of 11 acknowledged transfers have no fee`). Until
now that check had 270 s, because it ran after the billing command's own wait; the Redis kill at
the worker's start also meets Sidekiq's scheduler (finding 3), which was not looked into further.

Three explorations without shrinking (`--no-shrink`, `--workers 1`; the fault space changed, so
the seeds draw other schedules than in A and B):

| exploration | schedules | wall | failures (all reproduced) | by cause |
|---|---|---|---|---|
| D: `--strategy swarm --new-faults --seeds 21-40` | 20 | 18 min | 6 | disk I/O errors that PostgreSQL or the API cannot restart after 4, a transfer without its fee after a Redis kill and a step back 1, the clock behind the migrations 1 (finding 7) |
| E: `--strategy swarm --new-faults --seeds 41-60` | 20 | 25 min | 10 | lying disks 5 (lost events, a service that cannot recover), disk I/O errors 4, a power cut that left a termination invoice in `generating` 1 |
| F: `--strategy process --new-faults --seeds 1-20` | 20 | 19 min | 9 | power cuts that lose fees 3 (finding 2), the clock behind the migrations 3 (finding 7), a skipped billing day 1 (finding 4), a step back before a termination 1 (finding 6), **a subscription invoiced twice for November** 1 (finding 8, below) |

No schedule of the 60 named `migrate` (6 of 20 did in A). Three drew a step to a clock point
(`T=9:@month+9m58s`, `T=8:@:10-2s`, `T=3:@:10-2s`), and the last of them found finding 8. None
drew the schedule of lago-api 6901: in B its seed 36 killed Redis, and with `migrate` out of the
list of services the same seed now kills PostgreSQL. The saved regression and the hand schedules
above find it on every run.

### 8. A subscription cancelled shortly before the billing tick is invoiced twice (a Lago bug, reproduced without hydo; no upstream issue found)

Found by `final_business_result` alone in exploration F's seed 7: `disk=none
dseed=cb7302cecc8929b7 | run P=api_clock@7+2 T=3:@:10-2s O=db-api@0-3:resp:loss,74%
W=740c6cfa,p1,q1,r1500`. On the image `9621369557acf427…` it gave `ead9b89d0f62183b` four times:
in the exploration, in its re-run, in a third run, and once more for this check. The rule's output
in `result.json` says `units:sub4:api_calls: reported 24, acknowledged 12`. A replay
(`hydo inspect exec ... --at end`) read Lago's rows for `sub4`:

| invoice | created | reason | charges | fees |
|---|---|---|---|---|
| 1, finalized | 2026-12-01 00:10:00 | `subscription_periodic` | 11-01 00:00 to 11-30 23:59:59 | the base fee, 12 units of `api_calls` |
| 2, finalized | 2026-12-01 00:10:02 | `subscription_terminating` | 11-01 00:00 to 11-30 23:56:28 | the base fee, 12 units of `api_calls` |

The subscription row has `terminated_at` 23:56:28.44 and `updated_at` 00:10:02.13. So the API
set `terminated_at` at 23:56:28, inside the termination's transaction, and the transaction wrote
the row and committed 13 minutes later on the guest's clock:

1. The database's answers to the API are lost in rounds 0 to 2. No attempt of the cancellation
   in these rounds gets an answer within the client's 10 s. One of those requests has locked
   the row (`with_lock`) and set `terminated_at`, and then waits for the database.
2. The fault ends, and the clock steps from 23:56 to 00:09:58 before round 3. The request is
   still waiting, so its transaction is still open.
3. At 00:10:00 the hourly biller reads `sub4` as active and invoices November. At 00:10:02 the
   termination commits, and its invoice bills November up to 23:56:28 again. Both invoices are
   finalized. A later attempt of the cancellation gets 404, which the client counts as done.

**The cause in Lago's code** (lago-api v1.57.0; `main` at `2e119262` has the same code):

1. `Subscriptions::TerminateService` sets `terminated_at` to the current time and the status
   `terminated` inside `subscription.with_lock`, and commits at the end of the block. Until the
   commit, every other session reads the subscription as active.
2. `Subscriptions::OrganizationBillingService` selects the subscriptions to bill by
   `subscriptions.status = active`, and `Invoices::SubscriptionService` checks `active?` again.
   Neither takes the row lock, so both read the state before the termination. They invoice the
   whole period.
3. After the commit, the termination's `BillSubscriptionJob` bills from the start of the period
   to `terminated_at`. The duplicate check (`CreateInvoiceSubscriptionService#duplicated_invoices?`)
   and the unique index `index_uniq_invoice_subscriptions_on_charges_from_to_datetime` cover only
   periodic invoices, so nothing compares the termination invoice with the periodic one.

**What the lost answers contribute.** They keep the termination's transaction open. A
termination that commits in milliseconds never meets the biller with this result: the biller
runs at minute 10 of the hour, and a monthly period ends at midnight in the customer's time zone.
A termination before midnight that commits before the tick is not selected. A termination after
midnight bills only the new period. So the transaction must stay open from before midnight until
after the tick, at least 10 minutes in a time zone with a whole-hour offset. The clock step in
hydo's schedule only makes those 10 minutes pass in a few seconds. In production the same needs a
network delay of 10 minutes or more between the API and PostgreSQL that does not break the
connection. Nothing in Lago's defaults ends such a request: Puma has no request timeout, and
`LAGO_DATABASE_IDLE_TX_TIMEOUT` (PostgreSQL's `idle_in_transaction_session_timeout`) is off
unless it is set. Linux keeps a TCP connection through about 15 minutes of retransmissions.

**Reproduced without hydo** (2026-11-14, Docker on this Mac, Lago v1.57.0 from this example's
`docker-compose.yaml`). The API reaches PostgreSQL through a small TCP proxy. When the proxy sees
the termination's `UPDATE "subscriptions" ... "terminated_at"`, it holds the database's answers
on that connection for 16 minutes. Customers in the time zone `Atlantic/Azores` (UTC-1 in
November), with monthly anniversary plans that renew on the 14th, so that their period ended at
01:00 UTC. The time zone was set in the database, because the API accepts it only with a
license.

| subscription | what happened | invoices |
|---|---|---|
| `held1`, `held2`, `held3` | cancelled at 00:56:01, 00:56:11 and 00:56:21 UTC (23:56 local), answers held until 01:12:01 | each two, finalized, 11.20 USD each: the periodic one at 01:10:00 for 10-14 to 11-14 00:59:59, and the termination one at 01:12:01 for 10-14 to 23:56 local |
| `ctrl1` | cancelled at 00:57:30 without a hold; the API's three threads were busy, so it ran at 01:12:01 | periodic 11.20 USD at 01:10:00, termination 0.33 USD for 01:00 to 01:12 UTC (correct) |
| `plain1` | not cancelled | periodic 11.20 USD (correct) |

With the answers held across the billing run, 3 of 3 subscriptions were invoiced twice.

**A fix, tested the same way.** In `Invoices::SubscriptionService`, a periodic invoice first
locks its subscriptions (`lock_subscriptions!`, which activation billing already uses), bills
only those still active, and keeps the lock until its invoice subscriptions exist. The worker ran
with that change, and the next hour repeated the test in `America/Argentina/Buenos_Aires`
(UTC-3; an earlier try with `America/Noronha` billed nothing, because Lago accepts only the
zones of Rails' list):

| subscription | what happened | invoices |
|---|---|---|
| `fx2held1`, `fx2held2` | cancelled at 02:56:01 and 02:56:11 UTC (23:56 local), answers held until 03:12:01 | each one, the termination invoice, 11.20 USD for 10-14 to 23:56 local. Their periodic jobs waited 121.6 s for the lock, then found the subscription terminated and made nothing |
| `fx2ctrl` | cancelled at 02:57:20 UTC (23:57 local) without a hold | one, the termination invoice, 11.20 USD (correct; the change does not touch this case, because the biller already reads the subscription as terminated) |
| `fx2plain` | not cancelled | periodic 11.20 USD (correct) |

Lago's own tests were not run with the change.

**The schedule as a regression.** Which schedules show the bug depends on where the lost answers
stop the request. When they stop it before the API sets `terminated_at`, the termination
happens after the tick and is billed correctly. Every hydo run of one schedule on one image gives
one result, but an image with other bytes moves the point where the request waits. After the
regression file was first added, the image changed (`c00b630a459c365c…`), and on it the original
schedule gave OK (`57345a0a048cbbfb`), as did the regression's first version. Of four
neighbouring schedules on that image, two showed the bug: `loss,60%` and the fault in rounds 1
and 2 only. The saved regression `hydo/regressions/cancel-before-billing-tick.schedule` is
`disk=none | run T=3:@:10-2s O=db-api@1-3:resp:loss,74% W=740c6cfa,p1,q1,r1500`. On the image
with it (`129f1938e3b5ffb9…`) it gave VIOLATION `67e39fdc213992b6` with `hydo run` and with
`hydo repro --regressions`, which ran 4 of 4 regressions as expected. A change to any file of the
example changes the image and can turn this regression into OK. Then search a schedule again
with the variants above.

**Decision: a Lago bug, not known upstream.** No issue or pull request in getlago/lago-api or
getlago/lago describes it (searched 2026-11-14 for duplicate, double and twice invoices,
termination, and periodic billing, open and closed). Pull request 4795 serializes two periodic
jobs of one day, and it leaves termination jobs out on purpose. v1.57.0 is the newest release.
The billing rule `final_one_invoice_per_period` did not see the bug, because it counts only
periodic invoices.
