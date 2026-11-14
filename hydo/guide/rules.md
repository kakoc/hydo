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
