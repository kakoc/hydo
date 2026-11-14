# Rule suggestion: rules that nobody wrote (Milestone 236, tentative number)

Status: first version. `hydo rules scan` and `hydo rules check` are built. The onboarding skill
proposes rules from the scan's evidence and shows the user only the rules that `hydo rules check`
validated. The proof runs are in `docs/HISTORY.md`, Milestone 236.

## The problem

A rule, or check, is a statement about the system's state that must hold after every run. The
onboarding skill proposed rules from a list per domain (a shop, a bank, a job queue) and asked
the owner to review them. That helps only as far as the lists go. Three onboardings of business
systems showed what the lists miss:

- Formance Ledger (Milestone 215): no rule over the ledger's own state found anything. The
  rule over the client's acknowledged operations found the lost and duplicated payments.
- Lago (Milestone 218): both kinds of rule found the known bug lago-api 6901, and only the
  generic rule found usage that a clock step dropped.
- Blnk (Milestone 224): only a domain rule, the overdraft limit, found Blnk's overdraft bug.

The goal is bugs that nobody wrote a test for, found by rules that nobody wrote. The method must
not depend on the domain: it reads the same kinds of sources for a shop, a booking system, a CRM,
an inventory, or a ticketing system.

## The method

The method has three steps: propose, validate, show.

### Propose: rules from what the project promises

A project states its promises in five places. The skill reads each of them:

| source | what it promises | example of a rule |
|---|---|---|
| the database schema | `CHECK`, `UNIQUE`, `FOREIGN KEY`, `NOT NULL`; a status column and its values | no two rows share an idempotency key; no invoice in state `finalized` without a number |
| the API | the operations, their status codes (`409`, `422`), idempotency headers, limits | a request repeated with the same key creates one object |
| the documentation | words such as `never`, `exactly once`, `at most`, `idempotent`, `guarantee`, `must` | a balance never goes below its overdraft limit |
| the project's own tests | the assertions after an operation | a refund never exceeds the captured amount |
| the client library | what a caller may rely on: retries, keys, error meanings | a failed call left no effect |

`hydo rules scan <dir>` lists this evidence with the file and the line. It reads SQL files,
schema files of common frameworks, OpenAPI files, Markdown, and test files, and knows no domain.
It does not write rules: the skill turns the evidence into rules.

Each proposed rule is an entry of a candidates file (`hydo guide rules` has the format):

- `says`: the rule in plain words;
- `evidence`: the files and lines it comes from;
- `kind`: `sql` is a query that must return no rows. `command` is a check command, for example
  the generic rule over the client's acknowledged operations or a `hydo-sql check` line.
  `messages` and `history` are claims that need the workload's records;
- `confidence`: `high` when the project states the promise in words or in a constraint, `medium`
  when the code implies it, `low` when only the domain suggests it;
- `cases`: what the rule judges, as a count (a query or a command), so that a rule over nothing
  is found;
- `within`: optional, the seconds a rule about what the system does after the workload may take;
- `mutate`: optional, a change of the state that must break the rule.

The generic rule is always proposed: faults must not change the business result that the
client's acknowledged operations determine. In two of the three onboardings it found failures
that every domain rule missed.

### Validate: clean runs before the user sees a rule

`hydo rules check <candidates file>` runs the image's workload without faults on several
composition seeds (default 3), and evaluates every candidate after each run. A clean run is a
run of the schedule `disk=none | run W=<seed>`. The project's own checks are left out of these
runs, so that the candidates are judged alone. The command reports each rule as one of:

| status | meaning | what the skill does |
|---|---|---|
| `holds` | it held in every clean run, and judged at least one case | show it to the user |
| `vacuous` | it held, but its `cases` count was 0 in every run | do not show it; fix the workload or the rule |
| `fails-clean` | it failed in a run without faults | report it apart: the rule is wrong, or the system has a bug on its normal path |
| `unevaluated` | it could not be evaluated (exit status 2, a missing program, a timeout) | fix the rule |
| `not-run` | its kind needs the workload's records (`messages`, `history`) | write it into the project and run it |

With `--mutate`, one more clean run checks the rules that hold and have a `mutate` entry. In
that run each such rule runs, then its change is made, then it runs again and must fail. A rule
that still holds is blind to the change it was meant to catch (`blind`). One run serves all the
changes. A rule that already fails before its own change was broken by an earlier change, and
gets a run of its own.

The command reuses what exists. Each candidate becomes a final invariant of the run, added by
the host controller to the image's plan, so no image is rebuilt. The `eventually_` commands of
the workload stay and run before the candidates. Each candidate runs in a wrapper whose last line
reports the candidate's exit status and case count. The wrapper fails only when the candidate
failed, so a candidate that cannot be evaluated does not turn the run into an error. The command reads those lines from the run's
`HYDO-ASSERT` records. `--write-checks DIR` writes each rule that holds as a check file
(`final_<id>.sql` or an executable `final_<id>`), ready for `hydo/checks/`.

### Show: plain words first

The skill shows the user only the rules that hold, each with its plain statement first, then
its evidence and its confidence. The rules that failed without faults are a separate list: each
one is a wrong rule or a bug on the normal path, and the user decides which.

## What the proof runs taught

The proof ran on Blnk, Lago, and listmonk, which hydo had never seen (`docs/HISTORY.md`,
Milestone 236; the candidates files are in `docs/design/rule-suggestion/`):

- A rule that fails without faults is most often a wrong rule. Blnk's rule over the client's
  operations failed until it used Blnk's own name for a hold's commit. In listmonk the failing
  rule showed an inconsistency of the app itself.
- A rule that holds on clean runs has not been checked yet. Blnk's overdraft rule held on three
  compositions and failed on the one with concurrent draws. `--schedule` replays a known
  failure's schedule against the candidates.
- `cases` must count the rows that the rule's query judges. A Blnk bulk rule counted the bulks,
  looked at rows that Blnk never applies, and held on the failure it was meant to find.

## The clock audit (Milestone 248, tentative number)

On 2026-11-14 four bugs that hydo found had one cause: code that measured a duration with the
wall clock instead of a monotonic clock. They were in Valkey (valkey-io/valkey#5406), Temporal
(temporalio/temporal#13099), River (finding 2 of the River example), and a private queue. A step
of the clock back made a wait last longer by the size of the step, and a step forward ended a
lease early. `hydo rules scan --kind clock` finds such code and aims clock steps at it. It is
evidence of a sixth kind: it proposes schedules, not rules.

### How it reads the code

The audit works on text, without a compiler. It reads Go, C and C++, Rust, Java and Kotlin,
Python, JavaScript and TypeScript, Ruby, C#, and SQL. It skips tests, examples, benchmarks, and
vendored code. It runs in four steps:

1. It removes comments and the contents of strings from each line. SQL keeps its strings.
2. It finds the calls that read the wall clock, for example `gettimeofday`, `time(NULL)`,
   `SystemTime::now()`, `System.currentTimeMillis()`, `time.time()`, `Date.now()` and SQL's
   `now()`. Go's `time.Now()` keeps a monotonic reading, so it counts only against a value that
   lost it: `time.Now().UTC()`, `.Round(0)`, `.Unix()`, or a time read from storage.
3. It follows a reading through the code until nothing changes:
   - a function of at most 30 lines, with a name about time, that returns a reading;
   - a variable, a field or a parameter that is set to a reading.
4. It reports a line that uses a reading in one of three shapes:
   - `elapsed`: a subtraction compared with a limit;
   - `deadline`: a reading plus a duration;
   - `check`: a reading compared with a stored deadline.

Several guards keep the precision:

- A value is a reading only when the expression starts with one, so `buf = format(buf, now)`
  is not followed.
- A hop from one variable to another needs a name about time on one side, so `x.BuildId =
  y.Version` is not followed.
- A name with a second implementation that reads a monotonic clock is not followed.
- A line that only logs, prints or records a metric is dropped, and so is an overflow guard
  such as `LLONG_MAX - now`.

A place is `high` when it compares a duration with a limit or keeps a lease or a deadline, and
`medium` when only its names suggest a deadline. The limit's value comes from the code when it
is written as a constant, a `#define`, or a default in a table of settings. The unit of a bare
number comes from the name of the reading (`mstime`, `unixtime`).

### The ranking

The report shows one place per function, the 5 most likely first, and one line for the rest
(`--all` shows every place; `--json` has every place with its rank). The rank adds points:

| what | points |
|---|---|
| a limit the code states, from 50 ms to one hour | 4 (another stated limit: 1) |
| a lease or replication; a database connection or a request timeout | 5; 3 |
| a word of waiting, throttling or retrying (`refresh`, `retry`, `lag`, `timeout`) | 1 |
| a word of the function's or the file's name in the commands or environments of hydo.yaml | 3 |
| a measured time (`elapsed`) | 1 |
| a minor language of the repository (less than a fifth of its source files) | -5 |
| code that logs, reports, caches, or benchmarks | -6 |

The words of hydo.yaml leave out the project's name, the image's name and plain words such as
`server` or `port`. For Valkey the services' commands hold `replication` and `repl`, so the
functions of `replication.c` gain 3 and the cluster and Sentinel code does not. Equal ranks keep
the path order.

### The schedules

Each function with a `high` place gets three schedules:

- the clock goes back, then the fault that makes the code run;
- the fault, then the clock goes back while the system recovers;
- the clock goes forward by more than the limit.

A step back is the first of 5 minutes, 1 hour, 1 day, and 30 days that is longer than ten times
the limit. A step forward is the first of 30 seconds, 5 minutes, 1 hour, 1 day, and 30 days that
is longer than 1.5 times the limit. Without a limit both steps are 5 minutes. The fault
comes from the words of the function, the file and the line, in this order:

| words | fault |
|---|---|
| lease, leader, election, lock, heartbeat | the lease holder is killed (a group's `primary`) |
| replica, replication, follower, `repl`, `sync` | a replica is killed (a service whose command makes it a replica) |
| database, persistence, connection, pool, session | the database is killed (the one database service) |
| timeout, deadline, request, retry, `ack` | 30% of the answers of a service are lost |
| none of these | a service is killed |

The services come from `hydo/hydo.yaml`. When no service has the role, the schedule holds a
placeholder such as `<replica>`.

### What the proof runs measured

The tagged sources of the three public cases, read by the audit:

| project | source files | places | high | functions | the known line, and its rank with the example's hydo.yaml (without) |
|---|---|---|---|---|---|
| Valkey 9.1.3 | 255 | 159 | 144 | 96 | `replication.c:4962` (`refreshGoodReplicasCount`): 2 (4); `replication.c:5502` (`shouldStartChildReplication`): 4 (6) |
| Temporal 1.32.2 | 1,515 | 191 | 109 | 91 | `common/persistence/sql/sqlplugin/db_handle.go:98`, limit 1 second: 1 (3) |
| River v0.51.2 | 595 | 50 | 25 | 15 | `riverpgxv5/internal/dbsqlc/river_leader.sql` (lines 19, 27, 30, 36 and 49): 2 (2) |

Without hydo.yaml, Valkey's cluster and failover code ranks before its replication code, because
nothing tells the audit that the services run neither cluster mode nor Sentinel.

A hand review read a seeded sample: 40 places of Valkey, 40 of Temporal, and all 50 of River. It
counted a place as correct when the place measures a duration or keeps a deadline with the wall
clock:

- Valkey: 38 of 40 (2 were overflow guards, now excluded).
- Temporal: 34 of 40 (a fake test clock's `Now()`, a method named `Sub`, and calendar
  arithmetic of the scheduler).
- River: 50 of 50.

Five of the 130 places only fed a log line or a metric. The review used the audit before its last
fixes. The overflow guards and the method named `Sub` are gone from the counts in the table; the
fake test clock and the calendar arithmetic remain. Six projects that keep
their timeouts on a monotonic clock gave 8 places in all:

- hashicorp/raft v1.7.1 and bb8 0.8.5 gave none.
- urllib3 2.2.3 gave 2: a `Retry-After` date, and a test server.
- libuv v1.48.0 gave 3: the uptime from the boot time, on three systems.
- HikariCP 5.1.0 gave 2: its millisecond clock source.
- node-lru-cache v10.2.0 gave 1: a time stamp in its dump.

Each of the 8 reads the wall clock on purpose. None is a monotonic timeout that the audit took
for a wall-clock one.

For the Valkey example, the audit names `valkey3`, the last replica of `hydo.yaml`. On the Mac
engine its `back-after` schedule `disk=none | run K=valkey3@2+3 T=10:-300` failed with the
example's recorded fingerprint `d3c8c0fed4a3dbd8`. The `back-before` schedule passed, because
the replica's request is stamped after that step. The `forward-after` schedule passed too: a
step forward only shortens the 5-second wait.

### What the clock audit does not do

- It reads text, so a reading passed through an interface with several implementations, or
  through a channel or a queue, is not followed.
- The schedules place the steps at fixed workload steps (the fault before step 2, the step
  back before step 10 of a 20-step workload). Code that runs at another time needs the steps
  moved by hand.
- The private queue's code was not available, so its pattern is checked only by a unit test.

## What is not built

- The candidates file has no JSON schema yet. A change of its format raises its `format:` number.
- `rules-check.json` is not part of the results format.
- `messages` and `history` candidates are not validated by `hydo rules check`.
- The mutation is a change of the stored state after the run. A change of the workload's
  program is not built.
