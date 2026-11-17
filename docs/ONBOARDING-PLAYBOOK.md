# The onboarding playbook

This page is the process that every onboarding of a real project follows. An onboarding puts an
outside project under hydo, explores it with faults, judges what breaks, and reports new bugs to
the project. A maintainer of hydo or a coding agent follows the page from top to bottom.

The page comes from about 15 onboardings, Milestones 215 to 275 of `docs/HISTORY.md`. Each
example's `RESULTS.md` under `hydo/onboard/examples/` has the evidence. The skill
`plugin/skills/onboard/SKILL.md` writes the project's `hydo/` folder, and `hydo guide` holds the
facts of the installed hydo. This page adds what comes before and after the folder: the choice of
a target, the time limits, the judgment of a finding, and the report.

## 1. Picking a target

Pick a project that meets all of these criteria:

- **Issues on GitHub.** The project takes bug reports as GitHub issues, so a report needs no new
  account and `gh` can search and post.
- **A simple setup.** A Docker Compose file and released images, or one released binary. Hatchet,
  River, Valkey, and Temporal started from a Compose file with `hydo onboard`.
- **A promise about state.** The project promises something that a fault can break: acknowledged
  data stays, a job runs once, a payment is recorded once.
- **Released hydo is enough.** The reproduction must run with the published hydo. A finding that
  needs a hydo change first cannot go into a report that a maintainer can replay.

Treat these as red flags, and pick another target unless the user asks for this one:

- **A tracker that needs an account.** Mender keeps its tracker in Jira, and SWUpdate has GitHub
  issues switched off and takes reports on a mailing list. The candidates of those two examples
  were not posted.
- **A device-style setup.** RAUC, SWUpdate and Mender each needed their own disk images, a boot
  loader model, and hydo features that did not exist yet (Milestones 259, 263 and 272).
- **Memory over the guest's limit.** All services share one guest of at most 8 GiB.
  TigerBeetle allocates 2.1 GiB per replica, so three replicas did not fit before Milestone 179.
- **Kernel features in the guest.** k3s needed 57 kernel modules on hvx and does not start on
  the Mac engine (Milestones 246 and 256).
- **Behaviour that is known or documented.** A promise that the documentation already limits
  gives no report. Formance's backdated balance below zero and Lago's lost in-advance fees after
  a power cut were both documented.

The reporting channel is the project's GitHub issues. Use the project's security policy
(`SECURITY.md`) for a bug that an attacker can trigger, and do not post such a bug publicly.

## 2. The promises of each type of system

Every onboarding writes one generic rule and a few domain rules. The generic rule compares the
system's final state with what the client's acknowledged operations imply. A domain rule states
one promise of the system in its own terms.

The generic rule found more than any other rule. In Lago it failed in all 11 runs that broke a
business result, and the billing rules failed in 8 of them. In Formance and Blnk it was the only
rule that saw an acknowledged operation lost, because a lost transaction leaves a valid ledger.
Write it first, and write the domain rules beside it.

### Queues and job runners

The systems were River, Hatchet, NATS JetStream, Temporal, and Saleor's Celery tasks. Use the
messages check (`type: messages`, `hydo guide messages`) for the common promises. Add rules over
the system's own tables for the rest.

| promise | rule shape that worked | found |
|---|---|---|
| no acknowledged job or message is lost | the messages check's `no-loss`, with a drain that outlasts every lease | Hatchet's lost `ASSIGNED` messages ([hatchet#5527](https://github.com/hatchet-dev/hatchet/issues/5527)) |
| one periodic run per tick | a `.sql` check that groups the jobs by their scheduled time, not by the insert time | River's two leaders ([river#1602](https://github.com/riverqueue/river/issues/1602)) |
| an acknowledged request reaches its handler | a rule that every requested action ends as done or as failed | Saleor's lost refund request ([saleor#20022](https://github.com/saleor/saleor/issues/20022)) |
| no job from a rolled-back transaction runs | a `.sql` check over the jobs and the client's records | nothing broke it |
| a cancelled run does not complete | a `.sql` check over the runs' final states | Hatchet's cancelled run that became `COMPLETED` (known, issue #2655) |

Three lessons came from the queues:

- **Write the drain first.** The drain is the `final:` command that reads every message back. It
  must outlast every lease and the recovery after a power cut, inside `workload.timeout`
  (`hydo guide messages`, "Write the drain").
- **Keep the evidence.** River's cleaner deleted completed jobs after a forward clock step of a
  year, so the rules saw nothing. Turn off the retention of finished jobs in the workload.
- **Expect a crashed consumer to lose work.** Hatchet, Saleor, and Lago each lost acknowledged
  work when the process that held it died. Aim a kill at the consumer between the
  acknowledgement and the completion.

### Ledgers and billing

The systems were Formance Ledger, Blnk, Lago, and Saleor's payments. Write the domain rules as
`.sql` checks, each a query that must return no rows.

| promise | rule shape that worked | found |
|---|---|---|
| the business result matches the acknowledged operations | the generic rule: totals per account from the client's records against the system's totals | Lago's double invoice ([lago-api#7164](https://github.com/getlago/lago-api/issues/7164)), Blnk's lost commits of holds |
| an overdraft limit holds | a `.sql` check of each balance against its limit | Blnk's coalesced draws |
| an atomic bulk applies whole or not at all | a `.sql` check that each bulk has all or none of its transfers | Blnk's half-applied bulk |
| every acknowledged event is charged, every period is invoiced | an `eventually_` check that reads the money | Lago's cancellation during a Redis outage (known, lago-api#6901) |
| a balance equals its ledger | a `.sql` check of the stored balance against the sum of its entries | Saleor's gift card charge ([saleor#20023](https://github.com/saleor/saleor/issues/20023)) |
| the provider charged each payment once | `hydo-world check at-most-once <route> --by ref` over the fake's record | nothing broke it |

Two lessons came from the ledgers:

- **Rules inside one transaction rarely fail.** Formance enforces its rules inside one database
  transaction, and 7 of its 8 domain rules never failed. Blnk enforces its limits across the
  API, Redis, and a worker, and its domain rules found both of its bugs.
- **Turn on error logs.** Formance's only bug, a `500` on two first uses of one account, showed
  in the error logs alone (`errors: true`, `hydo guide error-logs`). The client retried, so every
  rule held.

### Key-value stores and databases

The systems were Valkey, etcd, TigerBeetle, and NATS JetStream as a store. For a SQL database,
run `hydo sql-gen` first (`hydo guide workloads`): it writes the workloads and the checks.

| promise | rule shape that worked | found |
|---|---|---|
| an acknowledged write survives a power cut | the client's acknowledged writes read back after the faults | NATS's lost messages with the default `sync_interval` (Jepsen's finding 3.4) |
| replicas converge with the primary | the replicas' contents compared after the heal | Valkey's delayed full sync ([valkey#5406](https://github.com/valkey-io/valkey/issues/5406)) |
| a key expires as set | the key's expiry read after a restart | Valkey's lost expiry after an AOF replay (known, valkey#5354) |
| the stream survives one node's storage faults | the stream read back after `G=` and `L=` | NATS findings 3.2, 3.3 and 3.5 |

Make each durability setting a parameter of the build, and run the durable value. A loss with
`appendfsync everysec` or `synchronous_commit=off` is the setting's documented cost.

### Update tools and devices

The systems were RAUC, SWUpdate and Mender, on a device model with two root slots and U-Boot's
environment. The device needs `guest.reboot: true` and, on the Mac and KVM engines,
`guest.disks` (`docs/design/devices.md`).

| promise | rule shape that worked | found |
|---|---|---|
| the device boots a complete slot of an allowed version | the booted version checked after every boot | SWUpdate's slot without a flush |
| an update can be committed or rolled back after a power cut | the update's state read after the recovery boot | Mender's empty `version` file |
| the commit command's exit code tells the truth | the exit code compared with the slot that boots next | Mender's rollback that exits 0 |

Aim the power cuts at the update's own events with reactions:
`--reactive fault --react-events ota.install_started,ota.reboot_started`. A window between an
install and the reboot exists only when the client waits between them. SWUpdate's first client
rebooted at once, and 25 schedules found nothing.

### Web apps

The systems were the web-shop example and Memos. Run the team's Playwright tests with `browser:`,
or let hydo walk through the pages with `browser.explore` (`hydo guide web-apps`). The five
invariants judge uncaught errors, spinners that stay, failed requests the page does not show, and
broken texts. Add one page rule of the project's own, such as the rule that every memo that Memos
answered as saved is in its database.

Narrow `watch.failures_of` to the writes. Background reads that fail under faults made every run
fail in Memos's first exploration. Name the busy parts of the app in `avoid:`, or the walk spends
its actions there: 216 actions of Memos's first walks filled the editor without saving it.

## 3. Which faults find what

Choose a target's checks and faults with `hydo guide strategies`. It lists each technique that
found a bug, with what to write, when it fits, what it found and missed, and its cost. Its first
table maps a type of system and its promise to the techniques that found most.

The table below lists the findings so far by the fault that made them. "None" means that the bug
showed without a fault.

| fault | token | findings |
|---|---|---|
| power cut at a step | `C=` | Valkey's lost expiry; SWUpdate's slot; Mender's empty `version` file; NATS 3.1 and 3.4 |
| power cut at the k-th disk flush (hvx only) | `cf=` | Saleor's gift card charge (a cut at a flush, after a clock step) |
| power cut at an event | `A=<event>:cut` | Mender's commit that rolls back and exits 0 |
| clock step | `T=` | Valkey#5406; Temporal's server down for the length of the step ([temporal#13099](https://github.com/temporalio/temporal/issues/13099)); River's clock effects; Blnk's rejected transfers; Restate's invocations held after a restart (with `L=` or `K=`) |
| clock step to a time of day | `T=<at>:@<point>` | Lago's double invoice, with one-way loss of the database's answers |
| kill | `K=` | Hatchet#5527 (an engine); River#1602 (PostgreSQL); Lago#6901 and Blnk's lost commits (Redis) |
| kill with a delayed answer | `K=` and `H=...:delay=` | Saleor#20022 (the worker, while the payment app is slow) |
| pause of the workload or of one thread | `P=workload`, `P=workload/<thread>` | etcd's flaky test 22796; Qdrant's flaky test 10200 |
| protocol faults on a fake | `H=` | web-shop's flaky test (`delay`), its double order (`drop`), its stuck spinner (`status=500`) |
| storage faults of one service | `G=`, `L=`, `L=...:strict` | NATS 3.2 (two nodes' corrupted blocks), 3.3 (a zeroed snapshot), 3.5 (one node's power loss) |
| none | | Blnk's overdraft limit (concurrent draws); Formance's account race; Memos's share link over plain HTTP ([memos#6575](https://github.com/usememos/memos/issues/6575)) |

What the table shows:

- **Clock steps gave the most new reports.** Valkey, Temporal, and River each measured a duration
  on the wall clock. Run the clock audit, `hydo rules scan --kind clock`, before the exploration,
  and keep `clock` in `faults:` (`hydo guide faults`).
- **No finding needed a pause of a service alone.** In Hatchet a pause of an engine for 15 s
  broke nothing, and a kill at the same step lost acknowledged runs. The pauses that failed a test
  by themselves were aimed at the workload or at one of its threads.
- **Disk errors and lying disks break the disk model.** In Blnk, Formance, Valkey and Lago, every
  failure with `disk=ioerr` or `disk=lying` was the disk's loss or a database that refused to
  start. Run `--durable-only` once the first exploration shows such failures.
- **Several faults in one schedule are common.** Saleor's refund needs a kill and a delayed
  answer, and each fault alone passes. Separate them (section 5).

## 4. The phases

An onboarding has five phases, each with a time limit. Two checkpoints decide whether the
onboarding goes on. The limits come from the onboardings so far. These took 2 to 7 hours of wall
time and roughly 0.4 to 1 million agent tokens each. Half or more of the wall time was machine
time: image builds and explorations. The tokens went to fights with the setup and to the
judgment of findings.

| phase | time limit | VMs |
|---|---|---|
| 1. Scouting | 45 minutes | none |
| 2. Setup to a first clean run | 3 hours | one at a time |
| 3. Exploration and shrinking | 4 to 5 hours | the explorer's workers |
| 4. Judging the findings | inside phase 3 | directed runs |
| 5. Duplicate search and write-up | 1 hour | none |

### Phase 1: scouting

1. Read the project's README, its Compose file, and its documentation of guarantees.
2. Write down the promises that a fault can break, in the project's own words.
3. Check the criteria and the red flags of section 1.
4. Read the project's open issues about crashes, restarts, clocks, and retries. They show where
   the maintainers expect trouble.
5. Run `hydo rules scan` on a checkout of the project. It lists the schema, the state columns,
   the API and the documented promises with file and line.

**Checkpoint after scouting.** Go on when the project meets every criterion and has at least one
promise that a fault can break. Otherwise stop, and write one paragraph with the reason.

### Phase 2: setup to a first clean run

1. Run `hydo onboard` in the project's folder. With several Compose files, name one with
   `hydo onboard --compose FILE`.
2. Run `sh hydo/fetch-images.sh`, which copies the images in once.
3. Write the folder with the skill (`plugin/skills/onboard/SKILL.md`, step 3).
4. Run `hydo build hydo/hydo.yaml --check` after every file, until it prints `OK`.
5. Build the image with `hydo build hydo/hydo.yaml`.
6. Run `hydo run "disk=none | run"` twice. Both runs must pass with the same fingerprint (`fp=`).

When the clean run fails, read the `hydo: diagnosis:` lines and the services' logs in
`<run>/logs/` first. Then look inside the run with `hydo inspect` (`hydo guide inspect`). Check
section 10 for the common causes before you change the folder.

**Checkpoint after setup.** Go on when the clean run passes twice with one fingerprint within 3
hours. Otherwise stop. Record each blocker as a gap of hydo in the example's `RESULTS.md`.

### Phase 3: exploration and shrinking

1. Validate the proposed rules on clean runs:

   ```sh
   hydo rules check hydo/rules.yaml --mutate
   ```

2. Run the clock audit and the schedules of its first three places:

   ```sh
   hydo rules scan --kind clock
   hydo explore --schedule "<schedule from the audit>"
   ```

3. Explore with the process strategy and the new fault kinds:

   ```sh
   hydo explore --seeds 1-20 --strategy process --new-faults
   ```

4. Read the problems with `hydo report`, and the minimal schedules with `hydo report --details`.
5. Fix each failure that is a mistake of the rules, rebuild, and explore the next seeds.
6. Add the options that fit the project. `hydo guide strategies` says which ones found bugs in
   systems of the same type:
   - `--cluster-faults` for the services of `groups:`;
   - `--more-faults` on hvx;
   - `--storage-faults` for services with `storage:`;
   - `--protocol-faults` for fakes and emulators.
7. Run a swarm exploration last, with `--strategy swarm --new-faults`.

Run the blind explorations on GitHub's KVM runners when the example builds an x86_64 image on
Linux. Keep this Mac and the bare-metal machine for the directed runs, such as `hydo explore
--schedule`, the shrinks, and the native reproductions. Push the branch, then start the workflow
`explore.yml` with the same options (section 9):

```sh
sh ci/remote-explore.sh --example hydo/onboard/examples/<name> --manifest hydo/hydo.yaml \
  --ref <branch> --seeds 1-60 --shards 20 --explore "--strategy process --new-faults" \
  --prepare "sh hydo/fetch-images.sh x86_64"
```

The first failures are usually the rules' own mistakes. In River five kinds of failure were
mistakes of the rules, and each of the 10 failures of Valkey's first exploration broke a rule that
was too strict. TigerBeetle's check read an empty `$HYDO_OUTCOMES` in the recovery boot and
reported every acknowledged write lost, which cost an hour. Read a failure's cause before you
call it a finding.

Stop an exploration when its failures are understood. The shrinker spends hours on clock steps
and on disk faults: in Saleor each shrink candidate took four minutes. Use `--no-shrink` or
`--time-limit` when the machine is shared.

### Phase 4: judging the findings

Judge each failure with section 5, during the exploration. Run directed schedules on the image
for each candidate: the minimal schedule, each fault alone, and the neighbours of the failing
step.

### Phase 5: duplicate search and write-up

Search for duplicates (section 5), write `RESULTS.md` and the milestone (section 6), and draft
the upstream report (section 7).

## 5. Judging a finding

### Is it a bug

Classify each failure with one of these labels. The examples' `RESULTS.md` files use the same
labels in their finding headings.

- **A bug of the project, new as far as the search found.** The project breaks a promise that its
  code or documentation makes, and no issue names the cause.
- **A known bug.** An issue, a pull request or a discussion names the same cause. Give its number.
- **Documented or by design.** The documentation limits the promise, or the code does it on
  purpose. Temporal delays every timer after a backward clock step on purpose.
- **A deployment setting.** The failure needs a setting that is not the durable one. Blnk's lost
  commits need Redis without an append-only file.
- **A clock effect.** The system measures time on the wall clock, and the failure follows from a
  step. Report it only when it breaks a promise, as Valkey's and Temporal's did.
- **A client mistake.** The workload did something that the documentation forbids. Formance's
  client retried without idempotency keys.
- **The environment.** The fault broke hydo's disk model or the guest. A lying disk that loses
  synced data is the disk's loss.
- **A mistake of the rules.** The rule is wrong. Fix it and run the schedule again.

### The duplicate search

The duplicate search is required for every candidate that is not a mistake of the rules. Search
the project's issues, pull requests and discussions, open and closed:

```sh
gh search issues --repo <owner>/<name> --include-prs "<phrase>"
gh api graphql -f query='{ search(type: DISCUSSION, query: "repo:<owner>/<name> <phrase>", first: 20) { nodes { ... on Discussion { number title url } } } }'
```

Use 10 to 30 phrases: the names of the functions in the cause, the error text, the symptom in a
user's words, and the setting's name. Also read the CHANGELOG since the tested version, and the
history of the files of the cause on the default branch. Record every phrase and what it found in
a table of `RESULTS.md`. Saleor's section "The duplicate searches" is the model.

### Separating the faults

Separate the faults of every failing schedule. Run the minimal schedule with one fault removed at
a time, one run each. A finding needs every fault that remains. Saleor's refund needs both the
kill of the worker and the delayed refund answer, and each alone passes. Hatchet's seed 2 was not
separated, so its cause stays open.

### Native reproduction

A reproduction without hydo, in the project's own Docker setup, is conditional:

- **Skip it when the cause is plain in a few lines of code.** SWUpdate's missing flush and
  Mender's empty file were judged from the code and hydo's replay alone.
- **Limit it to about an hour otherwise.** Use it when the cause is a timing or environment
  effect that the code does not show, as in Hatchet's queue and River's leader table.
- **Never do it for known or documented behaviour.**

Keep the scripts of a native reproduction in the example's `native/` folder.

### Versions

Run `hydo versions` to say in which releases the finding is present (`hydo guide workflow`,
"Which versions have a finding"). Formance's account race is present from v2.4.0 and absent in
v2.3.24, and a report that says so saves the maintainers a bisection.

## 6. The write-up

### The sections of the results page

Write `hydo/onboard/examples/<project>/RESULTS.md` with these sections, in this order. The
Saleor and Hatchet examples follow it.

1. **The short answers.** What was found, with each finding's label and issue link. What held.
   What hydo lacked, as a pointer.
2. **The version.** The project's version, commit, and image digests.
3. **The project.** Its services, and what changed from the import.
4. **The workload.** The client's operations and what it records.
5. **The rules.** Each rule, its promise, and its file.
6. **Without faults, and the regressions.** The clean run's fingerprint and the saved schedules.
7. **What was found.** One subsection per finding: the schedule, the cause in the code, the
   duplicate search's result, the native run or why there is none, and the regression file.
8. **The duplicate searches.** The table of phrases and results.
9. **The explorations.** A table: the options, the seeds, the schedules run, and the failures
   by cause.
10. **What hydo lacked.** Each gap in the order of the time it cost, with what was done instead.
11. **Time.** The wall time of each phase.

### The regressions

Save each finding's minimal schedule with `hydo save <explore dir>`. Name the file after the
finding, for example `regressions/stale-slot-after-switch.schedule`. Check that
`hydo repro --regressions` reproduces every saved schedule.

A saved regression depends on the image's bytes. A change of the example's files can turn a
timing-sensitive regression into a pass (Lago's `cancel-before-billing-tick`, open item 100).

### The milestone and the open items

1. Add a milestone section to the end of `docs/HISTORY.md`, with the words `(tentative number)`.
2. Add its one-line entry to the milestone index of `AGENTS.md`.
3. Add a paragraph to "Current state" in `AGENTS.md`.
4. Add one open item that lists what hydo lacked and what stays open.

Recorded fingerprints are re-recorded in batches, not per change. A change that moves recorded
values names them in its milestone and in open item 105. Before a merge, run these checks:

```sh
cargo test -p hydo --test it equivalence::
cargo test -p hydo-schedule --test it language::
cargo test -p hydo-config
cargo clippy --workspace --all-targets -- -D warnings
```

A new folder under `hydo/onboard/examples/` enters the explorer's and the controller's test
corpora. Re-record those goldens with `HYDO_BLESS=1`. The onboardings of NATS and TigerBeetle did
the same.

## 7. The upstream report

A report is public and permanent. Post it only after the user approves its text.

### The format

1. **The bug in one line.** Write the title and the first line as the user meets the bug.
2. **How it was found.** Open the section with this Markdown, unchanged, as one paragraph:

   ```text
   Found autonomously by [hydo](https://github.com/kakoc/hydo), an open-source tool that runs the whole system in a VM where every run repeats exactly. It looks for bugs on its own and can replay anything it finds.
   ```

   Then say which faults the exploration drew and which rule broke.
3. **Steps to reproduce with hydo.** Give the commands that build the image and replay the
   minimal schedule, with the verdict they print.
4. **The consequence.** One or two sentences on what a user loses.
5. **The cause.** Numbered steps through the code. Name the file and the line of each step on the
   default branch.

Do not add a closing paragraph about hydo, a fix, a patch, or a closing question. One maintainer
answered a report that the project's tracker is not a place for advertising
([saleor#20022](https://github.com/saleor/saleor/issues/20022)). The fixed sentence is the whole
of hydo in a report.

### Where to post

Post in the project's GitHub issues with `gh issue create --repo <owner>/<name> --body-file -`.
For a known bug with a new detail, comment on the existing issue instead, as on lago-api#6901.

### Attachments

`gh` cannot attach files. Drag a video or an image into the issue's comment box in the browser by
hand. For a web app, the recording replay of `hydo explore` writes the video under
`replays/rec-<k>/` (`hydo guide web-apps`). Memos's report
([memos#6575](https://github.com/usememos/memos/issues/6575)) has such a video.

### Following the threads

Check each posted thread for answers at the start of every session:

```sh
gh issue view <number> --repo <owner>/<name> --comments
```

Answer a maintainer's question in the next session. Draft the answer, and post it after the user
approves it. Record a maintainer's decision in the example's `RESULTS.md`. River's maintainers
closed river#1602 as not planned and keep the leader table unlogged by design. The finding's
heading says so.

## 8. The retro after each onboarding

Write the retro as the last part of the milestone in `docs/HISTORY.md`, in this form:

```text
Retro: <project> <version>, <engine>, <date>
- Time to the first clean run: <hours> of wall time, <hours> of it waiting for machines.
- Schedules run before the first finding: <n>, of which <n> failed by the rules' mistakes.
- The findings: <n> new, <n> known, <n> by design or documented, each with its label.
- What took the most time: <the three largest costs, each with its hours>.
- What hydo lacked: <each gap>, and for each the fix: an open item, a guide text, or code.
- Strategies: <each technique that found or missed a finding>, and the entry of hydo guide
  strategies that was added or updated.
```

Add the onboarding's evidence to `hydo/guide/strategies.md`, the text of `hydo guide
strategies`. Update the entry of each technique that found a finding or missed one, with the
example's name, the section of its `RESULTS.md`, and the numbers. Add an entry for a new
technique in the same shape. The next onboarding chooses its techniques from that page.

Turn each gap into one of three fixes:

- **Code.** A gap that blocked the onboarding becomes a milestone of its own with a test.
  Milestone 222 fixed four gaps of Formance, and Milestone 225 fixed six gaps of Lago.
- **Guide text.** A gap that a reader can avoid becomes text in `hydo/guide/`, as Milestone 240
  added "Write the drain" to `hydo guide messages`.
- **An open item.** Every other gap goes into the onboarding's open item in `AGENTS.md`.

Some reference values for the retro:

| project | time to the first clean run | schedules before the first finding |
|---|---|---|
| Formance Ledger | not recorded; the whole onboarding took 1.5 hours of machine time | 70 without a failure; the bug came next, as a new error kind |
| River | 40 minutes, the first run passed | 31, after two explorations of the rules' mistakes |
| Hatchet | 1.5 hours | 5 directed kills of 12 failed; blind, seed 2 of 3 |
| Saleor | not recorded; the first `first_` command failed | 1, the first seed |
| TigerBeetle | 85 minutes, 59 of them waiting for a quiet machine | none real; the first violation was the check's mistake |
| Mender, SWUpdate | not recorded | 1, the first seed |
| Memos | not recorded | one failure in seeds 21 to 60; seeds 1 to 20 judged no write |

## 9. Throughput

The engines differ in speed, memory, and fault kinds. Pick the engine by what the finding needs,
then by speed.

| engine | where | measured speed | limits |
|---|---|---|---|
| hvx | the bare-metal machine, shared | 231 runs an hour for SWUpdate; 38 to 43 s a run for Memos; 2 to 5 minutes a run for Hatchet at a load of 12 to 28 | the only engine with `cf=`, `--more-faults` and the PMU branch clock; one disk |
| the Mac engine | an Apple Silicon Mac | 1,400 runs an hour for Valkey with three workers; 128 runs an hour for Mender; 4.8 s a schedule for Formance | arm64 guests; image roots live in guest RAM; a 1 GiB worker takes about 1.8 GB |
| KVM | GitHub's runners, up to 20 at once (`explore.yml`) | Dragonfly's 60 schedules in 31 minutes on 20 runners, 75 on one; Redpanda's 40 in 2 h 53 min on 20 | 4 CPUs and 15 GB per runner; Intel and AMD give different fingerprints, the same verdicts |

Three facts set the wall time:

- **Memory decides the number of workers.** On the Mac six 1 GiB workers beside other sessions
  filled the swap (NATS). On the bare-metal machine the host's out-of-memory killer ended
  `hydo-hvx` in Saleor's rule check while another session held 9 GB.
- **A rebuild costs minutes.** Every change of a workload file or a check means a new image. On
  the loaded bare-metal machine, each Saleor build spent about 25 minutes on its files' owners.
- **Shrinking costs the most.** Lago's first exploration ran 20 schedules, 12 re-runs and 306
  shrink runs in 66 minutes.

To shard an exploration, run `hydo explore --shard I/N` on N machines of one engine. Then
`hydo explore --merge` joins the parts without a VM (`hydo guide ci`, "Explore on many
runners"). The workflow `explore.yml` does both on up to 20 of GitHub's runners, and
`ci/remote-explore.sh` starts it and downloads the merged folder. Parts of two engines, such as
hvx and the Mac, do not merge, because their images and options differ. In a team's CI, `hydo ci
run --shard I/N` and `hydo ci merge` do the same for a CI run.

## 10. Common setup problems

These problems came from the examples' sections on what hydo lacked. Each row says how it showed
and the fix that worked.

| problem | how it showed | fix | example |
|---|---|---|---|
| a program writes into the image's read-only root | Celery's beat stopped at start with `Read-only file system`, and the service counted as ready | point the file at `/run` or the data disk, or set `writable: true` on the image service | Saleor, Medusa |
| the import misses links through a one-shot job or a service without `depends_on` | the explorer drew no network fault between a worker and its database | name the dependencies in the service's `depends_on` by hand | Blnk, River, Hatchet |
| the import takes a port from the image's `EXPOSE` | a readiness check that never passes, and network faults aimed at a port without traffic | set the real port and a readiness check such as `celery inspect ping` | Saleor, Hatchet |
| a Compose healthcheck passes too early | PostgreSQL's `pg_isready` passed during the image's temporary init server | ask `pg_isready -h 127.0.0.1` | River |
| the guest's `/proc/cpuinfo` has no `cpu MHz` line on KVM | the kubelet stopped with the message `could not detect clock speed` | bind a copy with `cpu MHz : 2000.000` over the file | k3s |
| a build of the project's files differs from build to build | other image bytes, so other fingerprints for one schedule | fix the file times and the orders in the build, with `SOURCE_DATE_EPOCH` and sorted inputs | Mender's state scripts, SWUpdate's bundles |
| a Docker build differs from build to build | `fetch-images.sh` stopped at another digest | review the new image, then accept it with `HYDO_ACCEPT_REBUILD=1` | Docker 28.4's `EXPOSE` history (open item 81) |
| the guest's hostname is not in `/etc/hosts` | Temporal's entrypoint exited with status 1 and no output | set `TEMPORAL_BROADCAST_ADDRESS` | Temporal |
| `network_mode: host` with peers named `0.0.0.0:<port>` | every replica dialled itself | change the peers to the services' `addr:`, or import with `--network loopback` | TigerBeetle |
| the import's guest RAM is too small | services died during provisioning | raise `guest.ram_mib`; Saleor needed 3,072 MiB, Temporal 2,048 MiB | Saleor, Temporal |
| a one-shot job runs in the `setup:` of several services | the migration ran three times at once | make the other services wait for the first | Hatchet |
| error kinds split by an id in the log line | every request became its own kind, and the explorer shrank each one | add `normalize:` to the service, or group the kinds with regular expressions in `known-errors.yaml` | Formance, Blnk |
| a check prints changing ids in its last line | each count of lost ids became its own signature | print the ids on an earlier line | NATS |
| busybox tools differ from GNU tools | `grep -v -x -F -f` with an empty file matched every line, so a lost read passed | use `awk` in checks | NATS |
| `$HYDO_OUTCOMES` is empty in the recovery boot | every power cut looked like the loss of every acknowledged write | read `$HYDO_ACKED`, and read the store in full | TigerBeetle |
