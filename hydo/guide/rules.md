# Rules from what the project promises: `hydo rules`

A rule is a statement about the system's state that must hold after every run, for example that
no subscription is invoiced twice for one period. hydo checks rules as the files of
`hydo/checks/` (`hydo guide checks`). This page says how to find rules in a project's own
sources, and how to test each rule on runs without faults before anyone relies on it.

## Find the evidence: `hydo rules scan`

```sh
hydo rules scan [DIR] [--kind K] [--json] [--max N]
```

The command reads the project in DIR (default `.`) and lists each promise it finds with its file
and line. It knows no domain. It prints five kinds of evidence:

| kind | where it looks | what it finds |
|---|---|---|
| `schema` | `.sql` files, migrations, models, and schema files | `CHECK`, `UNIQUE`, foreign keys, enumerations, validations |
| `state` | the same files | status and state columns, and the values a record moves through |
| `api` | OpenAPI files, `.proto` and GraphQL schemas | idempotency keys, `409` and `422` answers, limits |
| `docs` | Markdown, reStructuredText, and AsciiDoc files | promise words: never, exactly once, at most, idempotent, guarantee, atomic, duplicate |
| `tests` | test files | assertions about refusals, duplicates, limits, and conflicts |

It writes no rule. The scan is a list of places to read. A file of documentation or tests gives
at most 40 lines; a schema or API file gives all of them. `--kind K` shows one kind, `--json`
prints every line of evidence, and the text output shows `--max N` lines of each kind (default
200).

A sixth section, `clock`, lists the code that measures a lease, a timeout, or a delay with the
wall clock. Such code fails when the clock is stepped, so it needs clock-step schedules, not a
rule. `hydo rules scan --kind clock` proposes those schedules (`hydo guide faults`, "Find the
durations measured on the wall clock").

A seventh section, `writes`, lists the routes of the project's API that write. The next section
describes it.

`hydo rules scan --kind sync` is the sync audit: it lists the code that can answer a write before
the write is on the disk, and proposes power cuts right after that answer. The scan without
`--kind` does not show it (`hydo guide faults`, "Find the writes acknowledged before they are
synced").

## List the routes that write: `hydo rules scan --kind writes`

```sh
hydo rules scan [DIR] --kind writes [--json] [--max N] [--all]
```

A workload has one command per client operation (`hydo guide composer`). An operation that no
command calls is never tested under faults. On Meilisearch, the index swap, the dump, and the
payload of a document addition held the three bugs that hydo found. This command lists every
operation that the project's API offers to write, so that the workload can cover each one.

It reads the project's own sources and description files, as text:

| what | where it looks |
|---|---|
| HTTP routes with `POST`, `PUT`, `PATCH`, and `DELETE` | Rust (axum, actix-web, and the route tables of a macro) |
| | Rust enumerations of endpoints: an `enum` whose variants a match on the request's method names (`Method::PUT => ...`), or that a macro's table lists after their methods (`POST CreateKey (body)`) |
| | Go (net/http, gorilla/mux, chi, gin, labstack's router, and httprouter) |
| | JavaScript and TypeScript (express, fastify, koa, hapi, and NestJS) |
| | Python (FastAPI, Flask, and Django), Java and Kotlin (Spring and JAX-RS) |
| | C and C++ servers that register a route with a call named for its method, such as Typesense's `server->post("/collections/:collection/documents", post_add_document)` and its kin `put`, `patch`, and `del` |
| RPC methods whose names do not say that they read | `.proto` files with their `google.api.http` mappings |
| | a Go type that embeds `Unimplemented<Service>Server`, a Rust `impl` of a tonic service |
| operations | OpenAPI 3 and Swagger 2 files, also through `$ref` files |
| commands flagged as writes | the command table of a Redis-compatible server in C or C++: an entry that names a command, its arity, and a write flag, such as `MakeCmdAttr<CommandSet>("set", -3, "write", ...)`, `{"set", setCommand, -3, "write ...", ...}`, a generated `MAKE_CMD(..., CMD_WRITE, ...)` table, or `CI{"SET", CO::WRITE, -3, ...}` |

It leaves out test files, the test code inside source files, examples, vendored code, and
generated code. It also leaves out a `POST` route or an RPC method whose name says that it
reads, such as `/query` or `GetMemo`; `--all` lists them. It joins the prefixes that a router
adds, such as axum's `.nest("/v1", ...)`, gin's `r.Group("/v1")`, and Spring's class-level
`@RequestMapping`.

Each route has its handler with the file and the line, the place that declares it, and why it
counts as a write. A route that a `.proto` file and the code both declare is listed once. The
routes are grouped by resource, the first two segments of the path that are not parameters or
versions. Each route has one of four classes, and the list puts durable writes first:

| class | what the route changes | examples |
|---|---|---|
| `data` | the system's data | an addition of documents, a new order, a deletion |
| `admin` | the stored data as a whole | a snapshot, a dump, a compaction, a migration |
| `session` | a sign-in or a token, not the system's data | a login, a token refresh |
| `runtime` | the process's own state, not durable data | a health check, metrics, profiling, a reload |

On Meilisearch v1.57.0 the report's first lines and its entry of the index swap read:

```text
hydo rules scan --kind writes: 83 routes that write, in 23 resources, in 576 files of meilisearch
  by class: 78 data, 4 admin, 1 runtime
  left out: 10 POST routes and RPC methods whose names say that they read

swap-indexes (1)
  POST   /swap-indexes  [data]
         handler swap_indexes (crates/meilisearch/src/routes/swap_indexes.rs:92)
         declared at crates/meilisearch/src/routes/swap_indexes.rs:21: POST in a route table of a macro; it changes the system's data (swap)
```

An operation of an enumeration of endpoints has the path `<Enum>::<Variant>`, because its URL
comes from the request's query and headers. Its handler is the call that the code makes for the
variant. On Garage v2.5.0 the entry of CopyObject, in the resource `object`, reads:

```text
  PUT    Endpoint::CopyObject  [data]
         handler handle_copy (src/api/s3/copy.rs:47)
         declared at src/api/s3/router.rs:480: PUT in an enumeration of endpoints; it changes the system's data
```

A command of a command table has the method `CMD` and the command's name as its path. Its
resource is the command's group, or the name of its table's file (`cmd_string.cc` gives
`string`), and its handler is the class or function that the entry names. A command that the
entry flags as a read (`read-only`, `CMD_READONLY`) is left out, and so is an entry in a comment.
On Kvrocks 2.17.0 the scan lists 142 write commands in 18 resources; the entry of `SET` reads:

```text
  CMD    SET  [data]
         handler CommandSet (src/commands/cmd_string.cc:341)
         declared at src/commands/cmd_string.cc:890: its command table entry flags it as a write; it changes the system's data (set)
```

Give the workload one command for each route of class `data` or `admin`, or say why a route
has none. `--json` prints every route with its rank, and the onboarding skill reads it. The
scan without `--kind` prints one line for each route in its `writes` section.

The scan reads text without a compiler, so it misses a route that a program builds at run time.
A prefix that a function adds from another file is not always joined. An RPC protocol of a
project's own, such as a binary protocol over TCP, is not read.

## Turn evidence into rules

These kinds of evidence give rules that hold for any business system:

- **A constraint that only the app enforces.** A `validates ... uniqueness` or
  `numericality` in a model, or a check in a service, is not a database constraint. Two
  concurrent requests or a retry can break it. Propose it as a rule.
- **A partial unique index.** An index such as `UNIQUE (subscription_id, period) WHERE recurring`
  states the rule for some of the rows. Propose the rule for all the rows that mean the same
  thing, for example no two invoices of any reason for one subscription and one period.
- **A database constraint.** The database refuses rows that break it, so a rule that only
  repeats it cannot fail. Propose the rule across tables instead, for example that a total
  equals the sum of its lines.
- **A status column.** Each state that the documentation says leads to an action gives an
  eventually rule, for example that a terminated subscription gets its final invoice. A
  final state gives a rule that it does not change.
- **A promise word in the documentation.** Words such as `never`, `exactly once`, and `at most`
  state a rule. Quote the sentence as the evidence.
- **The client's acknowledged operations.** The client records each operation it saw
  acknowledged (`hydo guide composer`). Faults must not change the business result that these
  operations determine. Every acknowledged operation is in the store once with its values, and
  no operation that was told it failed is there. Propose this rule for every project. In
  hydo's examples of a ledger and a billing system, it found failures that every domain rule
  missed.

Do not propose a rule that the project does not promise. A behaviour of today's code is not a
promise.

## The candidates file

Write the proposed rules into one YAML file, for example `hydo/rules.yaml`:

```yaml
format: 1
store: db                  # optional: the default endpoint of workload/helper_sql.conf
rules:
  - id: one_invoice_per_period
    says: A subscription is invoiced at most once for one billing period, whatever the reason.
    kind: sql
    confidence: high
    evidence:
      - {file: db/structure.sql, line: 12174, quote: "UNIQUE ... (subscription_id, charges_from_datetime, charges_to_datetime) WHERE recurring"}
    query: |
      SELECT a.subscription_id FROM invoice_subscriptions a JOIN invoice_subscriptions b
        ON a.subscription_id = b.subscription_id AND a.id < b.id
       AND a.charges_from_datetime < b.charges_to_datetime
       AND b.charges_from_datetime < a.charges_to_datetime
    cases: SELECT count(*) FROM invoice_subscriptions
  - id: acked_operations_kept
    says: Every operation the client saw acknowledged is stored once.
    kind: command
    confidence: high
    evidence: [the client's records ($HYDO_ACKED)]
    run: hydo-sql check exactly-once --left 'acked=hydo acked' --right 'stored=sql db "SELECT op_id FROM orders"'
    cases: hydo-sql read sql db 'SELECT count(*) FROM orders'
    mutate: hydo-sql read sql db 'DELETE FROM orders WHERE id = (SELECT min(id) FROM orders)'
```

The keys of a rule:

| key | needed | meaning |
|---|---|---|
| `id` | yes | letters, digits, `.`, `_` and `-`; it becomes the check's name |
| `says` | yes | the rule in plain words, for the person who reviews it |
| `kind` | yes | `sql`, `command`, `messages` or `history` |
| `confidence` | no | `high` (stated in words or a constraint), `medium` (the default: implied by the code), `low` (only the domain suggests it) |
| `evidence` | no | a list of `file:line` texts or `{file, line, quote}` maps |
| `query` | `sql` | a query that returns the rows that break the rule |
| `store` | no | the endpoint of `workload/helper_sql.conf` for a `sql` rule |
| `run` | `command` | a check command: exit 0 holds, 1 violated, 2 cannot evaluate |
| `cases` | no | what the rule judges, as a count: a query for `sql`, else a command whose last line ends with the number |
| `within` | no | seconds the rule may take to come true, for a rule about what the system does after the workload |
| `mutate` | no | a change of the state that must break the rule: a statement for `sql`, else a command |

Let `cases` count the rows that the rule's query looks at, not the operations the rule is about.
A rule whose query never matches a row holds, and only a count of those rows shows it.

A `messages` or `history` rule needs the workload to record what it did (`hydo guide messages`,
`hydo guide checks`). `hydo rules check` lists it but does not run it.

## Validate the rules: `hydo rules check`

```sh
hydo rules check hydo/rules.yaml [IMG] [--runs N] [--mutate] [--write-checks DIR]
```

The command runs the image's workload without faults: the schedules
`disk=none | run W=00000001`, `W=00000002`, and `W=00000003` (`--runs N` for another number). An
image without composed commands gets one run of `disk=none | run`. `--schedule S` runs S
instead, for example a saved regression's schedule: a rule that fails there catches that
finding. The image is not rebuilt: hydo
adds every candidate to the run as a final check. The project's own checks are left out, so that
each candidate is judged alone; `--keep-checks` keeps them. The `eventually_` commands still run
before the candidates.

Each rule gets one status:

| status | meaning | what to do |
|---|---|---|
| `holds` | it held in every run and judged at least one case | show it to the owner |
| `vacuous` | it held, but its `cases` count was 0 in every run | the workload never reaches what the rule judges: extend the workload, or drop the rule |
| `fails-clean` | it failed in a run without faults | the rule is wrong, or the system has a bug on its normal path: read the rows it printed and decide |
| `unevaluated` | exit 2, a missing program, a timeout, or no result | fix the rule's command |
| `not-run` | a `messages` or `history` rule | write it into `hydo/` and run it with `hydo run` |

A rule without `cases` that holds is reported as `holds`, with the note `cases not counted`. Such
a rule may judge nothing. `--mutate` makes one more run of the first schedule. In it, each rule that
holds and has a `mutate` runs, then its change is made, then the rule runs again. The rule must
fail the second time (`caught`). A rule that still holds is `blind` to that change. A rule that
already fails before its own change, because the change of a rule before it broke it, gets a run
of its own.

The command prints one line per rule and a summary, for example
`8 proposed: 5 hold, 1 vacuous, 1 fail without faults, 1 unevaluated, 0 not run`. The details,
with each run's case count and the failing rows, are in `hydo-out/rules/rules-check.json`
(`--out DIR` for another directory). The consoles of the runs are in `run-1/`, `run-2/`, and so on.

`--write-checks hydo/checks` writes each rule that holds as a check file: `final_<id>.sql`, or an
executable `final_<id>` for a `command` rule. A rule with `within` becomes an `eventually_`
check with `within=N after=commands`. Each file names its evidence in comment lines. Then
rebuild the image, and explore with faults (`hydo explore`).
