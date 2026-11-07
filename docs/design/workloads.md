# Ready-made workloads and checks for SQL systems

Status: built (Milestone 177). User decision, 2026-11-07: a user should not have to hand-write
the workload client and the business checks of an SQL system.

## Why

Two onboardings measured where the work goes.

- **libSQL (M173, `hydo/onboard/examples/libsql-replica/RESULTS.md`).** The hand-written client
  `sqlgen` (about 850 lines of Rust: an HTTP client, a JSON parser, Hrana batches and streams,
  seeded transfers and list-append transactions, the outcome rules, two histories) was most of
  the work. Its "What a ready-made SQL workload generator needs" names the pieces: the classic
  workloads with their checkers, both transaction shapes, the outcome rules written once per
  protocol, routing and the history claims it implies, readers for differential checks, SQL
  invariants without a program, durability as a knob, room in the console.
- **Medusa (M174, `hydo/onboard/examples/medusa/RESULTS.md`).** Every business rule was
  hand-written in shell, SQL and awk (136 lines plus helpers). Three of the five rules compare
  the shop's books with what the fake payment provider saw; the fake's record was not a
  `hydo-read` source, so `hydo-check` could not compare the two. The rules themselves (sold <=
  stocked, refunded <= captured, one payment per order, no money held without an order) are
  domain knowledge hydo cannot guess, but can ask for.

## What is built

### One guest program, `hydo-sql`, and one generator, `hydo sql-gen`

The pattern of the gRPC drivers (`hydo-grpc-gen` writes composer commands; the static guest
client `hydo-grpc` runs them). The difference: an SQL system has no schema to read the
operations from, so the generator's input is a few flags (dialect, endpoints with roles,
workloads, shapes), and its output is small shell files a user can read and edit.

- `hydo sql-gen` (in the `hydo` command; `hydo/crates/hydo-cli/src/sqlgen.rs`) writes into the
  project's `hydo/` folder: `workload/helper_sql.conf` (the only thing `hydo-sql` reads),
  `workload/first_sql_init`, one `parallel_driver_sql_*` per workload and shape, and the checks
  of each workload. Existing files are kept unless `--force`.
- `hydo-sql` (`hydo/crates/hydo-sql`, its own Cargo workspace and lock file, static musl,
  byte-reproducible like `hydo-read`) is staged into an image as `/bin/hydo-sql` only when a
  command or a folder file names it, so no other image changes.

### Workloads

| workload | operations | property and checker |
|---|---|---|
| `bank` | a transfer between two of N accounts | the total never changes and no balance is negative, on every endpoint while it runs (`always_`), on the primary after the faults (`final_`) |
| `list-append` | 1 to 4 appends of unique elements and whole-list reads over K keys | hydo's list-append checker (`checks:`), history `la` (and `lap`, below) |
| `register` | read, write, compare-and-set of K registers | hydo's linearizability checker (model `cas`), history `rg` |
| `set` | insert one unique element | every acknowledged element is there (`final_`) |
| `counter` | add one | the counter is at least the number of acknowledged increments (`final_`) |

Every choice comes from the command's `HYDO_SEED`, so the schedule token `W=` decides the
operations and the explorer shrinks them. The tables are prefixed `hydo_` so the workloads can run
inside an application's own database.

**Two transaction shapes.** *Batch*: the statements are decided before the transaction starts
and sent as one request where the protocol has one (PostgreSQL one simple-query message,
`BEGIN ISOLATION LEVEL ...; ...; COMMIT`; libSQL one Hrana batch with conditional steps). MySQL's
text protocol has no one-request transaction without a connection flag, so there the statements
follow each other on one connection with no decision between them, and a transfer is a single
multi-table `UPDATE`. *Interactive*: one statement per round trip and the client decides between
them (read both balances, write computed values), as ORMs run transactions; only this shape can
show a lost update, and only it lets a replica see a transaction's reads before its write.

**Isolation.** `serializable` by default (`BEGIN ISOLATION LEVEL SERIALIZABLE`, MySQL's session
level); `repeatable-read` and `read-committed` are choices, and the generator writes the claim
they allow (serializable -> `strict-serializable`, repeatable read -> `strong-snapshot-isolation`;
read committed allows no list-append claim, so the generator refuses that pair).

### Outcomes, written once (`hydo/crates/hydo-sql/src/db.rs`)

| what happened | outcome |
|---|---|
| no connection (refused, timed out, authentication failed) | failed |
| the server answered an error before COMMIT, or at COMMIT with a rollback | failed; *retryable* for PostgreSQL 40001/40P01, MySQL 1213/1205, SQLite `SQLITE_BUSY` |
| an interactive transaction lost its connection before COMMIT was sent | failed (an uncommitted transaction is rolled back with its session) |
| COMMIT, or a one-request transaction, sent and no complete answer (timeout, reset, HTTP 5xx from a node that forwards writes, PostgreSQL class 08/57/58/XX at COMMIT) | unknown |

Retryable failures are retried as new transactions up to `retries` times (default 2); in the
histories each attempt is its own operation (the failed ones `fail`). Outcomes go where hydo
reads them: `<id> ack|failed|unknown` to `$HYDO_OUTCOMES`, acknowledgements to `$HYDO_ACKED`
(`<id>`; `set-<e>` and `counter-<id>` for the two workloads whose checks read them back).

### Routing and the claims it implies

Each endpoint has a role. A `primary`'s reads are current; a `replica`'s may be stale. An
operation goes to an endpoint of its workload's route (default: every endpoint), the seed
choosing. From the routes the generator and the program derive the histories:

- no replica on the route: one history `la`, claimed at the isolation level's real-time claim
  (`strict-serializable`): an acknowledged append missing from a later read is a lost write;
- replicas on the route: `la` with every transaction, claimed without real time
  (`serializable`), and `lap`, the transactions a primary ran whole plus only the appends of
  those a replica served (writes execute on the primary within the request), claimed with real
  time. This is the libSQL example's lesson (its friction 11): a level without real time cannot
  see a lost write, and this took three attempts by hand.
- registers: a replica's reads are not recorded; its writes are.

With replicas the generator also writes `eventually_sql_replicas_converge`: within 120 s of the
heal, every replica holds exactly the primary's rows (a `hydo-sql check equal` over the `dump`
source).

### Checks people otherwise write in shell

- **A SQL check kind.** A non-executable file `checks/<final|always|eventually>_<name>.sql` is a
  query that must return no rows; its header names the store and the message:

  ```sql
  -- hydo: store=primary
  -- hydo-msg: no account's balance is negative
  SELECT id, bal FROM hydo_accounts WHERE bal < 0
  ```

  The folder reader (`hydo-manifest`, `composer::sql_check`) turns it into the check
  `run: [hydo-sql, none, --file, /hydo/checks/<file>, --on, <store>]`, which prints up to five
  offending rows and exits 1 when there are any. This is an addition to the folder convention: a
  non-executable file was refused before, and an executable `.sql` file stays an ordinary check.
  Nothing in the project model's schema changes (a check's `run` is already an argv).
- **More sources for `hydo-check`.** `hydo-sql read` and `hydo-sql check` are `hydo-read` and
  `hydo-check` with three more stores: `sql <endpoint> <query>` (any query on a configured
  endpoint), `libsql <host:port> <query>` (sqld's HTTP API, for SQLite and libSQL), `dump
  <endpoint>` (the workloads' rows) and `world <ep>.<route>|<route>|* [--all]` (the fake outside
  world's record: one record per request that took effect, with all its fields). So "what the
  provider captured equals what the books captured" is one line:

  ```sh
  hydo-sql check equal --left 'books=postgres <dsn> "SELECT data->>'"'"'id'"'"' FROM capture ..."' \
                       --right 'stripe=world stripe.intent_capture --field ref'
  ```

### The skill and the guide

`hydo guide workloads` explains the generator, the program, the claims and the checks, with a
list of standard rules per domain (commerce, money movement, inventory, queues). The onboarding
skill asks the owner for their domain's rules before writing a check, and reaches for `hydo
sql-gen` before writing an SQL client.

## How it is built without moving a recorded value

- `hydo-read`'s bytes are inside recorded images, and making its PostgreSQL and MySQL internals
  public changes them (measured: `88576fa8...` became `b391e630...`). So `hydo-read`'s sources
  are not changed: `hydo-sql`'s build script copies `hydo-read/src/{pg,mysql,check}.rs` into its
  own build (without their `//!` lines, which cannot be `include!`d) and compiles them as its own
  modules, with a session API beside their private items (`src/pg_session.rs`,
  `src/mysql_session.rs`), and `check.rs` reading through `hydo-sql`'s `read_store`. One source,
  two programs; `hydo-read`'s pins are unchanged (both rebuilt and compared).
- `hydo-build` stages `/bin/hydo-sql` only for a manifest whose commands or folder files name
  `hydo-sql` (the same word test as `hydo-read`'s); every recorded manifest builds the same bytes
  (`hydo-bench rebaseline --check --build-only` on the bare-metal machine).
- The `hydo-world` record keeps no request fields beyond the ref and the idempotency key. Keeping
  chosen fields (a refund's amount) would change `hydo-world`'s bytes and with them every image
  with fakes or proxies (the shop's recorded values), so it is left for a release that re-pins
  `hydo-world` (open item).

## Left for later

- A plain SQLite file source (an embedded database without a server): it needs an SQLite reader
  in the guest program.
- Business operations from templates (parameterized SQL with values drawn from earlier
  acknowledged results, like `hydo-grpc-gen`'s `from:` hints).
- The durability knob per dialect written by the generator (`synchronous_commit`,
  `innodb_flush_log_at_trx_commit`, sqld's `durability_mode`): today a user writes the `knobs:`
  entry.
- Console room for long histories (the libSQL example's friction 13).
- Shipping `guest/hydo-sql` in the release tarballs (a release-script change).
