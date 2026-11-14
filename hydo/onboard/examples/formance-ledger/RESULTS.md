# Business rules for a ledger under hydo

This onboarding is an experiment in finding business bugs with hydo. A business bug breaks a rule
of the system's domain, for example a payment recorded twice. An infrastructure bug loses data or
availability. The system is Formance Ledger v2.4.18, a double-entry ledger with an HTTP API,
written in Go, on PostgreSQL 16. It ran unmodified, built from its tag. The experiment gave the
system two kinds of rules and recorded which kind found what:

1. The domain's own rules: what a ledger promises about money. Every transaction balances, a
   balance is the sum of its postings, no account overdraws, one request makes one transaction,
   and a revert undoes its transaction. The ledger's state alone decides each rule.
2. One rule: faults must not change the business result. The client records each operation it
   saw acknowledged. A final check computes the expected ledger from that record alone and
   compares it with what the API reports and what PostgreSQL holds.

Every run used the Mac engine (Apple Silicon, the release `hydo-mac-0.3.1`). The bare-metal
machine was busy with another agent's timing measurement, and the build machine was off.

## The short answers

- **What was found?** Four things, one of them a ledger bug:
  1. **A ledger bug.** Two transactions that first use one account at the same moment, in two
     different assets, race when they create the account, and one fails with `500 INTERNAL`.
     v2.3.24 does not do this. It shows without any fault, and natively with two `curl`
     commands. A one-clause patch fixes it: 0 of 200 pairs fail with it, against 196 without.
     Reported upstream as https://github.com/formancehq/ledger/issues/2542 (2026-11-13).
  2. **A client mistake.** A client that retries without idempotency keys records a payment two
     or three times when an answer is lost after the commit.
  3. **A documented behaviour.** A backdated transfer can leave an account below zero at a past
     date, because the ledger checks only the final balance.
  4. **An expected loss.** A disk that drops acknowledged writes, or `synchronous_commit=off`,
     loses acknowledged payments at a power cut.
- **Which rule found what?** No business rule found the ledger bug: the error logs did
  (`errors: true`, no rule written). The client retried the `500` with its key, so every rule
  held. The client mistake and the lost payments failed only the rules that read the client's
  records: rule 1.5 and the kind-2 rule. A copy is a valid, balanced transaction, and a lost
  payment is a whole transaction gone, so every rule over the ledger's state held. Those rules
  found nothing in 177 runs with faults.
- **Did faults change the business result with idempotency keys on?** Only with the disk faults
  of finding 4. The runs also had kills, pauses, network faults between the ledger and
  PostgreSQL, power cuts on an honest disk, clock steps, and up to six concurrent clients. Under
  those, every acknowledged operation stayed, and every operation without an answer took effect
  at most once.
- **What did hydo lack?** Four things, in "What hydo lacked":
  - a recovery boot that hangs when the records carried across a power cut pass 4 KiB;
  - a limit of 1,024 control messages per run;
  - error kinds split by a request id;
  - the login name written into `known-errors.yaml`.

## The project

| file | what it is |
|---|---|
| `build.sh` | builds `bin/ledger-<arch>` from the tag v2.4.18 (`83cd8dc7`) and `bin/ledgerc-<arch>` from `client/`, static Linux programs for amd64 and arm64, and checks their sha256 against its pins |
| `fix-new-account-race.patch` | the tested fix of finding 1, against v2.4.18 (below) |
| `client/main.go` | `ledgerc`, the test client: every workload operation and the four checks that need Go (standard library only) |
| `hydo/hydo.yaml` | PostgreSQL 16 as the ledger's own `docker-compose.yml` runs it, and `ledger serve` with automatic schema upgrades, each in its own network namespace. Also the knob `S` (`synchronous_commit`) and 30 rounds of up to 6 concurrent commands |
| `hydo/workload/` | `first_init` creates the ledger `main`; nine `parallel_driver_` commands, one per operation (below); `helper_sql.conf` from `hydo sql-gen --workloads none` |
| `hydo/checks/` | the rules (below): 9 SQL files over PostgreSQL and 4 programs |
| `hydo/known-errors.yaml` | 31 reviewed error kinds: the first start's migration probes, connection errors while a fault is on, a deadlock the ledger retries, and the two lines of finding 1 |
| `hydo/fetch-images.sh` | PostgreSQL 16.16 by digest for both architectures |

```sh
sh build.sh                    # needs Go 1.27.3 and network once; prints "ok" for the four pins
cd hydo && sh fetch-images.sh
hydo build hydo.yaml --set arch=arm64           # on a Mac; on Linux: hydo build hydo.yaml
hydo run "disk=none | run"
hydo explore --seeds 1-40 --strategy process --new-faults --errors-shrink 0
```

The client's operations, each with an idempotency key (`Idempotency-Key`, or the bulk element's
`ik`) that a retry reuses:

| command | operation | postings |
|---|---|---|
| `deposit` | money from outside | `world` to a user |
| `transfer` | a payment with a fee | a user to a user, and the same user to `fees` |
| `exchange` | two assets in one transaction | a user's USD to `world`, `world`'s EUR to the user |
| `backdated` | a transfer dated up to an hour in the past | a user to a user, with `timestamp` |
| `credit` | a draw on a credit line, in Numscript | `credit:<n>` to a user, `allowing overdraft up to [USD/2 5000]` |
| `repay` | a repayment of a credit line | a user to `credit:<n>` |
| `revert` | the revert of an acknowledged transaction, half of them with `atEffectiveDate=true` | the reversed postings |
| `bulk` | two transfers in one atomic bulk (`?atomic=true`) | two transactions or none |
| `open` | a new shop's first two sales, one in each asset, sent at the same moment (added after finding 1) | `world` to `shops:<n>` in USD, and in EUR |

A transfer from a user without the funds is refused (`INSUFFICIENT_FUND`), which the client
records as a certain failure. The client gives up after three attempts of 4 seconds each. An
attempt without an answer, or with a 5xx, makes the outcome unknown. A connection refused means
the attempt reached no ledger. Each operation is one token in the client's records, for example
`t17~u5,u4,E,154+u5,f,E,2`: transaction 17 moves 154 EUR cents from `users:5` to `users:4` and 2
to `fees`. Every transaction carries the metadata `hydo_op=t17`.

## The rules

The rules come from the ledger's documentation (the API reference and the pages on
idempotency, bi-temporality and reverting) and from the standard rules for money movement in
`hydo guide workloads`. Each is one or two files in `hydo/checks/`. The comments in those files
number the domain rules with letters; this page numbers them 1.1 to 1.8.

| id | rule | where it is read | file |
|---|---|---|---|
| 1.1 | every transaction balances: its moves of each asset sum to zero, two moves per posting | PostgreSQL | `final_tx_balanced.sql` |
| 1.2 | a balance is the sum of its postings: `accounts_volumes` equals the sum of the moves, and every move's post-commit volumes are the running sum | PostgreSQL | `final_balance_is_sum_of_moves.sql` |
| 1.2' | the same through the API: each account's volumes are the sum of the postings of the listed transactions | the API | `final_api_sums` |
| 1.3 | each move's effective volumes are the sum of the moves up to its effective date, backdated ones included | PostgreSQL | `final_effective_volumes.sql` |
| 1.4 | no overdraft: users, shops, and `fees` never below zero, a credit line never below -5000 | PostgreSQL, every 2 s, and after the faults | `always_no_overdraft.sql`, `final_no_overdraft_after.sql` |
| 1.5 | one transaction per operation: an acknowledged operation is in the ledger once, an unanswered one at most once, a refused one never | the client's records and the API | `final_recorded_once` |
| 1.6 | a revert undoes its transaction: one revert per reverted transaction, its moves the mirror of the original's; with `atEffectiveDate` the original's date, else the time of the revert | PostgreSQL | `final_reverts_restore.sql`, `final_revert_dates.sql` |
| 1.7 | an atomic bulk is applied whole or not at all | PostgreSQL | `final_bulk_atomic.sql` |
| 1.8 | the log is whole: each log's hash chains to the one before, every transaction has its log, and a replay of the log gives every balance | PostgreSQL and the API | `final_log_chain.sql`, `final_log_replay` |
| 2 | faults do not change the business result. The acknowledged operations, and the unanswered ones at most once, give every account's volumes. The API and PostgreSQL both show those volumes | the client's records, the API, and PostgreSQL | `final_acked_determine_ledger` |

Rule 2 knows nothing of the ledger but the postings in each token. It counts the transactions
with each operation's `hydo_op` to learn which unanswered operations took effect. Then it adds up
the postings of every operation found and compares the volumes per account and asset. A revert's
token names the operation it reverts, and its postings are those reversed. The metadata is only
a label: a missing or doubled operation shows in the counts, and wrong amounts show in the
volumes.

Each SQL rule was checked against a deliberate break of a native PostgreSQL with the ledger's
data, inside a transaction that was rolled back. The breaks:

- an amount, a balance, or an effective volume changed by one;
- an account taken below zero;
- a revert's moves deleted;
- a bulk's second part relabelled;
- a log's hash zeroed.

Each check returned the broken rows. The Go
rules were checked by the client mistake below.

## Without faults

The final image (`explore-ob-formance-ledger-arm64.cpio.gz`, sha256 `1fce9750…`), `disk=none |
run`, twice, held every rule with fp `8fd7ae2f4353b6de` and ctl `e49e0e20c1174074`. The run started 58 commands: 44
succeeded and 14 were refused for lack of funds or found nothing to revert. The client saw 51
operations acknowledged, and all 14 rules held. Three operations were answered `500` once and
acknowledged on the retry: that is finding 1.

## What was found

### 1. Two transactions that first use an account in two assets at once fail one of them (a ledger bug)

When two transactions are the first to use an account, at the same moment and in two different
assets, one of them fails with `500 INTERNAL`. The ledger logs `upserting accounts: ... duplicate
key value violates unique constraint "accounts_ledger" (SQLSTATE 23505)`. The cause, in three
steps:

1. Each transaction locks only its own rows of `accounts_volumes`: one row per pair of an account
   and an asset.
   `(shops:1, USD/2)` and `(shops:1, EUR/2)` are different rows, so neither transaction waits for
   the other. In one asset the second waits for the first, and then sees the account.
2. `UpsertAccounts` (`internal/storage/ledger/accounts.go`) reads the existing addresses in a CTE,
   `existing_accounts`, and inserts the others (`WHERE d.address NOT IN (SELECT address FROM
   existing_accounts)`). Both statements see no row, so both insert the account.
3. The second insert waits on the unique index `accounts_ledger (ledger, address)` and fails when
   the first transaction commits. `forgeLog` retries only deadlocks and idempotency key conflicts,
   so the request fails with `500`, and the whole transaction is rolled back.

v2.3.24 inserted with `ON CONFLICT (ledger, address) DO UPDATE`, which waits for the other insert
and then updates the row. The CTE came with pull request 1064, `feat: introduce schemas`, merged
on 2025-12-12. It is in every v2.4 release and on the branch `main_v2`. No upstream issue names it.
The draft of a report is outside the repository.

**Native, without hydo.** PostgreSQL 16.16 in Docker and the ledger on the Mac, each built from its
tag. 200 pairs of deposits from `world`, each pair to a new account, one in `USD/2` and one in
`EUR/2`, sent at once with `curl`:

| ledger | pairs with one `500` | pairs with two `200` |
|---|---|---|
| v2.4.18 | 192 | 8 |
| v2.4.18, both deposits in `USD/2` | 0 | 200 |
| v2.3.24 | 0 | 200 |

The scripts of these runs, with their steps, are in [`native/README.md`](native/README.md).

**The fix, tested natively.** `fix-new-account-race.patch` adds `ON CONFLICT (ledger, address) DO
UPDATE` to the insert of `UpsertAccounts`. The clause waits for the other transaction's insert and
then changes the row as the CTE's `updated_rows` changes an existing account:

- the metadata of the request is merged into the row;
- `first_usage` becomes the earlier of the two dates;
- `updated_at` is set.

As in `updated_rows`, the row changes only when the request adds metadata or has an earlier date.

The metadata to merge comes from `data_batch`, not from `excluded`. In v2.4 the inserted row
also holds the schema's default metadata, which applies only when an account is created, so
`excluded.metadata` would overwrite the other transaction's values with defaults. The ledger's
transactions run at PostgreSQL's default isolation level, read committed, at which `ON CONFLICT DO
UPDATE` updates a row that another transaction committed after the statement began.
`ON CONFLICT DO NOTHING` also stops the `500`, but it drops the second transaction's changes to
the account. The last row of the table below shows that.

The test used the same Mac and PostgreSQL 16.16 in Docker, with a new database for each row.
Each row sent 200 pairs of requests, each pair to a new account:

| ledger | the two requests of a pair | pairs with one `500` | pairs with two `200` | the 200 accounts afterwards |
|---|---|---|---|---|
| v2.4.18 | `USD/2` and `EUR/2` | 196 | 4 | 106 accounts lack the `USD/2` deposit, and 90 lack the `EUR/2` one |
| v2.4.18 with the patch | `USD/2` and `EUR/2` | 0 | 200 | each has both deposits, and its `first_usage` is the date of its first move |
| v2.4.18 with the patch | both in `USD/2` | 0 | 200 | each has 200 cents |
| v2.4.18 with the patch | `USD/2` with the account metadata `a=1`, and `EUR/2` dated one hour back with `b=2` | 0 | 200 | each holds `a` and `b`, and its `first_usage` is the earlier date |
| v2.4.18 with `DO NOTHING` | the same as the previous row | 0 | 200 | 4 hold `a` and `b`, and 108 have the earlier `first_usage` |

The unmodified build failed 196 pairs in this run and 192 in the first one. The ledger's own
storage tests passed on the patched tree: `go test -tags it ./internal/storage/ledger/`, 49 tests
with 215 subtests, against PostgreSQL 16.16 in Docker (`POSTGRES_VERSION=16.16`). To build the
patched ledger, apply the patch in the clone after `build.sh` has checked out the tag
(`git -C work/ledger apply ../../fix-new-account-race.patch`). Then run the `go build` line of
`build.sh` there. `build.sh` itself checks out the tag again and checks the unpatched pins.

**Under hydo.** The error logs found it, not a business rule. The first sighting was exploration
`e5` (seed 11, `K=ledger@12+1 N=postgres@7-9:delay,809ms T=22:300`): a new error kind in a run
whose rules all held. The command `parallel_driver_open` sends a new shop's first two sales
together, one in each asset. With it the bug shows in the run without faults: 3 requests failed
with `500` and were acknowledged on the client's retry. No rule failed, for two reasons. The client retries a
`500` with the same idempotency key, and the retry finds the account. A `500` is an unknown
outcome, so rule 2 allows it to be absent. A client that does not retry loses a valid payment.
The two error kinds are in `known-errors.yaml` as a known ledger bug, so that explorations show
other kinds.

### 2. Retries without idempotency keys record a payment more than once (a client mistake)

The build parameter `ik=0` makes the client retry without `Idempotency-Key` and without the
bulk's `ik`. The idempotency page recommends keys for the use "Retry logic for network
failures." The cause, in three steps:

1. The client posts a transfer. The ledger commits it in PostgreSQL and writes its answer.
2. The network loses or delays the answer past the client's timeout of 4 s. The client sees no
   answer and posts the same transfer again.
3. Nothing ties the second request to the first: there is no key and no reference. The ledger
   commits a second transaction, and a third after a third attempt.

The exploration of the `ik=0` image (`e5`, 20 schedules) found it at seed 15 and shrank it in 80
runs to one fault: `disk=none dseed=b988c2bd3620cc45 | run O=ledger@20-25:resp:loss,81%
W=ca251a75,p5,q3,r200,oparallel_driver_repay+parallel_driver_revert`, VIOLATION
`acked_determine_ledger` (2 operations twice), fp `3b4c45cc98cd20b9` on both runs. It is saved
as `hydo/regressions/retry-after-lost-answer.schedule` with `# expect: ok`: on the default image
the same schedule passes (`63cb265786f6b644`), because the ledger answers the retry from the key.

| image | schedule | verdict | fp (two runs) |
|---|---|---|---|
| `ik=0` | `disk=none \| run N=ledger@5-9:delay,2500ms` | VIOLATION, 4 operations twice | `73c2ebfcc33c96a2` |
| `ik=0` | `disk=none \| run N=ledger@2-12:delay,3500ms` | VIOLATION, 11 operations duplicated | `6747fd7607fb7a20` |
| `ik=1` | `disk=none \| run N=ledger@5-9:delay,2500ms` | OK: 4 unanswered operations took effect once each | `7f161bb038cef9a4` |
| `ik=1` | `disk=none \| run N=ledger@2-12:delay,3500ms` | OK: 11 unanswered operations took effect once each | `7ee0ee8a55962238` |

Two rules failed: 1.5 (`recorded_once`) and 2 (`acked_determine_ledger`). Every other rule held,
because each copy is a balanced, complete transaction. Its moves sum to zero, the volumes are the
sum of the moves, no account went below zero, and the log holds every copy. A rule over the
ledger's state alone cannot tell a copy from a second real payment. Only the client's record of
what it sent can.

A timeout alone does not make a copy. When the client gives up before the commit, the ledger
cancels the request and rolls the transaction back. Under a pause of PostgreSQL for 5 s
(`P=postgres@6~100:5000`), the `ik=0` client's retries were acknowledged and nothing was doubled
(`42a348cfc312265d`, twice). A copy needs the answer lost after the commit.

### 3. A backdated transfer leaves a past balance below zero (documented)

An owner could expect this rule: an account without overdraft is never below zero, at any date.
The ledger does not promise it. A backdated transaction is checked against the final balance
only. The bi-temporality page says so: "The ledger does not validate the intermediate states of
the ledger, only the final state." No fault is needed. On the native ledger, on a new ledger `probe`:

1. Post `world` to `users:a`, 100 `USD/2`, now.
2. Post `users:a` to `users:b`, 100 `USD/2`, with `timestamp` one hour ago. It is accepted: the
   final balance of `users:a` is 0.
3. `GET /v2/probe/volumes?endTime=<30 minutes ago>` reports `users:a` at `-100`.

The backdated command does the same in the hydo runs: on the native run of the client, a query
for users' moves whose `post_commit_effective_volumes` are below zero returned rows. It is not a
check, since it would fail on documented behaviour.

### 4. Lost acknowledged payments on a disk that lies, or with `synchronous_commit=off` (expected)

The swarm exploration (`e3`) drew the disk faults that the process strategy did not. In 5 runs a
power cut lost acknowledged operations: 3 to 34 of them. Three had a lying disk, which drops
writes it acknowledged. Two had the knob `S=off` on an honest disk; PostgreSQL documents that a
crash can then lose the last acknowledged commits. The explorer put them in the
classes `lying-disk` and `unsafe-knob`. Each lost operation is a whole transaction that PostgreSQL
lost with all its moves and its log, so every rule over the ledger's state held ("0 volumes
differ"). Only rules 1.5 and 2 saw the loss, from the client's record of what was acknowledged.
The other failures of `e3` come from the disk faults too. PostgreSQL did not start on a disk with
I/O errors, or could not recover after a lying disk, or a lying disk left the data disk
unmountable.

### Not found

Apart from the disk faults of finding 4, no rule failed with idempotency keys on. In particular:

- No acknowledged operation was lost, none was doubled, and no refused one was applied, also
  across power cuts with the honest disk and PostgreSQL's default `synchronous_commit=on`.
- No overdraft: the overdraft check takes row locks on the volumes it reads (`SELECT ... FOR
  UPDATE` in `GetBalances`), and up to six concurrent clients never broke it.
- Effective volumes stayed exact under backdated transfers, reverts with `atEffectiveDate=true`,
  and clock steps from 30 days back (`T=16:-2592000`) to one year forward (`T=26:31536000`).
- Every revert mirrored its transaction and had the documented date, and no transaction was
  reverted twice.
- Both transactions of every acknowledged atomic bulk were there, and none of a refused one.
- The audit log's hash chain held, and a replay of the log gave every balance.

## Which rule found what, and what it cost

"Rules written" counts the files in `hydo/checks/` for the rule. The lines are the query or the
Go function, without comments. The client itself (operations, records, retries) is 1,000 lines of
Go, shared by every rule. The runs are the 177 runs with faults: 156 explored schedules and 21
directed runs. In 16 of them the run ended before the checks. Six directed runs met the limit of
gap 2, and in 10 swarm runs PostgreSQL or the data disk did not come back. The date half of rule 1.6
joined after `e2`, so it ran in 107 of them.

| rule | runs | findings | rules written |
|---|---|---|---|
| balanced transactions (1.1) | 177 | none | 1 SQL file, 12 lines |
| balance is the sum of the postings, in PostgreSQL and the API (1.2) | 177 | none | 1 SQL file, 21 lines; 1 program, 30 lines of Go |
| effective volumes (1.3) | 177 | none; finding 3 came from a query beside it, not a check | 1 SQL file, 10 lines |
| no overdraft (1.4) | 177 | none | 2 SQL files, 5 lines each |
| one transaction per operation (1.5) | 177 | findings 2 and 4 | 1 program, shares the 156 lines of rule 2 |
| reverts (1.6) | 177 | none | 2 SQL files: 19 lines, and 7 lines |
| atomic bulks (1.7) | 177 | none | 1 SQL file, 5 lines |
| the audit log (1.8) | 177 | none | 1 SQL file, 12 lines; 1 program, 52 lines of Go |
| the acknowledged operations determine the ledger (2) | 177 | findings 2 and 4 | 1 program, 156 lines of Go and the token parser (29 lines) |
| error logs as findings (`errors: true`, no rule written) | 177 | finding 1 (ledger bug) | 1 line in `hydo.yaml`, 31 reviewed kinds |

What this says about business bugs:

- The rules over the ledger's own state are cheap (5 to 21 lines each) and found nothing. The
  ledger keeps them by construction. The moves of a transaction come from its postings in one
  database transaction. Row locks and triggers keep the volumes and the effective volumes. The faults never split a database transaction.
- The rule that compares the ledger with what the client saw is the expensive one: the client
  must record each operation in a form the check can replay. It is also the only kind that can
  see a duplicate or a lost payment. A duplicate is a valid transaction by every rule of the
  ledger, and a lost payment leaves no trace in the ledger's state.
- The ledger bug broke no business rule at all, because the client handled its symptom (a `500`)
  the way a careful client does. The error log, which needed no rule, was the only signal.

## The explorations

| name | image | strategy | schedules | failures | notes |
|---|---|---|---|---|---|
| `e1` | before the error-log `normalize:` and the compact tokens | `process --new-faults`, seeds 1-30 | 30 | 0 | 30 verdicts OK; the exploration was stopped while it shrank new error kinds one by one (gap 3) |
| `e2` | before `parallel_driver_open` | `process --new-faults`, seeds 31-70 | 40 | 0 | 22 new error kinds, all connection errors under faults, reviewed into `known-errors.yaml` |
| `e5` | `ik=0` | `process --new-faults`, seeds 1-20 | 20 | 1 | finding 2, shrunk; finding 1 seen as a new error kind (seed 11) |
| `e3` | final | `swarm --new-faults`, seeds 101-106 and 107-136 (without shrinking) | 36 | 16 | finding 4 (5 runs). PostgreSQL did not start on I/O errors (4 runs) or did not recover after a lying disk (2). A lying disk left the data disk unmountable (4), and once the API unreadable |
| `e4` | final | `process --new-faults --timing --preempt`, seeds 201-230 | 30 | 0 | 15 schedules with forced preemption points (`pct=`); 4 new error kinds, among them one deadlock in PostgreSQL that the ledger retried |
| directed | `ik=1` and `ik=0` | schedules aimed at lost answers (network delays, one-way loss and delays, pauses of the ledger and of PostgreSQL) | 21 runs | the 2 `ik=0` violations of finding 2 | 6 runs ended without a verdict (gap 2) |

No exploration exercised `fault.pause_mid_request`, the property that a pause began while a
request was in flight. The process strategy drew no lying disk and no I/O errors; only the swarm
strategy did. Its failures are faults a team would turn off or accept, not ledger findings.

## What hydo lacked

1. **A recovery boot hangs when the records it carries pass 4 KiB (Mac and KVM engines).** After a
   power cut the host writes `$HYDO_ACKED`, the failed ids and `$HYDO_OUTCOMES` into the recovery
   boot as several messages, one after another, without waiting for a reply
   (`hydo_control::schedule`, the `post` of each chunk). The engine types each message into a
   4,096-byte receive queue and drops what does not fit: `hvmx: seed-1: send 690: 1566 of 3129
   bytes do not fit in the receive queue`. The agent never sees a complete message, and the run
   ends after 50 million exits with `ERROR recovery: exit budget exhausted`, about five minutes of
   wall time each. The first exploration lost 3 of 6 schedules to it. The example now keeps its
   records small (short account names in the tokens, 30 rounds instead of 40). Open item 82.
2. **A run holds at most 1,024 control messages (every engine).** Under a slow network between the
   ledger and PostgreSQL (`N=postgres-ledger@2-28:delay,3000ms`) the 30 rounds last longer. The
   checks every 2 s and the queue polls add up, and the run ends without a verdict: `the
   controlled run broke: unexpected engine output: "send more than 1024 sends in one run"`. Six of the
   directed runs ended this way. Open item 83.
3. **Error kinds split by a request id.** The ledger's log lines end with `trace-id=qjGcd4lJvd`, ten
   letters and digits. The default template blanks mixed ids only from 16 characters, and only
   with a digit, so every request was its own kind. The explorer shrinks each new kind (8 by
   default), and the first exploration spent its time on that. `normalize:` on the service and
   `--errors-shrink 0` fixed it for this project. Open item 84.
4. **`hydo errors accept` writes the login name into a tracked file.** Its `by:` field defaults to
   the user's login, which this repository's rules forbid in tracked files; `--by` replaces it.
   Open item 84.

Milestone 222 fixed all four. The engines type a message only when it fits in the receive
queue. A run holds 65,536 messages. The default template blanks `trace-id=qjGcd4lJvd` and other
request ids. `hydo errors accept` writes `by:` only with `--by`. A copy of this example with 40
rounds and the full account names in the tokens ran `disk=cut dseed=1 | run C=38` on the Mac.
Its recovery boot carried about 5.8 KB of records. Before the fix it ended with `ERROR
recovery: exit budget exhausted` after 121 s. After the fix it ended with `OK invariants=14
held` in 21 s, and its replay gave the same fingerprint. This example keeps its 30 rounds and
short names, so that the results above stay comparable.

Not a gap of this onboarding, but met on the way: the Mac release 0.3.1 answers `hydo explore
--help` only next to a built image, fixed on `main` by Milestone 210.

## Time

About an hour and a half of wall time on the Mac, from the first build to the last exploration,
with one VM at a time. A run from a fresh boot takes 20 to 30 s. With one worker, `e2` ran its
40 schedules in 226 s, a mean of 4.8 s each, and `e5` ran 20 schedules in 406 s.
