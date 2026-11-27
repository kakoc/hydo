# Proposing rules from the project's own sources

The best rules are the ones the project already promises but nobody wrote a test for. Find them
in the project's sources, validate them with hydo, and show the owner only the rules that passed.
Do not start from a list of rules for the domain: the lists in `hydo guide workloads` are a
check at the end, not the source.

The format of the candidates file, the statuses and the commands are in `hydo guide rules`. Read
it first.

## 1. Collect the evidence

1. Run `hydo rules scan` in the repository (`hydo rules scan --json` for all of it). It lists
   schema constraints and state columns, API contracts, promise words in the documentation, and
   test assertions, and each with its file and line.
2. Read the files the scan points at, and also:
   - the database schema itself (a schema dump, the migrations, the model classes);
   - the API description (OpenAPI, `.proto`, the routes) for idempotency keys, conflict answers
     and limits;
   - the public documentation (the repository's `docs/`, or the project's documentation site
     when the user allows you to read it) for the sentences that state a guarantee;
   - the project's own tests, for what they assert after an operation;
   - the client library, for what a caller may rely on (retries, keys, error meanings).
3. Read the workload you wrote: a rule can judge only what the workload makes happen.

## 2. Write the candidates

Write a rule where the default checks cannot see the promise. Such promises are at most once,
once per period, a money limit, a device's version, a listing, an expiry, and a retention. The
read-back of the acknowledged writes already covers the promise that an acknowledged write stays
(`hydo guide strategies`, section "Which check noticed each bug").

For each promise, write one rule into `hydo/rules.yaml` (`hydo guide rules`, "The candidates
file"):

- `says` in plain words that the owner understands without the schema.
- `evidence` with the file, the line, and the quoted text. A rule without evidence is a guess:
  give it `confidence: low`.
- A `cases` count for every rule, so that a rule over nothing shows as `vacuous`. Count the rows
  that the rule's query looks at (with the same names and states), not the operations the rule
  is about.
- A `mutate` change where one is easy to write (delete one row, change one amount), so that
  `--mutate` shows that the rule catches what it is meant to catch.
- `within` for a rule about what the system does after the workload (an invoice, a settled
  transfer, a delivered message).

Always add the rule over the client's acknowledged operations (`hydo guide rules`, "Turn evidence
into rules"), both halves: what was acknowledged is there once, and what was told it failed is
not. For a group that the system applies whole or not at all (a bulk, an order with its items),
add a rule that counts the group's applied members: all or none. Prefer the rules that `hydo guide rules` names as strong: constraints only the
app enforces, partial unique indexes taken to all rows, totals across tables, states that
must lead to an action. Do not repeat a database constraint as a rule.

## 3. Validate them with hydo

After the image builds and a clean run passes (SKILL.md step 4), run
`hydo rules check hydo/rules.yaml --mutate`. Then:

- `holds`: keep the rule. If its change was not caught (`blind`), make the rule or the change
  sharper, or say so to the owner.
- `vacuous`: the workload never reaches what the rule judges. Extend the workload when the rule
  matters, else drop the rule.
- `fails-clean`: the rule failed without faults. Read its rows in `rules-check.json`. Either the
  rule is wrong (fix it and validate again), or the system has a bug on its normal path. Never
  turn such a rule into a check without the owner's decision.
- `unevaluated`: fix the command and validate again.

Then review each rule that holds on two points before the first exploration, because business
rules made most of the false findings so far (`hydo guide strategies`, "Which check noticed each
bug"):

- **The wait.** A rule that waits for the system must wait out the system's own recovery: its
  leases and retry timers, a failover, and the recovery boot after a power cut.
- **The clock.** Under a clock step the rule must expect what the system promises, not what the
  wall clock's new time implies.

Fix a rule that fails either point before you explore.

A rule that holds on the clean runs is ready for exploring, not proven: Blnk's overdraft rule held
on three clean compositions and failed on the one with concurrent draws. When the project has
saved regressions or known failures, also run `hydo rules check hydo/rules.yaml --schedule
"<their schedule>"`: a rule that fails there catches that failure.

If the installed hydo has no `hydo rules` (`hydo rules --help` fails), say so, show the
candidates as unvalidated, and tell the user that a newer hydo validates them.

## 4. Show the owner

Show the rules that hold, one per line, in this order: the plain statement, its evidence (file
and line, and the quoted words), its confidence, and its case counts. Then, apart, each rule that
failed without faults, with the rows it printed, and ask the owner: a wrong rule, or a bug? Ask
the owner to confirm, reject, or add to the rules. Then compare them with the domain's list in
`hydo guide workloads`, "Business rules: ask the owner", and ask about any rule of the list that
the project's sources did not give.

Remove the rules the owner rejected from `hydo/rules.yaml`. Then write the confirmed rules as
checks with `hydo rules check hydo/rules.yaml --write-checks hydo/checks`, and rebuild the
image.
