# Saleor under hydo, the money of an e-commerce platform under faults

Saleor (github.com/saleor/saleor) is an e-commerce platform: a Django application with a GraphQL
API, Celery workers for background work, PostgreSQL, and Redis as the cache and Celery's broker.
This example puts Saleor's backend, as Saleor's own platform file runs it, under hydo, and hunts
for a business bug that nobody reported. It is also the first run of the rule suggestion
(`hydo rules scan`, `hydo rules check`, Milestone 236) on a project that hydo had never seen.

## The short answers

- **What was found?** Two Saleor bugs that no report names, and two known Saleor bugs. The blind
  exploration found both new ones, and Saleor's own images in Docker reproduce both. Nothing was
  posted.
  1. **New: a refund request is lost with its Celery task.** A refund that staff request is lost
     when the Celery worker that sends it to the payment app dies, or when the broker loses the
     task. `transactionRequestAction` acknowledged it, and Saleor never sends it again or marks
     it failed: the payment shows no refund and nothing pending. When the worker died after
     sending it, the customer was refunded and Saleor does not know it. The exploration found it
     in its first seed, and `disk=none | run H=payments.refund@1-12:delay=6000 K=worker@6+1`
     breaks the rule on every run, while each fault alone passes.
  2. **Known (saleor/saleor#19645): two refunds of one payment that overlap refund more than it
     charged.** With Saleor's own gift card gateway the card ends with more money than it was
     issued with.
  3. **Known (saleor/saleor#19919): a checkout completion that stops between its two database
     transactions spends a single-use voucher code without an order,** and the paid checkout's
     retry is refused. The open fix covers exceptions, not a process that ends.
  4. **New: a gift card is charged and the charge is not recorded.** When the process stops
     between the two commits of a gift card charge, the card pays, the order stays unpaid, and
     nothing can charge or release the payment. The exploration found it in its fifth seed.
- **What held?** In the clean runs and the six explored seeds no purchase was acknowledged without
  its order, no stock or voucher code went over its limit, every order total added up, and the
  payment app never charged a checkout twice, also across kills and pauses of every service,
  error answers of the payment app, and clock steps.
- **The rule suggestion on a project it had never seen.** The scan read none of Saleor's model
  files, so 2 of the 17 rules came from its lines, 14 from reading Saleor's code, and one is the
  generic rule. The new bugs were caught by rules that came from reading the code
  (`refund_requests_end`, `gift_card_balance_is_its_ledger`, `placed_orders_are_paid`) and by
  the generic rule.
- **What did hydo lack?** Section "What hydo lacked": ten gaps, none fixed here.

## The version

Saleor 3.23.49 (2026-11-12) was the newest release when the onboarding started; it ran from
Saleor's own image `ghcr.io/saleor/saleor:3.23.49`. PostgreSQL is 16.16
(`postgres:16.16-alpine3.24`) and Redis is 7.4.7 (`redis:7.4.7-alpine`). Saleor's platform file
names PostgreSQL 15 and Valkey 8.1; Saleor supports both versions used here. The CHANGELOG of
the unreleased 3.24.0 on `main` (2026-11-14) was read: it has no change to refunds, gift card
payments, vouchers at checkout completion, or Celery's handling of payment requests.

## The project

| file | what it is |
|---|---|
| `docker-compose.yaml` | the system as Saleor's platform file runs its backend: PostgreSQL, Redis, a one-shot migration, the API (uvicorn with two workers) and one Celery worker with its beat scheduler |
| `hydo/hydo.yaml` | the services as `hydo compose` imported them (`hydo/hydo.generated.yaml`), with the changes marked `hand:` |
| `hydo/workload/` | `first_setup` makes the shop; three `parallel_driver_` commands are the client's operations; `helper_client.py` is the client, run with the API image's Python; `helper_business_result` is the generic rule |
| `hydo/rules.yaml` | the proposed rules, with their evidence in Saleor's sources (section "The rules") |
| `hydo/checks/` | the 17 rules as checks, in the format that `hydo rules check --write-checks` writes |
| `hydo/regressions/` | the schedule that loses a refund request (finding 1) |
| `native/` | the stack and the probes without hydo (`native/README.md`) |

The shop has one channel in USD, one warehouse, a shipping method of 5.00, and three variants of
one product: 10.00 (20 in stock), 25.00 (12 in stock) and 40.00 (6 in stock). It has three
vouchers for the whole order: `SAVE5` (5.00 off, used at most 3 times), `ONE-A`, `ONE-B` and
`ONE-C` (8.00 off, each code used once), and `WELCOME` (10 % off, once per customer, ending at
00:40 of the guest's first day). It has four gift cards of 60.00.

Customers pay in the way Saleor 3.23 recommends, with the transactions API:

- **A payment app.** The client calls `transactionInitialize` with the app's identifier, and
  Saleor calls the app synchronously. In hydo the app is the fake `payments` of `hydo.yaml`: it
  answers every charge and refund with success, and a repeated idempotency key gets its first
  answer. Saleor's own Stripe integration is a separate app (a Next.js service), so the
  ready-made Stripe emulator does not apply: Saleor never calls Stripe itself.
- **A gift card.** Saleor's own gateway `saleor.io.gift-card-payment-gateway`, new in 3.23,
  authorizes the card at `transactionInitialize` and charges it when the order is confirmed.

Two things in `docker-compose.yaml` differ from the platform file. `HTTP_IP_FILTER_ENABLED` is
`False`, so that Saleor calls the payment app on a private address, as the platform file's
comment says for local apps. The migration is a one-shot service; the platform's README runs it
by hand. `hydo/hydo.yaml` marks each change of the import with `hand:`. The changes are the
payment app (the fake `payments`), the worker's readiness check, the beat scheduler's schedule
file (section "What hydo lacked", item 7), the guest's RAM and clock settings, the data disk's
size, one shared image tree, the staff user, the client's folder in the API's image, and a
clock point at the end of `WELCOME`.

## The workload

| command | operation | what the client records |
|---|---|---|
| `parallel_driver_checkout` | a new checkout with one or two lines, a shipping method, maybe a voucher code; a gift card pays half or all of it, or none; the payment app pays the rest; then `checkoutComplete` | `co,<checkout token>,<total>`: `ack` with the order's total, `failed` when Saleor refused, `unknown` without an answer |
| `parallel_driver_checkout_gift` | the same, and a gift card always pays half or all of it | the same |
| `parallel_driver_refund` | a refund of 5.00 or 10.00 of an order's app payment or gift card payment, with `transactionRequestAction`, as staff do from the dashboard | `rf,<order>,<app or gift>,<amount>,<n>`: `ack` or `unknown` |

The seed of each command picks its lines, its customer, its code and its gift card. The client
repeats a call whose answer it did not get, and it repeats a payment that failed without an
answer with the same idempotency key, as a storefront does. It never repeats a refund: Saleor
gives a refund request no key that the client could send again. Each order gets at most one
refund in a run. Two refunds of one payment that overlap break the refund rule by a known Saleor
bug (finding 2), and that failure would hide every other one.

A run has 16 rounds of 1.5 s with up to three commands each. `first_setup` makes the shop
inside the snapshot that every run starts from.

## The rule suggestion

Saleor is the first project with its schema in Django model classes that the rule suggestion
met. `hydo rules scan` on Saleor's repository at the tag 3.23.49 took 2.3 s and printed
4,453 lines of evidence from 3,276 files:

| kind | lines | where they came from |
|---|---|---|
| `schema` | 1,184 | 1,109 from Django migrations; 45 from the one model package `saleor/attribute/models/`; the rest from `saleor/core/db/` |
| `state` | 289 | 288 from migrations |
| `api` | 32 | `saleor/graphql/schema.graphql`: the error code `DUPLICATED_INPUT_ITEM` (20 lines) and the idempotency keys of transactions (12 lines) |
| `docs` | 84 | 55 from instruction files for coding agents (`AGENTS.md`, `CLAUDE.md`, `.claude/skills/`), 29 from the CHANGELOG, `CONTRIBUTING.md` and the decision records in `docs/adr/` |
| `tests` | 2,864 | the tests, at most 40 lines per file |

The scan missed the files that state most of Saleor's rules. Saleor keeps its models in 25 files
named `models.py` (`saleor/order/models.py`, `saleor/payment/models.py`,
`saleor/giftcard/models.py` and others). The scan reads a model file only in a folder named
`models`, so it read none of them. It read the schema's error codes and idempotency keys, but
not the field descriptions that state limits, such as "Limit number of times this voucher can
be used in total."

The rules came from three places:

- The scan's lines gave the evidence of 2 of the 17 proposed rules: the decision record
  `docs/adr/0004-race-safe-balance-adjustments.md` line 7 ("a card can never go negative") and
  the idempotency key of `transactionInitialize` in the schema.
- Reading the files that the scan missed gave 14: the models, the checkout completion
  (`saleor/checkout/complete_checkout.py`), the transaction mutations, the gift card gateway
  (`saleor/giftcard/gateway.py`), and the voucher functions.
- The generic rule over the client's acknowledged operations was added as the skill says, for
  every project.

Section "Without faults" gives what `hydo rules check` reported. In short, 16 of the 17 rules
held with cases in the one clean run that the host's memory let finish, the 17th held but gave
no result to the check, and no planted change ran.

## The rules

`hydo/rules.yaml` has 17 rules. Each SQL rule is a query over Saleor's tables that returns the
rows that break it; each command rule is a program. The last column is the rule's result in the
clean run `disk=none | run W=00000001` of `hydo rules check` on the image with the final
workload and checks, with the number of rows or cases it judged (section "Without faults" says
why only one run counted).

| id | the rule | evidence in Saleor | clean run |
|---|---|---|---|
| `gift_card_never_negative` | a gift card's balance never goes below zero | `docs/adr/0004-race-safe-balance-adjustments.md` line 7 (from the scan) | held, 4 |
| `gift_card_not_above_initial` | a card never holds more than it was issued with (the shop never tops one up) | `saleor/giftcard/gateway.py` line 408 | held, 4 |
| `gift_card_balance_is_its_ledger` | a card's balance is its issue less its charges plus its refunds | `gateway.py` lines 251 and 443 | held, 7 |
| `refund_within_charge` | no payment is refunded for more than it charged | `transaction_request_action.py` line 136 | held, 28 |
| `refund_requests_end` | every refund request ends as a success or a failure (within 400 s) | `transport.py` line 69 | held, 6 |
| `one_order_per_checkout` | a checkout becomes at most one order | `saleor/order/models.py` line 50 | held, 14 |
| `stock_allocated_is_sum` | a stock's allocated quantity is the sum of its allocations | `saleor/warehouse/models.py` line 557 | held, 3 |
| `allocations_within_stock` | a warehouse never allocates more than it has | `saleor/warehouse/management.py` line 198 | held, 19 |
| `voucher_within_usage_limit` | a code is used at most its voucher's usage limit | `schema.graphql` line 31833 | held, 5 |
| `voucher_usage_counts_orders` | a limited code's usage count is the number of its orders | `saleor/discount/utils/voucher.py` line 99 | held, 5 |
| `single_use_code_spent_by_an_order` | a single-use code is inactive exactly when one order used it | `voucher.py` line 95 | held, 3 |
| `once_per_customer` | a customer uses a once-per-customer code in at most one order | `schema.graphql` line 11410 | held, 1 |
| `voucher_used_in_its_dates` | no order uses a voucher after it ended | `saleor/discount/models.py` line 84 | held, 1 |
| `order_total_adds_up` | an order's total is its lines' total plus shipping | `saleor/order/base_calculations.py` line 57 | held, 14 |
| `placed_orders_are_paid` | every order from a checkout was charged its total | `schema.graphql` line 9387 (`allowUnpaidOrders`) | held, 14 |
| `acked_operations_kept` | the generic rule: acknowledged purchases are one order each with the client's total, refused ones none, refunds as the client asked (within 400 s) | the client's records | held, 42 cases of 14 orders |
| `provider_charged_once` | the payment provider charges each checkout at most once (`hydo-world check`) | `schema.graphql` line 39789 (from the scan) | held (13 effects), but `unevaluated` in the report: "What hydo lacked", item 9 |

The `.sql` rules and the generic rule are the files of `hydo/checks/`, written from the
candidates file in the format of `hydo rules check --write-checks`; the two rules with a time
limit are `eventually_` checks.

## Without faults

Every run used hvx on the bare-metal machine, with this branch's `hydo` programs in a copy of the
0.3.3 Linux release, at the fingerprint epoch 3. A run boots, runs Saleor's migrations (1,436
migration files; natively they take about 3 minutes) and `first_setup`, and then the workload;
on the busy machine one `hydo run` took 13 to 25 minutes.

| image | schedule | result |
|---|---|---|
| without checks (12 rounds of 2 commands) | `disk=none \| run`, twice | OK both times, `fp=c065c7e0cbdb717f`, 20 commands |
| with the 17 checks, 4,096 MiB | `disk=none \| run` | OK, `fp=a4390ecc5d2f5471`, 30 commands, every check held |
| with the 17 checks, 3,072 MiB (the final image) | `disk=none \| run`, twice | OK both times, `fp=d62fe7b18f4cbfd3`, 30 commands, every check held |

The final image was built before the second regression file was saved, and an image stages
the project's regressions, so the image of the committed folder differs from it in that file
only.

Of a run's 30 commands, 7 end `failed` without faults. A refund command fails when the chosen
means did not pay the order (a gift card refund of an order that no gift card paid), and a
purchase fails when Saleor refuses it, for example a gift card that does not cover the amount
or a spent single-use code. The commands' logs stay in the guest, so the split was not counted.
The client records a refused purchase as `failed`, and the generic rule checks that no order
exists for it.

`hydo rules check --mutate` ran twice. The first time, on an earlier image, the host's
out-of-memory killer ended all three clean runs while another session's exploration held 9 GB
of the machine. The second time, on the image with the final workload and checks and 4,096 MiB
guests, the first clean run (`W=00000001`) held 16 rules, each with cases; the out-of-memory
killer ended the other two runs, so the planted changes never ran. Every rule is a check of the
example, and every check held in the clean runs above.

## What was found

A finding is a Saleor bug only when its cause is in Saleor's code or a documented promise is
broken. A finding is new only when it was reproduced without hydo and the search of Saleor's
issues, pull requests and discussions found no report of it.

### 1. A refund request is lost with its Celery task (a Saleor bug, new as far as the search found)

`transactionRequestAction(actionType: REFUND)` answers at once, without errors, after it saves
a `REFUND_REQUEST` event and a webhook delivery in state `PENDING`. The request reaches the
payment app later: `trigger_transaction_request` queues the Celery task
`handle_transaction_request_task` (`saleor/webhook/transport/synchronous/transport.py`, lines
616 and 75), which sends it and saves the app's answer. Charge and cancel requests take the same
way. Saleor's Celery settings keep
Celery's default, which acknowledges a task to the broker before it runs. So a task is gone when
its worker dies while it runs, and when the broker loses it before a worker takes it. Nothing in
Saleor sends the delivery again or marks it failed: `CELERY_BEAT_SCHEDULE` has no task for
pending deliveries. The staff member saw the refund acknowledged, and then:

- If the worker died after it sent the request, the payment provider refunded the customer, but
  Saleor never learns of it. The payment shows its whole amount charged, nothing refunded and
  nothing pending, so a second request refunds the customer twice.
- If the task was lost before it ran, the customer is never refunded, and Saleor shows no
  pending refund either.

Without hydo (`native/`, Saleor's images in Docker, the payment app answering after 6 s):

```text
probe-refund-worker-kill.sh 9 5.00          # the worker is killed 3 s after the request
payment|40.000|0.000|0.000                  # charged, refunded, refund pending: before
rk refunded 0.0 charged 40.0                # the mutation's answer: no errors
payment|40.000|0.000|0.000                  # 6 minutes later
event|refund_request|5.000||2026-11-14 20:57:37
delivery|pending|transaction_refund_requested
PAYAPP refund key=8dbc3fba-... amount=5.0 replay=False      # the provider did refund 5.00

probe-refund-broker-loss.sh 1 5.00          # Redis is killed while the task waits in its queue
payment|27.500|0.000|0.000                  # 6 minutes later
event|refund_request|5.000||2026-11-14 21:09:53
delivery|pending|transaction_refund_requested
                                            # the provider was never asked
```

After both probes Redis held no unacknowledged message (`HLEN unacked` 0) and no queued task: the
requests were gone.

The rule that catches it is `refund_requests_end`: every refund request has a success or a
failure within 400 s after the faults stop, which leaves time for Saleor's 5 retries. In hydo
the fault is a kill of the worker while the payment app is slow to answer, so that a request is
in flight:

| schedule on hvx | image | result |
|---|---|---|
| `disk=none \| run H=payments.refund@1-12:delay=6000 K=worker@6+1` | 4,096 MiB | VIOLATION `[refund_requests_end]`: `id=7 app_identifier=hydo.payments requests=1 ends=0` |
| the same | final, 3,072 MiB | VIOLATION `[refund_requests_end]`, `fp=03fafdf938b9f887` |
| the same, again | final | VIOLATION `[refund_requests_end]`, `fp=03fafdf938b9f887` |
| `disk=none \| run H=payments.refund@1-12:delay=6000` (no kill) | final | OK, `fp=c4bf48b27564e826` |
| `disk=none \| run K=worker@6+1` (no slow answers) | 4,096 MiB | OK, `fp=c51165d23bd87ba8` |

Each fault alone passes: the kill needs a refund in flight, and slow answers lose nothing. The
schedule is the saved regression `hydo/regressions/refund-request-lost-with-worker.schedule`.

The blind exploration found the same loss in its first seed, with other faults:
`disk=cut dseed=4127cc1347fae6ba | run K=worker@1+2 N=db-worker@13-15:loss,72% C=16 P=redis@6+1
T=8:@:40-1s W=3e71cf9e,p1,q3,r3000`. Both rules broke: `refund_requests_end` (`requests=1
ends=0`) and the generic rule ("order 10: the client saw refunds of 5 from its app payment
acknowledged, Saleor refunded 0"). The explorer's shrinker ran 38 candidates and kept two faults:
packet loss between the worker and PostgreSQL in rounds 13 to 15, and a power cut before round
16 (`N=db-worker@13-15:loss,18% C=16`, with two gift card purchases and three refunds). Each of
the two alone passed. Most likely the loss keeps a refund task running until the cut, and the
cut ends the worker like a kill.

The generic rule held in the directed runs. It compares acknowledged refunds with Saleor's refunds only
where they add up to no more than the payment charged, and which client refund was lost was not
looked up, so the dedicated rule is the one that caught it.

Is it a bug? Saleor acknowledges the request and then keeps no way to finish it. A worker that
the kernel kills for lack of memory, a node that fails, or a Redis that restarts without its
data is an ordinary event in a Celery deployment. The documentation of transactions says that
the app answers the request or reports the result later; it says nothing about a request that
never reaches the app. Celery's `acks_late` and a periodic task that sends pending deliveries
again are the usual ways to keep such a task, and Saleor has neither for these requests.

### 2. Two refunds of one payment refund more than it charged (a Saleor bug, known: saleor/saleor#19645)

`transactionRequestAction` limits a refund to the payment's `charged_value`
(`transaction_request_action.py`, line 136), but it reads that value without a lock, and a
requested refund lowers it only when the app's answer arrives. So two refunds that overlap both
pass the limit. With Saleor's own gift card gateway Saleor is the payment provider, so nothing
outside Saleor stops the second refund. Without hydo:

```text
probe-refund.sh 5 0.5 app              # two refunds of 40.00 of a 40.00 payment, 0.5 s apart,
                                       # the payment app answering after 2 s
5|-40.000|80.000                       # charged -40.00, refunded 80.00; the app refunded 80.00
race_refund.py 2 gift 27.5 2           # two refunds of a 27.50 gift card payment at once
after: charged -27.5 refunded 55.0
B1F4-65C2-71A1|60.000|87.500           # the card was issued with 60.00 and now holds 87.50
```

Issue #19645 (open since 2026-08-10) names these lines: the refund check "should use remaining
refundable amount, including existing `refunded_value` and `refund_pending_value`, under a
transaction row lock", and refund events must not "reduce `charged_value` below zero". It says
that payment providers reject such refunds; it does not name the gift card gateway, where Saleor
itself is the provider.

The rules `refund_within_charge` and `gift_card_not_above_initial` state it. The first
workload let two refund commands pick the same order; the final workload refunds each order at
most once, so that this known failure does not hide the others, and the hydo runs of the final
image do not reach it.

### 3. A single-use code is spent by a completion that made no order (a Saleor bug, known: saleor/saleor#19919)

`create_order_from_checkout` (`saleor/checkout/complete_checkout.py`, line 1579) takes the
voucher's use in one database transaction and creates the order in a second one. When the API
process ends between the two, the first stays committed. A single-use code is then inactive,
the checkout is marked as having used it, and no order exists. The storefront's retry gets
`VOUCHER_NOT_APPLICABLE`, because the inactive code no longer counts as the checkout's voucher,
and the customer's paid checkout cannot be completed with the code. Without hydo:

```text
probe-voucher-crash.sh                  # a paid checkout with ONE-C; the API is killed while the
                                        # order's transaction waits for a lock on the stock
code|ONE-C|f|0                          # the code: inactive
checkout|98cc1e04-...|ONE-C|t|77.000|full   # the voucher's use counted, 77.00 charged, no order
complete refused complete: voucherCode:VOUCHER_NOT_APPLICABLE    # the retry
```

Issue #19919 (open since 2026-10-16) reports the same two transactions and the same effects for
exceptions in the second transaction, and its fix, pull request #19955, releases the use in a
third transaction after the second one rolls back. A process that ends never runs that third
transaction, so the fix leaves this case open. The cause is the one #19919 names, so this
finding counts as known.

The rule `single_use_code_spent_by_an_order` states it. In hydo a kill of the API must come
while the order's transaction runs for a checkout with a single-use code; the exploration did
not reach it.

### 4. A gift card is charged and the charge is not recorded (a Saleor bug, new as far as the search found)

Saleor's gift card gateway authorizes a card at `transactionInitialize` and charges it when the
order is confirmed, in `charge_gift_card_transactions` (`saleor/giftcard/gateway.py`, line 273).
The charge has three steps, each its own database commit:

1. it saves a `CHARGE_REQUEST` event on the gift card payment;
2. in one transaction it locks the card and lowers its balance (from line 324);
3. after that transaction it saves the `CHARGE_SUCCESS` event and the payment's charged amount
   (`create_transaction_event_from_request_and_webhook_response`, line 365).

When the process ends between steps 2 and 3, the card has paid and the order does not know it.
The order stays `charge_status none`, the gift card payment keeps its authorization and a
charge request without a result, and nothing charges it again: the same function skips every
payment that has a `CHARGE_REQUEST` event, so that a card is never charged twice. Staff cannot
finish it either: `transactionRequestAction(actionType: CHARGE)` on the payment answers
`MISSING_TRANSACTION_ACTION_REQUEST_WEBHOOK`, because no app owns the gift card gateway. The
customer's card is short of the amount, and the shop holds an unpaid order. The same split
is in `refund_gift_card_transaction` (line 408): a death between its balance commit and its
`REFUND_SUCCESS` event credits the card without recording the refund (not reproduced).

Without hydo (`native/probe-giftcard-charge-crash.sh`, two row locks hold the charge at the
point after step 2, then the API container is killed):

```text
gc gift card C883 45.0 AUTHORIZATION_SUCCESS     # the card pays the whole checkout of 45.00
card|FBB8-7912-C883|60.000|60.000               # issued with 60.00; before checkoutComplete
--- after the API restarted
card|FBB8-7912-C883|60.000|15.000               # the card paid 45.00
order|1|45.000|0.000|none|unfulfilled           # the order: total 45.00, charged 0.00
event|authorization_success|45.000
event|charge_request|45.000                     # and nothing after it
--- the storefront sends checkoutComplete again
gc order 1 45.0                                 # Saleor answers with the order; nothing changes
# staff: transactionRequestAction(CHARGE) -> MISSING_TRANSACTION_ACTION_REQUEST_WEBHOOK
```

The blind exploration found it in its fifth seed, `disk=cut dseed=978180e49ee2ae24 cf=270 | run
P=redis@4+1 T=8:31536000 W=3cc7df19,p1,q2,r1500`: a power cut at the 270th disk flush after a
forward clock step of a year. Two rules broke:

```text
VIOLATION [gift_card_balance_is_its_ledger]
  db: code=4667-8D8D-81B9 initial_balance_amount=60.000 current_balance_amount=0.000 charged=45.000 refunded=0
VIOLATION [placed_orders_are_paid]
  db: number=8 total_gross_amount=30.000 charged=15.000
```

The card lost 15.00 more than its recorded charges, and order 8 is short by the same 15.00. The
shrinker kept the cut and the clock step: the cut alone passed twice and the step alone twice,
and a step of half a year failed too. The step most likely moves the 270th flush into the
charge's window; the native probe needs no clock step. The schedule
`disk=cut dseed=978180e49ee2ae24 cf=270 | run T=8:15768000 W=3cc7df19,p1,q2,r1500` is the saved
regression `hydo/regressions/gift-card-charged-without-record.schedule`. From fresh boots of the
final image it broke the rule twice with the same rows and `fp=bd67c9fd8169cb50` both times.

### 5. Authorizing a gift card on a second checkout while the first one completes ends in a deadlock (not a money bug)

A gift card that pays checkout A in full is authorized on checkout B while `checkoutComplete` of
A runs (`native/probe-giftcard-detach.sh`). B's authorization cancels A's, as Saleor documents
("Previous authorizations will be cancelled"). PostgreSQL then finds a deadlock between A's
order transaction and B's update of A's payment, and ends A's transaction: A's
`checkoutComplete` answers `Internal Server Error`, and its retry `CHECKOUT_NOT_FULLY_PAID`. No
money moves wrongly. The search found no report of the deadlock.


## The duplicate searches

Each search ran on 2026-11-14 over saleor/saleor's issues and pull requests, open and closed
(`search/issues`), and its discussions (GitHub's discussion search), with the phrases below.
The CHANGELOG of the unreleased 3.24.0 on `main` was read as well.

| finding | phrases | what they found |
|---|---|---|
| 1, the lost refund request | `refund never sent`, `refund request lost`, `refund stuck pending`, `refund pending forever`, `REFUND_REQUEST pending`, `refund not sent to app`, `refund request not processed`, `refund webhook not delivered`, `transaction action webhook lost`, `transaction request lost`, `transactionRequestAction celery`, `TRANSACTION_REFUND_REQUESTED not sent`, `handle_transaction_request_task`, `refund_pending_value`, `celery worker killed refund`, `celery task lost`, `worker restart lost task`, `acks_late`, `task_reject_on_worker_lost`, `delivery pending forever`, `EventDelivery pending`, `outbox webhook`, `broker restart webhook lost`, `redis restart webhooks lost` | no report of a lost payment action request. The nearest is issue #19835 (open): a broker that refuses one publish during `checkoutComplete` loses the order's later webhooks and its confirmation, and "nothing publishes it again"; its notes add that a worker death after the order commits leaves the order unconfirmed. It is about the work after an order is placed, not about refund requests, and its cause is a publish that raises inside an on-commit callback. Pull request #19811 (closed) added the idempotency key that the retries of a request carry; it does not cover a request that is never retried |
| 2, the double refund | `concurrent refund`, `refund race`, `refund exceeds`, `refunded more than`, `double refund`, `over refund`, `refund twice`, `refund charged amount`, `negative charged`, `gift card refund`, `refund_gift_card_transaction`, `transactionRequestAction refund` | issue #19645 (open) |
| 3, the spent single-use code | `single use voucher`, `single-use voucher`, `singleUse voucher`, `voucher usage not released`, `voucher usage released`, `release voucher usage`, `is_voucher_usage_increased`, `VOUCHER_NOT_APPLICABLE checkoutComplete`, `voucher deactivated`, `voucher code inactive`, `voucher used without order`, `voucher usage leak`, `checkout complete crash voucher`, `two transactions checkout complete` | issue #19919 (open) and its fix #19955 (open); #19928 (closed, an earlier fix of the same issue) |
| 4, the gift card charged without a record | `charge_gift_card_transactions`, `gift card balance deducted`, `gift card charged twice`, `gift card charge not recorded`, `gift card charge failure order`, `gift card transaction charge`, `gift card balance mismatch`, `gift card payment gateway charge`, `gift card not charged`, `gift card charged order unpaid`, `gift card crash`, `gift card balance order unpaid`, `MISSING_TRANSACTION_ACTION_REQUEST_WEBHOOK gift card`, `gift card payment gateway order confirmed`, `gift card charge request`, `gift card authorization order`, `gift card balance lost`, `gift card refund twice`, `giftcard gateway atomic`, `charge gift card on order confirm` | no report. The pull requests that built the gateway (#18436, #18550, closed) say only "More details in the internal issue", and #19442 (closed) does not mention the charge's commits. Issue #19835 (open) says that a death after an order commits leaves it unconfirmed, which is before the charge starts: the card then keeps its money. Here the card has paid |
| 5, the deadlock | `deadlock checkoutComplete`, `deadlock transactionInitialize`, `deadlock gift card`, `detach_gift_card_from_previous_checkout_transactions`, `gift card authorized another checkout` | no report; older deadlock fixes are about the Adyen and Stripe plugins |

Saleor's issues of the last weeks report many faults of the same family: the open pull request
#19996 fixes stock and checkout races (oversold stock, a voucher's use counted twice), and
issues #19852 and #19854 report lost or overwritten charge totals of transaction reports. None
of them is about a lost refund request.

## The code of the two new candidates on Saleor's main

The runs used the tag 3.23.49. On 2026-11-15 Saleor's `main` was at
`5e1679b1e91f76ac9fd553a62adeffb6e9acf5c3` (2026-11-13). The files of both code paths were
compared with the tag: `saleor/giftcard/gateway.py`, `saleor/payment/utils.py`,
`saleor/payment/gateway.py`, `saleor/order/actions.py`, `saleor/celeryconf.py` and
`saleor/graphql/payment/mutations/transaction/transaction_request_action.py` are the same on
`main`. `saleor/webhook/transport/synchronous/transport.py` differs only by a removed function
(`trigger_webhook_sync_if_not_cached`) and an unused import, which moves its line numbers;
`saleor/settings.py` differs in other settings. Both code paths are unchanged on `main`.

**Finding 1, the lost refund request.**

- `transaction_request_action.py` line 136: the mutation limits the refund to `charged_value`,
  saves the `REFUND_REQUEST` event (`create_transaction_event_requested`) and calls
  `request_refund_action` (`saleor/payment/gateway.py` line 130).
- `transport.py`, `trigger_transaction_request` (line 555 at the tag, 449 on `main`): it saves
  the delivery in state `PENDING` and queues `handle_transaction_request_task.delay` (line 617
  at the tag, 511 on `main`).
- `transport.py`, the task (decorator from line 69 at the tag, 67 on `main`):
  `@app.task(bind=True, retry_backoff=10, retry_kwargs={"max_retries": 5})`. It retries only an
  answer of status 500 or more; it sets no `acks_late`.
- The Celery settings: `saleor/celeryconf.py` line 34,
  `app.config_from_object("django.conf:settings", namespace="CELERY")`, reads the `CELERY_`
  settings of `saleor/settings.py`, lines 631 to 648 at the tag and on `main`. They set
  `CELERY_ACCEPT_CONTENT`, `CELERY_BROKER_URL`, `CELERY_RESULT_BACKEND`,
  `CELERY_RESULT_SERIALIZER`, `CELERY_TASK_ALWAYS_EAGER`, `CELERY_TASK_SERIALIZER`,
  `CELERY_TIMEZONE` and `CELERY_WORKER_PREFETCH_MULTIPLIER`. No `CELERY_TASK_ACKS_LATE`, no
  `task_acks_late`, and no `acks_late=` or `reject_on_worker_lost` appear anywhere in `saleor/`
  outside the tests, so Celery's default holds: a worker acknowledges a task when it takes it.
- `CELERY_BEAT_SCHEDULE` (`settings.py`, from line 673 at the tag and on `main`) has no task that sends a
  pending delivery again.

**Finding 4, the gift card charged without a record.** All lines are in
`saleor/giftcard/gateway.py`, the same at the tag and on `main`, unless named otherwise:

- `charge_gift_card_transactions`, line 273, runs from `order_confirmed`
  (`saleor/order/actions.py` line 380), which `order_created` calls in an on-commit callback of
  the order's transaction.
- Line 301: `create_transaction_event_requested` saves the `CHARGE_REQUEST` event (its own
  commit).
- The first transaction: `with transaction.atomic():` at line 324 to line 359. It locks the
  card (`select_for_update`, line 327), lowers its balance in `charge_gift_card` (line 341;
  the update is in `charge_gift_card`, lines 251 to 270), and saves the card's
  `used_in_order` event. It commits at line 359.
- The second step: `create_transaction_event_from_request_and_webhook_response` at line 365,
  outside any transaction (`saleor/payment/utils.py` line 1564). It saves the request event's
  PSP reference (line 1589), creates the `CHARGE_SUCCESS` event (line 1592), and saves the
  payment's recalculated amounts (line 1616), each its own commit.
- The guard against a second charge: lines 288 to 298 skip every gift card payment that has a
  `CHARGE_REQUEST` event.
- The same split is in `refund_gift_card_transaction`: the card's credit in
  `with transaction.atomic():` from line 436, the `REFUND_SUCCESS` event at line 468.

## The explorations

One exploration ran on the final image: `hydo explore --seeds 1-24 --strategy process
--more-faults --new-faults --protocol-faults --workers 1`. The faults were kills, pauses, packet
loss and one-way faults of the four services, power cuts (also at the k-th disk flush), clock
steps (also to 1 s before `WELCOME` ends), and protocol faults on the payment app. It was
stopped after six seeds, about three hours in, because each shrink candidate took four minutes
and the shared machine was short of memory:

| seed | faults | result |
|---|---|---|
| 1 | `K=worker@1+2 N=db-worker@13-15:loss,72% C=16 P=redis@6+1 T=8:@:40-1s` | VIOLATION `[acked_operations_kept]` and `[refund_requests_end]`: finding 1; 38 shrink candidates, minimal `N=db-worker@13-15:loss,18% C=16` |
| 2 | `K=redis@1+3 P=db@9+4 H=payments.refund@14-15:delay=10000,p=25` | OK |
| 3 | `K=worker@13+2 N=db@1-4:loss,61% P=worker@5+2 T=10:30` | OK |
| 4 | `K=db@12+4 P=api@3+1 T=8:300 H=payments.initialize@10-11:status=502` | OK |
| 5 | `cf=270`, `P=redis@4+1 T=8:31536000` | VIOLATION `[gift_card_balance_is_its_ledger]` and `[placed_orders_are_paid]`: finding 4; shrunk at the level of faults to `cf=270 T=8:15768000` |
| 6 | `K=worker@14+2 C=16 P=worker@3+2 T=7:300` | OK |

In the OK runs every rule held: no purchase acknowledged without its order, no stock or voucher
over its limit, no order total off, and no payment charged twice at the provider, across kills
of each service, a pause of PostgreSQL in the middle of the run, error answers of the payment app
(`status=502`), and clock steps. The exploration did not reach finding 3 (a kill of the API
during the order's transaction of a checkout with a single-use code).

## What hydo lacked

None of these is fixed here; open item 122 in AGENTS.md lists them.

1. **`hydo rules scan` does not read Django's model files.** A Django project keeps its models in
   files named `models.py`. The scan takes a code file as schema only in a folder named
   `models`, `schema`, `db`, `entities` or a migrations folder, so it read none of Saleor's 25
   `models.py` files and 1,109 lines of old migrations instead. The same holds for SQLAlchemy and
   other projects with one models file per module.
2. **The scan reads instructions for coding agents as documentation.** 55 of its 84 `docs` lines
   came from `AGENTS.md`, `CLAUDE.md` and `.claude/skills/`: rules for contributors, such as
   "Never use `instance.count += 1`", not promises of the product.
3. **The scan reads a GraphQL schema's names, not its descriptions.** Saleor's schema states its
   limits in field descriptions ("Limit number of times this voucher can be used in total."),
   and the scan printed error-code names instead.
4. **`hydo compose` gives a worker the image's port.** Saleor's Celery worker runs the API's image
   with another command. The import took port 8000 from the image's `EXPOSE` and wrote
   `ready: "nc -z -w 2 10.89.0.15 8000"`, which never passes; the one-shot migration got the port
   too. The fix by hand is a readiness check with `celery inspect ping`.
5. **The import's guest RAM was too small.** It guessed 1,536 MiB for uvicorn's two workers,
   Celery's worker with its two pool processes and beat, and PostgreSQL. The runs used 4,096 MiB
   and then 3,072 MiB, which is enough.
6. **A failing `first_` command's own output is not in the diagnosis.** The first clean run ended
   after 738 s of guest work with `HYDO-ORACLE: ERROR provision failed` and `first_setup` exit
   status 2. `hydo run` then replayed the run to collect the services' logs for the diagnosis,
   which took 26 more minutes on the busy machine. The API's log named the cause, Django's
   `DisallowedHost` for the client's IP address. The command's own output was not among the
   evidence. `first_setup` now copies its output to the console on a failure.
7. **A program that writes into the image's own files fails quietly.** An image service's root is
   read-only in the guest, and writable in Docker. Celery's beat scheduler keeps its schedule in
   its working directory, `/app`, so in the guest it stopped at start (`Read-only file system:
   'celerybeat-schedule'`), while the worker ran on and the service counted as ready. No
   periodic task ran: no release of funds of abandoned checkouts, no expiry of orders, no
   automatic completion. Neither the import nor `hydo build --check` warns of it, and the
   worker's log showed it only by chance. The fix by hand is `--schedule=/run/celerybeat-schedule`.
8. **An engine killed for lack of memory is reported as "the engine exited".** The first
   `hydo rules check` lost all three runs: the host's out-of-memory killer ended `hydo-hvx`
   each time (another session's exploration held 9 GB), and the check reported 17 rules
   `unevaluated` with `the controlled run broke: unexpected engine output: "the engine
   exited"`. Neither the message nor `rules-check.json` says that the engine was killed, or
   by what.
9. **A rule that is a `hydo-world check` line gives no result in `hydo rules check`.** The rule
   `provider_charged_once` (`run: hydo-world check at-most-once payments.initialize --by ref`)
   ran and held in the clean run ("payments.initialize: 13 effects, every ref at most once"), but
   its record carries only that line, not the wrapper's `hydo-rule exit=... cases=...` line, so
   the check reported it `unevaluated`. The cause was not found. As a check file in
   `hydo/checks/` it works.
10. **A rebuild costs most of an hour on a busy machine.** Every change of a workload file or a
   check means a new image. On the bare-metal machine during the re-recording pass, writing the
   owners and modes of the image's 64,535 files took about 25 minutes of each build.
