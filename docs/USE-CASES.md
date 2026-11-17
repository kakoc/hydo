# Use cases: what hydo serves and what it could serve

This page lists every use case that hydo serves today, and every idea for one that it does not
serve yet. Read it to see all of hydo's capabilities in one place, and to choose what to build
next. It is a reference: each row names a command or a key, and where its proof is.

hydo runs a whole system in one virtual machine (VM): the services, their databases, and a client
that uses them. An image is the built guest of one project. A schedule is one line of text that
names the faults of one run, for example `disk=none | run K=redis@11+1 C=24`. The same image and
the same schedule give a bit-identical run. A finding is a run that broke a check, shrunk to the
fewest faults that still break it.

Each row has one of these status words:

- `built`: the capability is in `main`, and a recorded run proves it. The row names the run.
- `partly built`: the capability is in `main`, with gaps that an open item or a milestone names.
- `idea`: the capability is not built. The section [The ideas](#the-ideas) describes it.

A row says "not hydo's job" when the technique needs something that a VM cannot give. Examples
are production traffic and real users. Every status comes from `AGENTS.md`, a milestone of
`docs/HISTORY.md`, or an example's results page (`hydo/onboard/examples/<name>/RESULTS.md`).
"M238" is short for Milestone 238 of `docs/HISTORY.md`, and "open item 13" is item 13 of the list
"Open items" in `AGENTS.md`.

## The map against the testing stages

A common chart of testing sorts the techniques into four stages: before production, at deploy,
at release, and after release. The tables below take the chart's techniques in its order.

### Before production

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Unit tests | `workload.kind: test` runs a team's own test program in the VM as one step, with faults only when `faults:` lists them. | built | M205; the examples `nats-flaky-8363`, `etcd-flaky-22796`, and `qdrant-flaky-10200` |
| Functional tests | Commands in `hydo/workload/` use the system, and checks in `hydo/checks/` say what must hold after each run. | built | Every example; `docs/ONBOARD.md` |
| Component tests | One service runs against fakes of its outside services (`fakes:`), ready-made emulators (`emulators:` for Stripe, SMTP, and S3), or a fake from an OpenAPI description (`hydo fake from-openapi`). | built | M89, M242 (`checkout`), M249 (`openapi-payouts`) |
| Fuzz tests | `hydo fuzz` calls a Rust function with generated inputs natively. `hydo fuzz --vm` makes the calls inside the VM under power cuts and disk errors, and under clock steps, pauses, and kills inside a call. `hydo explore` draws the faults of each run from a seed. | partly built | M275, M282, M289, M292, M296 (`fuzz-vm`); open item 139 |
| Property-based tests | Function contracts (`// hydo: requires` and `ensures`) judge each generated input of `hydo fuzz`, and each call in a whole-system run. Value properties (`// hydo: keep`) judge a recorded value along its history. | partly built | M241, M255, M291 (`blnk`); open items 113 and 146 |
| Formal verification | Not hydo's job. A proof covers every state of a model, and hydo checks only the runs that it makes. | not hydo's job | |
| Static analysis | `hydo rules scan` lists a project's promises from its sources. `hydo rules scan --kind clock` lists the code that measures a duration with the wall clock. Both read text, not syntax. | partly built | M236, M248; open items 110, 117, and 122 |
| Mutation tests | `hydo rules check --mutate` plants a change of the stored state and says whether each rule catches it. A change of the program is not built. | partly built | M236; open item 110 |
| Coverage | hvx traces the guest's code with Intel Processor Trace (`HVX_EXPLORE_PT_ELF`). A CI report names the properties that no run reached. Coverage-guided exploration found no bug sooner. | partly built | M40, M41, M44, M197 |
| Benchmarks | `perf:` in the manifest counts the work of each workload step, and `hydo perf compare A B` says each change in words. The work is exact on hvx only. | partly built | M237 (`perf-kv`) |
| Contract tests | For functions, see the property-based row. For APIs, `hydo fake from-openapi` builds a fake from the provider's published description, so the client runs against that contract. | partly built | M241, M249 |
| Lint | Not hydo's job. A linter reads source text, and hydo runs programs. | not hydo's job | |
| Acceptance tests | Business rules are checks: a SQL query over the system's tables, or the generic rule over the client's acknowledged operations. `hydo rules check` validates proposed rules on runs without faults. | built | M215, M218, M224, M236, M251 (`blnk`, `lago`, `saleor`) |
| Regression tests | `hydo save` keeps a finding's schedule in `hydo/regressions/`, and `hydo repro --regressions` replays every saved one. | built | M197, M269 |
| Smoke tests | `hydo doctor` checks the machine, `hydo build --check` checks the project, and the schedule `disk=none \| run` runs the system once without faults. | built | M239; the quickstarts |
| Usability tests | Not hydo's job. They need people who use the product. | not hydo's job | |
| UI and UX tests | `browser:` runs a team's Playwright tests in the VM and watches every page. `browser.explore` walks through the pages without tests. `browser.screens: compare` compares screenshots pixel for pixel. The look and feel needs people. | partly built | M253, M254, M258, M273, M284 (`web-shop`, `memos`, `vikunja`); open items 124, 131, and 145 |
| Penetration tests | Not hydo's job. They need an attacker's tools and goals. `hydo fuzz` of a decoder of untrusted input is the nearest capability (Qdrant's `decompress_sequence`, M275). | not hydo's job | |
| Threat modeling | Not hydo's job. It is a design review that people do. | not hydo's job | |
| Performance tests | `perf:` counts work per step, and `hydo run --memory` prints each service's peak memory. Time in the VM is virtual, so speed on real hardware is out of reach (`docs/LIMITS.md`). | partly built | M237, M239 |

### At deploy

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Integration tests | The whole system runs in one VM. `hydo onboard`, `hydo compose`, and `hydo k8s` start the project from a repository, a Compose file, or Kubernetes manifests. | built | M84, M214, M229, M243; every example |
| Tap compare | `hydo compare-versions` runs the same schedules on two versions and compares the verdicts, the client's outcomes and histories, and the stored state (`services[].compare`). `hydo versions` replays one finding on several versions. | built | Milestone 298 (Redis settings, Formance Ledger v2.3.24 against v2.4.0); M221 |
| Load tests | `parallel_driver_` commands run copies of a workload command at once, so races show without faults. Throughput is not hydo's job: time in the VM is virtual, and a guest has at most 8 GiB of RAM. | built for concurrency | M86; `formance-ledger`, `blnk`, `medusa` |
| Shadowing | Not hydo's job. It sends copies of production traffic to a new version. The nearest command is `hydo compare-versions`, on schedules instead of traffic. | not hydo's job | |
| Configuration tests | Build parameters (`params:`, `hydo build --set k=v`) give one image per setting, and `hydo versions --param` replays a finding per setting. The idea [Configuration as an explored dimension](#configuration-as-an-explored-dimension) draws the settings. | partly built | `dragonfly` (`--set takeover=yes`), `swupdate` (`--set dio=1`); Milestone 299, being built |
| Soak tests | The idea [Soak tests with exact resource counts](#soak-tests-with-exact-resource-counts). `hydo run --memory` (M239) and `perf:` (M237) are its first parts. | idea | |

### At release

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Canary releases | Not hydo's job. A canary needs a share of production traffic. The upgrade token `U=` tests the switch of a version itself, member by member, with rollbacks. | not hydo's job | M238 (`etcd-upgrade`) |
| Monitoring | Not hydo's job. It watches a live production system. | not hydo's job | |
| Traffic shaping | Not hydo's job in production. Inside the VM, `N=` (loss, delay, and partitions) and `O=` (one-way loss) shape the guest's traffic as faults. | not hydo's job | M24, M195 |
| Feature flags | Not hydo's job to serve flags. A flag can be a build parameter, and the idea [Configuration as an explored dimension](#configuration-as-an-explored-dimension) would draw its values. | not hydo's job | |
| Exception tracking | In production, not hydo's job. Before production, `errors: true` makes each error-level line of a service's log a finding until its kind is accepted. | built | M94, M215 (`formance-ledger`, the only signal of ledger#2542) |

### After release

| Technique | What hydo has | Status | Evidence |
|---|---|---|---|
| Teeing | Not hydo's job. It copies production traffic. | not hydo's job | |
| Profiling | `perf:` gives the exact work of each step on hvx. The experiment `HYDO_COST` counted flushes, writes, and branches per phase. Exact profiles per function are the idea [Exact profiles](#exact-profiles). | partly built | M198, M237 |
| Logs and events | Each run keeps its control transcript and the services' logs (`<run>/logs/`). `hydo inspect events` shows the timeline, and `hydo report --html` shows a failure as one web page. | built | M178, M220, M228, M239 |
| Chaos tests | `hydo explore` draws kills, pauses, partitions, power cuts, disk faults, clock steps, storage faults, and upgrades, and shrinks each failure to a replayable schedule. hydo does it in a VM before release, not in production. | built | M38, M102, M195, M238, M245; every example |
| Monitoring | Not hydo's job. | not hydo's job | |
| A/B tests | Not hydo's job. They need real users. | not hydo's job | |
| Tracing | Value history (`// hydo: record`) records chosen values at every call without a change of the run. `HYDO_PREEMPT_RECORD=1` records each forced preemption point. `hydo diff` names where two runs first differ. | partly built | M165, M208, M285; open items 77 and 146 |
| Dynamic exploration | `hydo explore` searches the fault space from seeds, and `browser.explore` walks through a web app's pages. | built | M38, M102, M258 |
| Real user monitoring | Not hydo's job. It needs real users. | not hydo's job | |
| Auditing | `hydo rules scan --kind clock` audits the code for durations on the wall clock. The control transcript records every command and fault of a run. An audit of access or compliance is not hydo's job. | partly built | M248 (`valkey`, valkey#5406) |
| On-call experience | When an incident's cause is a fault that hydo can make, a schedule makes it again. `hydo inspect` opens the failed run at any stop, and `hydo report --html` says what broke in plain words. Paging and runbooks are not hydo's job. | partly built | M178, M228 |

## Use cases by who asks

Each section lists the questions of one type of team. The tables give hydo's answer today. The
last line names the ideas that would answer more.

### A backend team with a database or a queue

| The question | What hydo answers today | Status |
|---|---|---|
| Does an acknowledged write survive a power cut? | `C=` cuts the whole guest's power, and `cf=` cuts it at the k-th disk flush (hvx only). History checks and the messages check judge what survived. | built; `nats-jetstream`, `vikunja`, `yugabyte`, `saleor` |
| Does one node's damage lose data or stop the cluster? | `G=` damages one file of a service's storage, and `L=` cuts one service's power. `hydo explore --storage-faults` draws both. | built; `nats-jetstream/blocks`, `redpanda`, `surrealdb` |
| Does a backward clock step break a lease or a timeout? | `T=` steps the guest's wall clock. `hydo rules scan --kind clock` proposes the steps. | built; `valkey`, `temporal`, `dragonfly`, `river` |
| Does an acknowledged job run, and run once? | A `type: messages` check declares the claims. Kills of the process that holds the work test them. | built; `hatchet`, `river`, `redpanda` |
| Do the books still match the client's operations? | The generic rule and domain rules, proposed by `hydo rules scan` and validated by `hydo rules check`. | built; `blnk`, `lago`, `formance-ledger` |
| Where did a wrong value come from? | Value history and value properties on the Mac engine. | partly built; `blnk` (M285, M291) |
| Does the cluster upgrade in place? | `U=` switches a member's version during a run, and `hydo explore --upgrades` draws it. | built; `etcd-upgrade` (M238) |

Ideas that would answer more:

- [Configuration as an explored dimension](#configuration-as-an-explored-dimension);
- [Soak tests with exact resource counts](#soak-tests-with-exact-resource-counts);
- [Value properties over two values and over call order](#value-properties-over-two-values-and-over-call-order);
- [Clocks per service](#clocks-per-service);
- [Repeated fault windows and membership changes](#repeated-fault-windows-and-membership-changes);
- [Resource limits per service](#resource-limits-per-service); and
- [Transactions across messages](#transactions-across-messages).

### A team with a web app

| The question | What hydo answers today | Status |
|---|---|---|
| Why is one of the team's Playwright tests flaky? | `browser:` runs the tests in the VM, and an `H=` delay of the API makes the flaky test fail on every run. | built; `web-shop` (M253) |
| Does a page show an uncaught error, a spinner that stays, or a failed request it never shows? | Five invariants watch every page that a test or a walk opens. | built; `web-shop` (M253) |
| Can hydo test the pages without the team's tests? | `browser.explore` and the token `I=` walk through the pages from a seed, and each run leaves the walk as a Playwright test. | partly built; `memos`, `vikunja` (M258, M273); open items 124 and 131 |
| Is a saved object still saved after a crash? | A rule over the database, with power cuts of the whole guest. | built; `vikunja` finding 1 (M273) |
| Did a page change visually? | `browser.screens: compare` compares each page with a recorded screenshot. | partly built; M254; the Mac engine ran no comparison (open item 145) |
| Does the frontend behave when a payment API fails? | Fakes, emulators, and `hydo explore --protocol-cover`. | built; `web-shop`, `openapi-payouts` (M257) |

Ideas that would answer more:

- [A walk that follows the page](#a-walk-that-follows-the-page);
- [Faults inside a step](#faults-inside-a-step); and
- [Stateful emulators](#stateful-emulators).

### A library author in Rust

| The question | What hydo answers today | Status |
|---|---|---|
| Does a function keep its contract for every valid input? | `hydo fuzz` generates inputs from `requires` and `input` lines, judges each call by `ensures`, and shrinks a failure. | partly built; M275, M282, M289; open item 139 |
| Does a function that writes files keep its promise under a power cut? | `hydo fuzz --vm` with a `// hydo: durable` check, which runs again in the recovery boot. | partly built; `fuzz-vm` on hvx (M292) and on the Mac engine (M296, M297) |
| Does a function that measures a timeout or waits for a thread keep its contract? | `hydo fuzz --vm` steps the clock, pauses the call or its thread, or kills it, a few milliseconds after the call's start (`E=` and `A=fuzz.call+<ms>:<action>`). | partly built; `fuzz-vm` and rs-snowflake on hvx and the Mac engine (M296) |
| Does a contract hold inside the running system? | `contracts:` in the manifest checks every call at hardware breakpoints, without a change of the run. | partly built; the Mac engine and KVM (M241, M255) |
| Is a flaky test a real race? | `workload.kind: test` with a pause of one thread (`P=workload/<thread>@...`). | built; `qdrant-flaky-10200` (M226) |

Ideas that would answer more:

- [Function fuzzing of trait methods and of types with pointers](#function-fuzzing-of-trait-methods-and-of-types-with-pointers);
- [Active property tests from a snapshot](#active-property-tests-from-a-snapshot); and
- [Function fuzzing in more languages](#function-fuzzing-in-more-languages).

### An embedded or device team

| The question | What hydo answers today | Status |
|---|---|---|
| Does a power cut during an update leave a bootable slot of an allowed version? | Reactions at the update's console events (`A=...:cut`), `C=`, and `cf=`. | built; `swupdate`, `mender`, `rauc` (M259, M272) |
| Does the update hold with real reboots and separate storage chips? | `guest.reboot: true` and `guest.disks`, each disk with its own write cache. | partly built; added disks on the Mac engine and KVM only (M263; open item 128) |
| Where exactly in a commit does a cut break the device? | One run per delay of a reaction, written as a shell loop. | partly built; `mender` (M276; open item 136) |

Ideas that would answer more:

- [Added disks on hvx](#added-disks-on-hvx);
- [A built-in search of a reaction's delay](#a-built-in-search-of-a-reactions-delay); and
- [A device's boot details](#a-devices-boot-details).

### A team that adopts hydo in CI

| The question | What hydo answers today | Status |
|---|---|---|
| Did this pull request add a failure? | `hydo ci run` decides each finding against the base by replay: `new`, `pre-existing`, `fixed`, and more. | built; M197; a GitHub trial in a private repository (open item 67) |
| Can a known bug stay without failing every pull request? | `hydo ci accept` writes the finding into `hydo/known-findings.yaml`. | built; M223 |
| Can the run fit a time budget? | `budget:`, `--shard I/N` with `hydo ci merge`, and `seeds_from: history`. | built; M223, M286; ran on the Mac only (open item 67) |
| What is the history of each finding? | `hydo ci record` keeps a store, and `hydo ci page` writes it as one web page. | built; M286; not run on GitHub |
| Can the pull request show only what it changed? | `hydo ci run` writes `comment.md`, which the GitHub action posts and edits. | built; M286 |
| Which commit introduced a finding? | `hydo versions --git A..B` replays it per commit, by hand. | partly built; M221 |

Ideas that would answer more:

- [CI stage 3](#ci-stage-3-bisection-the-object-store-and-one-shrink-across-shards); and
- [Cost per operation in CI](#cost-per-operation-in-ci).

### A maintainer who receives a bug report

| The question | What hydo answers today | Status |
|---|---|---|
| Can the maintainer see it fail? | `hydo run <image> '<schedule>'` repeats the run bit for bit, and `hydo repro` replays a run directory. | built; every example |
| Why did it fail? | `hydo report --html`, `hydo inspect` at any stop of the run, `hydo diff`, and `hydo explain`. | built; M165, M178, M220, M228 |
| Is it a bug of the system or of the setup? | Each failure class has a label and advice (`hydo_explore::advice`). A script under Docker in the example's `native/` folder confirms the cause outside hydo. | built; M228; `redpanda/native`, `saleor/native` |
| Which versions have it? | `hydo versions` bisects over versions, assuming one change point. | built; `formance-ledger` (M221) |
| Is the fix complete? | The same schedule on the fixed build, and the saved regression in CI. | built; `blnk`, `qdrant-flaky-10200` |

Ideas that would answer more:

- [Deeper explanations of where two runs differ](#deeper-explanations-of-where-two-runs-differ);
- [An MCP server for hydo inspect](#an-mcp-server-for-hydo-inspect); and
- [A preemption record on hvx](#a-preemption-record-on-hvx).

### Someone who evaluates an upgrade or a dependency bump

| The question | What hydo answers today | Status |
|---|---|---|
| Does the new version pass every saved regression? | `hydo repro --regressions` on an image of the new version, or `hydo ci run`. | built; M197 |
| Is a known finding still there in the new version? | `hydo versions` with `--param`, `--git`, or `--images`. | built; M221 |
| Does a rolling upgrade, or a rollback, keep the data? | `U=<target>@<at>:<version>`, with `+<back>` for a rollback. | built; `etcd-upgrade`, `surrealdb/upgrade` (M238, M277) |
| Does the new version answer as the old one did? | `hydo compare-versions` runs the same schedules on both versions and names the first difference. | built; Milestone 298 |
| Does each operation cost more after the bump? | `perf:` and `hydo perf compare`, exact on hvx. | partly built; M237 |
| Did the bump change a default setting that matters? | Not answered today. | idea; [Configuration as an explored dimension](#configuration-as-an-explored-dimension) |

## The ideas

Each idea says what it is, why it matters, what it would need, and where it came from. An idea
that a milestone builds moves into the tables of this page with its new status.

### Configuration as an explored dimension

- **What it is.** The explorer draws a system's settings as it draws faults, and the shrinker
  removes the settings that a failure does not need.
- **Why it matters.** Many findings depend on one setting: Redis's `appendfsync`, libSQL's
  durability, Dragonfly's `takeover`. Today each setting is a separate image, chosen by hand.
- **What it needs.** Settings that change without a new image, or one image per drawn setting,
  and a schedule token that names them.
- **Where it came from.** The user's chart of testing stages; being built as Milestone 299
  (tentative number), not in `main` on 2026-11-17.

### Soak tests with exact resource counts

- **What it is.** A long run that counts memory, open files, disk use, and work per operation
  at every step, and fails when a count grows without a bound.
- **Why it matters.** A leak shows only after hours. In a deterministic run, every count is
  exact, so a growth of one page per operation is visible.
- **What it needs.** The guest-side counters of `docs/design/cost-tracking.md` section 6, read at
  every step, and runs longer than the 65,536 messages of one run (`hydo_engine_proto::MAX_SENDS`).
- **Where it came from.** A conversation with the user; the cost experiment (M198), `hydo run
  --memory` (M239), and `perf:` (M237).

### A check that an error path is reached under faults

- **What it is.** A declaration that a panic, an error branch, or a recovery path must run in at
  least one run of an exploration. hydo reports the paths that no run reached.
- **Why it matters.** Static architecture rules, such as those of the cargo-pup lint tool, say
  where errors may go. Only a run shows that the error handling runs at all under real faults.
- **What it needs.** The contracts' breakpoints at the paths' functions, or hvx's Processor
  Trace, and a results addition that lists the paths never reached.
- **Where it came from.** A conversation with the user about static architecture rules, and the
  user's direction to observe the code that runs under faults.

### Value properties over two values and over call order

- **What it is.** A `keep` line that compares two recorded names, a value with a constant, or the
  order in which functions ran.
- **Why it matters.** Blnk's overdraft limit had two copies, one in the transaction and one in its
  metadata. A property over both names states the bug directly.
- **What it needs.** A wider language of `// hydo: keep`, the property table of `result.json`
  with the `keep` lines, and `hydo rules check` that runs them.
- **Where it came from.** Open item 146; the section "Next steps" of `docs/design/value-history.md`.

### Function fuzzing in more languages

- **What it is.** `hydo fuzz` for Go and other languages, beside Rust.
- **Why it matters.** Many of the onboarded systems are Go. Build it only if an evaluation shows
  that the language holds users back: the user's direction is Rust and the experience first.
- **What it needs.** A harness writer and a value reader per language. Before that, a fresh
  user's evaluation of the kind that M280 and M288 made must show the need.
- **Where it came from.** Open item 139; the user's direction on function fuzzing.

### Function fuzzing of trait methods and of types with pointers

- **What it is.** `hydo fuzz` of trait methods, methods of generic `impl` blocks, enumeration
  results, and values built by a named constructor.
- **Why it matters.** Most of a crate's API is such methods. base64's engine, byteorder's
  `ReadBytesExt`, and bytes' `Bytes` still need hand-written property functions.
- **What it needs.** A reading of trait implementations, constructors named in an `input` line,
  and a reader of enumerations.
- **Where it came from.** Open item 139; M280 and M288.

### Active property tests from a snapshot

- **What it is.** From a snapshot at a function's entry inside a running system, hydo calls the
  function with inputs from its `input` domains, in branches that leave the run as it was.
- **Why it matters.** It tests a function in the real state of the system, not in a copy of the
  crate.
- **What it needs.** Branches from a mid-run snapshot on the Mac engine or KVM, and a call
  injected at a breakpoint.
- **Where it came from.** The section "Next steps" of `docs/design/contracts.md`.

### Contracts for interpreted languages

- **What it is.** Contracts and value history for interpreted and JIT-compiled programs.
- **Why it matters.** Lago, Saleor, and Ghost run on Ruby, Python, and Node. Today contracts read
  only compiled Rust and Go through their debug information.
- **What it needs.** A stop in each interpreter's call path, and reads of the interpreter's own
  structures.
- **Where it came from.** The section "Next steps" of `docs/design/contracts.md`.

### A guest counter for loops without exits on the Mac engine

- **What it is.** A counter of the guest's branches on the Mac, so that the engine can stop a
  loop that makes no VM exits.
- **Why it matters.** k3s's kine loops without exits, so k3s does not start on the Mac engine.
  KVM has `guest.branch_tick` for this (M274).
- **What it needs.** An M2 has no such counter. The guest PMU that macOS 15 emulates with guest
  EL2 on M3 and later chips was not measured.
- **Where it came from.** Open items 13 and 119; `docs/design/exit-free-loops.md`.

### Breakpoints for contracts and value history on hvx

- **What it is.** Hardware breakpoints in the hvx module, so that contracts and value history run
  on hvx as on the Mac engine and KVM.
- **Why it matters.** hvx is the reference engine and runs the most examples. uprobes changed the
  fingerprint (M241).
- **What it needs.** The side module of the section "The hvx design" of `docs/design/contracts.md`, the full
  suite on it, and then a reload of the main module.
- **Where it came from.** Open items 113 and 146.

### CI stage 3: bisection, the object store, and one shrink across shards

- **What it is.** The commit that introduced each new finding (`hydo bisect`), `hydo ci
  reproduce` on a laptop, an object-store backend for the history, and one shrink per signature
  across shards.
- **Why it matters.** A team with many shards and a long history needs the commit that caused a
  finding, and a store that is not a directory.
- **What it needs.** About four to six milestones, mostly independent of each other
  (`docs/design/ci.md` section 9).
- **Where it came from.** `docs/design/ci.md`; open item 67.

### Cost per operation in CI

- **What it is.** A pull request comment that names exact changes of cost, such as flushes per
  acknowledged write, base against head.
- **Why it matters.** A timing benchmark in CI is noisy. A count from a replayable run is exact.
- **What it needs.** A `cost` object in `result.json` from the host-side counters, and the
  guest-side counters for marked schedules.
- **Where it came from.** `docs/design/cost-tracking.md` section 6 (M198).

### Exact profiles

- **What it is.** The work of each function of a service, counted exactly in a run under faults.
- **Why it matters.** A profile that repeats bit for bit shows a change of a few branches between
  two builds.
- **What it needs.** Samples at exact branch counts on hvx, or breakpoints at chosen functions.
- **Where it came from.** A conversation with the user on observing whole systems: value history,
  the code that runs under faults, and exact profiles.

### An MCP server for hydo inspect

- **What it is.** The commands of `hydo inspect` as a Model Context Protocol (MCP) server, the
  interface that AI coding tools use to call programs.
- **Why it matters.** The first users of `hydo inspect` are AI agents (`docs/design/inspect.md`).
  Today they drive it through a shell and files.
- **What it needs.** A server over the existing session, with bounded answers.
- **Where it came from.** Open item 56; the section "Left for later" of `docs/design/inspect.md`.

### Environments for AI agents

- **What it is.** An environment that an agent can create from an image, branch, and drop, each
  branch exact and replayable.
- **Why it matters.** Training and evaluation of agents need environments that repeat. The spike
  of M199 showed branches with `hydo inspect exec --branch`.
- **What it needs.** A network through recorded outside services, branches that can be dropped,
  an incremental state hash, and several vCPUs under lock-step control.
- **Where it came from.** Open item 68; `docs/design/agent-environments.md`.

### Deeper explanations of where two runs differ

- **What it is.** `hydo diff` and `hydo explain` that also compare the client's replies, the disk
  requests, and guest memory at each stop.
- **Why it matters.** For a durability bug, the disk requests name the writes that were not
  flushed. Today the diff stops at the controller's records.
- **What it needs.** An engine log of disk requests and packets outside the fingerprint, and a
  hash of RAM and disk at a chosen stop.
- **Where it came from.** Open item 49; the section "What deeper levels need later" of `docs/design/explain.md`.

### A preemption record on hvx

- **What it is.** A record of each tick that a timing schedule forced on hvx, with the guest's
  instruction pointer and page table.
- **Why it matters.** Without it, a verdict that flips between two hvx timing schedules cannot be
  traced to the tick that made the difference. KVM and the Mac engine have the record (M208).
- **What it needs.** A small log in the module and a new ioctl to read it.
- **Where it came from.** Open item 77.

### Resource limits per service

- **What it is.** Memory and CPU limits per service, as Compose's `mem_limit` and `cpus` give a
  container.
- **Why it matters.** A program that sizes itself from `memory.max` or `cpu.max` uses the whole
  guest today, which hides its behaviour at its real size.
- **What it needs.** A cgroup per service under the cgroup2 mount of M283.
- **Where it came from.** Open item 144.

### Clocks per service

- **What it is.** A clock step for one service while the others keep the guest's clock.
- **Why it matters.** Real clusters have clocks that differ between nodes. Today `T=` steps the
  whole guest's clock.
- **What it needs.** A time-namespace variant of the guest kernel, or a time offset per process.
- **Where it came from.** Item 9 of the section "Ranked plan" of `docs/design/jepsen-gaps.md`.

### Repeated fault windows and membership changes

- **What it is.** One token that repeats a fault (`K=n1@3+2/7+2`), and faults that add or remove
  a cluster's members during a run.
- **Why it matters.** Some bugs need a fault that comes back, or a member that joins during a
  failure.
- **What it needs.** New schedule syntax, an explorer option, and the system's own commands for
  membership.
- **Where it came from.** Items 6 and 10 of the section "Ranked plan" of
  `docs/design/jepsen-gaps.md`.

### Added disks on hvx

- **What it is.** `guest.disks` on hvx. The Mac engine and KVM have it since M263.
- **Why it matters.** A device model with separate storage chips runs only on the Mac engine and
  KVM today, and hvx refuses it before any VM.
- **What it needs.** A second block device in the module with its own PCI function, queue, cache,
  snapshot, and fingerprint part; first on a side module.
- **Where it came from.** Open item 128.

### A built-in search of a reaction's delay

- **What it is.** An explorer option that runs one reaction at every delay of a range, to map the
  window where a fault breaks a promise.
- **Why it matters.** The explorer draws a reaction's delay from six fixed values. Mender's
  window on hvx was 69 to 87 ms after the event and needed a shell loop.
- **What it needs.** A range in the reaction's token and an explorer mode that walks it.
- **Where it came from.** Open item 136.

### A device's boot details

- **What it is.** The device's kernel command line, squashfs, and the device mapper in the Mac
  engine's guest kernel.
- **Why it matters.** RAUC reads the booted slot from the kernel command line, and its `verity`
  bundles need the device mapper.
- **What it needs.** A kernel variant for devices and a manifest key for the command line.
- **Where it came from.** Open item 125.

### Faults inside a step

- **What it is.** A kill, a pause, or a network fault in the middle of a workload step, timed by
  the guest's clock.
- **Why it matters.** A page walk's step takes one step, so a fault starts only between steps.
  Memos's finding 2 needs a refused write in the middle of a step. Since Milestone 296 a fuzzed
  call gets its faults inside it from a reaction at the call's start.
- **What it needs.** A timed form of each guest fault that the explorer draws for every manifest.
- **Where it came from.** Open items 131 and 139.

### A walk that follows the page

- **What it is.** A page walk that chooses its actions by what the page shows, such as a memo by
  its text, with breadth that depends on the page alone.
- **Why it matters.** A fault changes the page, and so the walk. Then a shrink candidate takes
  other actions, and the walk loses its failure.
- **What it needs.** A choice of action keyed by the control's role and name, not by its place.
- **Where it came from.** Open items 129 and 131.

### Leaving a fault kind or a known bug out of an exploration

- **What it is.** An option that leaves one fault kind out, and a way to keep a known finding
  from coming back in every schedule.
- **Why it matters.** The SurrealDB and Dragonfly examples needed copies of the manifest for this.
  In SurrealDB, half of the drawn clock steps reached a known bug.
- **What it needs.** An explorer option, and `hydo explore` that reads `faults:` with the build's
  parameters.
- **Where it came from.** Open items 140 and 141.

### Shrinking a run that ends in error

- **What it is.** The shrinker for a run that ends in `ERROR`, such as a node that does not start
  or a drain that cannot finish.
- **Why it matters.** hydo shrinks only violations. Redpanda's errors were shrunk by hand.
- **What it needs.** A signature for an error that a shrink candidate must keep.
- **Where it came from.** Open item 142.

### Upgrades of image services and several upgrades per schedule

- **What it is.** `U=` for a service that runs a Docker image, and more than one `U=` in a drawn
  schedule.
- **Why it matters.** Many teams run their services from images, and an upgrade often follows
  another.
- **What it needs.** Versions as images in the manifest, and an explorer that draws several `U=`.
- **Where it came from.** Open item 109.

### Kubernetes beyond one pod

- **What it is.** `hydo k8s` with the replicas of a Deployment, a Service's port mapping,
  `localhost` shared by a pod's containers, and the Kubernetes API.
- **Why it matters.** A team whose system depends on leases or operators cannot test them through
  the importer today.
- **What it needs.** A proxy per Service, a network namespace per pod, and k3s in the guest on
  every engine (open item 119).
- **Where it came from.** Open items 114 and 119.

### Stateful emulators

- **What it is.** Emulators that keep state and send webhooks, such as a Stripe emulator that
  remembers payments.
- **Why it matters.** stripe-mock keeps no state and sends no webhooks, so a webhook's bugs stay
  out of reach.
- **What it needs.** An image with Node.js 24 that hydo builds and pins, for the emulate project
  of vercel-labs.
- **Where it came from.** Open item 116.

### A rules scan that reads syntax

- **What it is.** `hydo rules scan` and the clock audit that parse the code instead of matching
  words.
- **Why it matters.** The scan read none of Saleor's 25 `models.py` files, and the clock audit
  found no place in Redpanda's C++.
- **What it needs.** A parser per language, and a reading of calls through interfaces and
  channels.
- **Where it came from.** Open items 110, 117, and 122.

### Transactions across messages

- **What it is.** A claim of the messages check over consume-transform-produce transactions, with
  a cycle search over them.
- **Why it matters.** Redpanda's half-committed transaction shows as a loss today, without the
  transaction it belongs to.
- **What it needs.** A `txn` operation with send and commit parts in the history format.
- **Where it came from.** The section "Left open" of `docs/design/messaging.md`.

## How to keep this page current

Every milestone that builds or changes a capability updates its rows here: the status, the
command or key, and the evidence. A milestone that builds an idea moves it from
[The ideas](#the-ideas) into the tables. A new idea from an open item, a design page, or a
conversation gets its entry here in the same commit. `AGENTS.md` "How to record new work" step 4
gives the rule.
