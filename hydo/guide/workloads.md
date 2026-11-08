# Ready-made workloads and checks (SQL systems, and business rules)

Before writing a client or a check by hand, look here: for a system with a SQL database, hydo
writes the workload and its checks, and for business rules it reads the stores and the fake
outside world for you. The full reference is "SQL systems: ready-made workloads and checks" in
`hydo guide manifest`; the design and its reasons are docs/design/workloads.md in hydo's
repository.

## The generator: `hydo sql-gen`

```sh
hydo sql-gen --dialect postgres|mysql|libsql --endpoint NAME=ADDRESS[,replica] ... \
  [--workloads bank,list-append,register,set,counter|none] [--shapes batch,interactive] \
  [--route NAME,...] [--isolation serializable|repeatable-read|read-committed] [--out hydo]
```

- An endpoint's address is how the guest reaches the database: `host:port` of sqld's HTTP API for
  `libsql` (SQLite and libSQL), a DSN for the others
  (`postgres://user:pw@10.89.0.2:5432/db?sslmode=disable`, `user:pw@tcp(10.89.0.3:3306)/db`).
- `,replica` marks an endpoint whose reads may be stale (a read replica). Writes may still go to it
  when it forwards them. Every other endpoint is a primary: its reads are current.
- It writes into `hydo/` (keeps existing files unless `--force`): `workload/helper_sql.conf`
  (everything the guest program reads; edit it freely), `workload/first_sql_init` (the tables,
  prefixed `hydo_`, and their first rows), one `parallel_driver_sql_*` per workload and shape, and
  the checks. `hydo build` stages the guest program `/bin/hydo-sql` because the commands call it.
- Set how many commands run at once in `hydo.yaml`'s `test:` (`parallel`, `per_round`,
  `round_ms`), and the durability knob of the database in `knobs:` yourself
  (`synchronous_commit`, `innodb_flush_log_at_trx_commit`, sqld's `durability_mode`): the knob's
  default is what the system ships with, which is the setting worth testing.

## The workloads and what they check

| workload | operations | checks written |
|---|---|---|
| `bank` | transfers between N accounts | `always_sql_bank_live` (the total and no negative balance on every endpoint, every 2 s), `final_sql_bank_after` (on the primary after the faults), `final_sql_no_negative_balance.sql` |
| `list-append` | transactions of appends and whole-list reads | `list_append_sql.la` (and `.lap` with replicas): hydo's list-append checker |
| `register` | read, write, compare-and-set | `linearizable_sql.rg`: hydo's linearizability checker |
| `set` | insert one unique element | `final_sql_set`: every acknowledged element is there |
| `counter` | add one | `final_sql_counter`: no acknowledged increment is lost |

With replicas it also writes `eventually_sql_replicas_converge` (every replica holds the primary's
rows within 120 s of the heal).

**Shapes.** `batch`: the statements are fixed before the transaction starts and sent as one
request (MySQL: one after another, no decision between them). `interactive`: one statement per
round trip, the client deciding in between (as ORMs do); only this shape can show a lost update.

**What a history may claim.** A level without real time (`serializable`) cannot see a lost write:
an acknowledged append missing after a power cut is not an anomaly there. So the generator claims
real time wherever the reads are current: with one primary, `strict-serializable` for every
transaction; with replicas, `serializable` for all of them (history `la`) and
`strict-serializable` for what a primary ran plus the appends of what a replica served (history
`lap`).

**Outcomes** (each operation's line in `$HYDO_OUTCOMES`): no connection = failed; an error the
server answered before or at COMMIT = failed (serialization failures, deadlocks and `database is
locked` are retried as new transactions, twice by default); no answer to COMMIT (or to a
one-request transaction) = unknown.

## SQL checks: a query that must return no rows

A file `checks/final_<name>.sql` (or `always_`, `eventually_`), **not executable**:

```sql
-- hydo: store=primary every=2
-- hydo-msg: no order is paid twice
SELECT order_id, count(*) FROM payment WHERE status = 'captured' GROUP BY order_id HAVING count(*) > 1
```

`store=` names an endpoint of `workload/helper_sql.conf` (the first primary when left out). Up to
five offending rows are printed; the check fails when there is any. The first three rows are
also in the verdict line and in the check's finding in `result.json`.

Do not order rows by a time the system wrote (`ORDER BY created_at`, `a.created_at <
b.created_at`) in a rule about which event came first. A clock fault (`T=`) steps the realtime
clock, so a later row can carry an earlier time. Compare the order the client saw instead: its
outcome lines in `$HYDO_OUTCOMES`, or a sequence number the client writes into the row. For a project's own tables
without the generated workloads, `hydo sql-gen --workloads none ...` writes only
`workload/helper_sql.conf` (the `dialect` and `endpoint` lines).

## Comparing stores and the outside world: `hydo-sql check`

`hydo-sql read` and `hydo-sql check` are `hydo-read` and `hydo-check` (`hydo guide checks`,
"Reading the stores") with more sources:

| source | records |
|---|---|
| `sql <endpoint> '<query>'` | the rows of any query on a configured endpoint |
| `libsql <host:port> '<query>'` | the rows of a query through sqld's HTTP API |
| `dump <endpoint>` | the workloads' rows, one item per row |
| `world <ep>.<route>\|<route>\|* [--all]` | the fake outside world's record: one record per request that took effect (`--all`: every request), with its fields `ref`, `idem`, `route`, `out`, ... |

"Every captured payment in our books was captured at the provider, and the other way round":

```sh
hydo-sql check equal --left 'books=sql db "SELECT intent FROM payment WHERE captured"' \
                     --right 'stripe=world stripe.capture --field ref'
```

## Business rules: ask the owner

hydo cannot guess a domain's rules; ask the project's owner which of these hold, then write each
as a SQL check or a `hydo-sql check` line:

- **Commerce:** units sold <= units stocked; units sold = units reserved; one payment per order;
  no order without a payment; no money held (authorized) without an order; refunded <= captured;
  every acknowledged checkout has its order.
- **Money movement:** the sum over accounts never changes (transfers only move money); no balance
  below its limit; every transfer has both legs or neither; ledger entries sum to the balance.
- **Inventory and booking:** no seat, room or slot booked twice; a released reservation frees its
  units; reserved never exceeds capacity.
- **Queues and jobs:** every acknowledged job runs at least once (exactly once when promised); no
  job runs after its cancellation; a job's effects appear once.
- **Accounts and identity:** unique emails or user names stay unique; a deleted account has no
  live sessions or orders.
- **Agreement with the outside world:** our books and the provider's record agree (captures,
  refunds, messages sent), compared with a `world` source.
