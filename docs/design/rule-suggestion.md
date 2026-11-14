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

## What is not built

- The candidates file has no JSON schema yet. A change of its format raises its `format:` number.
- `rules-check.json` is not part of the results format.
- `messages` and `history` candidates are not validated by `hydo rules check`.
- The mutation is a change of the stored state after the run. A change of the workload's
  program is not built.
