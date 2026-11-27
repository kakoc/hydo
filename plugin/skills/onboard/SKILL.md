---
name: onboard
description: Set up deterministic fault testing with hydo for a software project. Writes the repository's hydo/ folder (the system's services, workload commands and checked properties), starting from a docker-compose file when there is one, so that hydo can run the whole system under process kills, pauses, network faults, power cuts, clock jumps and disk faults and replay every failure bit for bit. Use when asked to onboard, add or test a project, repository, service or Docker Compose stack with hydo, to write or fix a hydo.yaml or hydo/ folder, or to add hydo workload commands or checks.
license: Apache-2.0
compatibility: Needs the hydo command line tool, 0.1.0 or later (x86_64 Linux, or an Apple Silicon Mac); the skill offers to install it. Every hydo fact comes from the installed hydo (hydo guide); the skill itself needs no network.
metadata:
  version: "0.7.0"
  hydo-min: "0.7.0"
---

# Onboard a project into hydo

hydo runs a whole system (several services in one deterministic virtual machine) under seeded
faults, checks its properties, replays every failure bit for bit and shrinks it to a minimal
schedule. Your job is to write the project's `hydo/` folder: the services, one command per client
operation, and the properties that must hold.

This skill is the **process**. It does not contain hydo's facts (the format's keys, the commands
and their options, the fault kinds, the limits): those change between hydo versions, and the
installed `hydo` prints the ones that are true for it. Never write a key, command or option from
memory or from the web; read it from `hydo guide`.

An onboarding of a real project for hydo's own record (the target, the time limits, judging a
finding, the upstream report) also follows the [onboarding playbook](https://github.com/kakoc/hydo/blob/main/docs/ONBOARDING-PLAYBOOK.md).

## 0. Check hydo

Run `hydo --version`.

- It prints `hydo <version>`: go on. If the version is older than this skill's `hydo-min`
  (frontmatter), tell the user to upgrade (references/install.md) and stop.
- It prints a usage text instead: this is hydo 0.1.0, the first release, which has no
  `--version` (`hydo setup --check` names the version in its first line). It works with this
  skill: see "Facts" below.
- `hydo: command not found`: first look where the installers put it, because a
  non-interactive shell often does not read the startup file that adds it to `PATH`. Run
  `ls ~/.hydo/bin/hydo ~/.cargo/bin/hydo "$(brew --prefix 2>/dev/null)/bin/hydo" 2>/dev/null`.
  If one exists, tell the user "hydo is installed at X but not on this shell's PATH; I will call
  it by that path", and use that path for every `hydo` command.
- None exists: **ask the user** whether to install hydo, saying what will be installed and
  where. Only after a yes, follow [references/install.md](references/install.md). Without hydo,
  stop and say what is missing.

Every network install needs the user's yes first, with what it installs: hydo itself, a Rust
target (`rustup target add aarch64-unknown-linux-musl`), a Go or C toolchain, and Docker images.

Then run `hydo update --check`. It changes nothing; `hydo guide update` explains it.

- Exit status 0: hydo and every copy of this skill are current. Go on.
- Exit status 3: something is newer. Tell the user, in one message, the hydo version and the
  skill copies it names with their versions, and the "Before you upgrade" lines it printed. Ask
  whether to update. On a yes, run `hydo update --yes`, show what it says changed, and go on
  (this session keeps the skill it loaded; a new session loads the new one). On a no, go on
  with what is installed.
- It prints hydo's usage text (exit status 2): this hydo is 0.4.0 or older and has no `hydo
  update`. Offer the one manual upgrade of [references/install.md](references/install.md)
  ("Upgrading"), and do it only after a yes.
- It fails (no network, for example): say so in one line and go on.

Then run `hydo doctor`: it says whether this machine can build and run guests.

## Facts: ask the installed hydo

- `hydo guide workflow`: the folder, the commands of each step below, and the topic that holds
  each detail. Read it first.
- `hydo guide minimum-path`: one page with the least a first folder needs, for a project with
  its own binaries and no Compose file. Start writing from it, and read the full topics when a
  check or a run fails. An older hydo answers "no guide topic": use the full topics then.
- `hydo guide <topic>` for details (`hydo guide --list` lists the topics), `hydo guide schema` for
  the project format (authoritative: read the sections you use before you write them), and
  `hydo guide examples/...` for complete examples.
- With hydo 0.1.0, `hydo guide workflow` answers "no guide topic": run `hydo guide` with no topic
  instead; it prints 0.1.0's whole onboarding guide, and its topics have the same names.
- When a step needs something the installed hydo lacks (a topic it does not know, a key that
  `hydo build --check` refuses although a newer guide lists it), do not work around it: tell the
  user which hydo version is installed, what is missing, and to upgrade (references/install.md).

## 1. Talk to the user

Ask, in one short message: which services make up the system, which operations its clients
perform, and what must never happen (lost acknowledged data, a double charge, an order stuck
forever). Do not ask the owner to list the business rules: you propose them from the project's
own sources in step 3 and show the owner the ones hydo validated. For a message queue, a stream
or a pub/sub system, also read the bundles of `hydo guide messages`: each one names the promises
of a common shape (ordering keys, leases, deduplication ids, groups). If you cannot ask, read the
project's documentation and state your assumptions in the report. Do not invent guarantees the
project does not make.

When the user is not the system's owner and cannot say what must never happen, derive the
promises yourself. Read the deployment manifests and the code: the timeouts, the leases, the
retries, and the replication settings say what the system means to keep. Mark each such promise
"unconfirmed by the owner" in the report.

## 2. Inventory the project

Follow [references/inventory.md](references/inventory.md): every process, its program, its port,
its data directory, its dependencies, and the file where you found each fact. Show it to the user.

## 3. Write the folder

1. Start the folder as `hydo guide workflow` says (with a Compose file: import it first).
2. Decide where every program comes from: the guest has no network, so each program must be in
   the image (`hydo guide workflow`, "Where each program comes from").
3. One service per long-running process (`hydo guide services`). Give each the restart behaviour
   and the durability settings it has in production. Copy production's shape too: read the
   deployment manifests (Helm values, Kustomize overlays, the platform's environment files) and
   list each service's replica count, each cluster's size, and each enabled feature, such as a
   cache or the replication of results. The folder must match them, scaled down only in memory
   (`hydo guide strategies`, "Production's topology" and "The features behind a flag"). Tell
   the user every place where the folder differs from production, and why.
4. One workload command per client operation (`hydo guide composer`). First list the operations
   with `hydo rules scan --kind writes` (`hydo guide rules`, "List the routes that write"). Give
   each route of class `data` or `admin` a command, or tell the user why it has none: a route
   that no command calls is a write that no run tests. Each command records what
   the client saw: acknowledged, failed for certain, or unknown (a timeout is unknown). Derive
   every choice from the seed hydo gives, never from the clock. An outcome recorded as unknown is
   a property that no check judges: for each kind of unknown outcome (a query still running, a
   result that cannot be read), plan in step 5 the `eventually_` check that says how it must end,
   and a check that a completed operation's result stays readable after the heal (`hydo guide
   strategies`, "How each unknown outcome ends"). **Before writing a client for a
   SQL database** (PostgreSQL, MySQL, SQLite or libSQL behind sqld), run `hydo sql-gen` (`hydo
   guide workloads`): it writes ready-made workloads with their checks and the outcome rules, and
   claims what the endpoints' roles allow. Write your own commands only for the operations it does
   not cover (the application's own API), and keep the generated ones beside them.
   An `eventually_` command runs once, so it must retry by itself until its own deadline; an
   `eventually_` check is polled by hydo until it holds (`hydo guide checks`).
5. Plan the checks and the faults together: first read `hydo guide strategies` (an older hydo
   answers "no guide topic"; go on without it). It names the techniques that found bugs in
   systems of the same type, with their evidence. Tell the user which ones you chose and why.
   Checks: propose the business rules from the project's own sources (its schema, API,
   documentation, tests and client library) with
   [references/proposing-rules.md](references/proposing-rules.md), into `hydo/rules.yaml`; hydo
   validates them in step 4. Choose the other checks with
   [references/choosing-checks.md](references/choosing-checks.md), write
   them as `hydo guide checks` says, and prefer hydo's ready-made checkers to hand-written loops:
   a business rule over SQL tables is a `.sql` check (a query that must return no rows), and a
   rule that compares the books with what an outside service saw is one `hydo-sql check` line
   over a `sql` and a `world` source (`hydo guide workloads`).
   Turn on error logs as findings (`hydo guide error-logs`). For a queue, log or stream, write
   the drain of the `messages` check as `hydo guide messages` says ("Write the drain"): it must
   outlast every lease and the system's recovery after a power cut, inside `workload.timeout`.
6. Outside services the system calls (payments, e-mail, object storage, webhooks): offer the
   user a ready-made emulator for each one the catalog has (Stripe, SMTP, S3: one
   `emulators:` line each), and write a fake for the rest. When the service has an OpenAPI
   description, write its fake with `hydo fake from-openapi`, then read the routes it wrote.
   Durability settings become knobs; allow only the fault kinds that make sense for the project
   (`hydo guide outside-services`, `hydo guide faults`).
7. Check the project for the pitfalls in [references/pitfalls.md](references/pitfalls.md).

After **every** file you write, run `hydo build hydo/hydo.yaml --check` and fix what it reports.
It must print `OK` before you go on.

## 4. Build and run (when the user wants it)

Before the first run, read every warning of `hydo build hydo/hydo.yaml --check`. It compares the
image services' unpacked roots with the guest's RAM or disk, and a root that does not fit stops
the guest at boot with a message about something else. Fix such a warning first (`hydo guide
services`, `guest.ram_mib`). An older hydo does not warn: then compare the images' unpacked sizes
with the limits as that topic says.

Use the "Build and run" commands of `hydo guide workflow`: build the image, run one schedule
without faults twice (it must pass, with the same fingerprint both times), validate the proposed
rules (`hydo rules check hydo/rules.yaml --mutate`, step 3 of
[references/proposing-rules.md](references/proposing-rules.md)), show the owner the rules that
hold, and write the confirmed ones as checks. Then run the clock audit, `hydo rules scan --kind
clock` (`hydo guide faults`, "Find the durations measured on the wall clock"), when the
installed hydo has it. Show the owner its first three places in plain words, and offer only
their schedules: `hydo explore --schedule`, after you fill in the placeholders and move the
steps to where that code runs. Mention the rest with the report's last line. Keep `clock` in `faults:`.
Run the sync audit the same way, `hydo rules scan --kind sync` (`hydo guide faults`, "Find the
writes acknowledged before they are synced"), when the installed hydo has it. Show its first
three places and offer their power cuts. Keep `cut` and `powerloss` in `faults:`. Then explore. A first
run that fails points at the folder or at a real bug. Before changing anything, read why: the
`hydo: diagnosis:` lines `hydo run` prints and the `diagnosis` in the run directory's
`result.json`; then, when your hydo has `hydo inspect` (`hydo inspect --help`), look inside the
failed run at the stop that matters (`hydo guide inspect`). Do not add debug commands to the
image to find a cause: that changes the run's timing, and the failure can go away.

When the first exploration finds nothing, do not report "no bugs" yet. Run a second round, and
tell the user what it adds:

- more seeds, 60 or more;
- the fault families that `hydo guide strategies` names for this type of system, such as pauses
  longer than the failure detector's timeout;
- the liveness checks of step 3.4, if the first round lacked them;
- production's shape, if the first round scaled it down.

Report "no bugs" only after the second round, with what each round ran.

## 5. Report

Give the user: the folder you wrote; the inventory; **each check, for review** (the checks are
the part that needs human judgment; say which ones you are unsure about and why); the proposed
rules as `hydo rules check` reported them: the ones that hold, plain words first, with their
evidence, and apart the ones that failed without faults, each a wrong rule or a bug; the places
of the clock audit and of the sync audit, with the schedules you ran and their verdicts; the knobs; the
pitfalls you found and what you did about them; each promise you derived without the owner,
marked "unconfirmed by the owner"; each place where the folder differs from production's shape;
the explorations of each round, with their seeds and options; what you could not determine
(unknown readiness checks, unclear data directories, missing programs, ambiguous promises), each
with what happens if it stays as it is; the hydo version you used; and the exact build and first-run commands.

## Platforms

A hydo on Linux builds and runs x86_64 guests; a hydo on an Apple Silicon Mac builds and runs
arm64 guests. Programs and images must be built for the guest's architecture. Some features exist
on one platform only: `hydo guide capabilities` says which, for the installed version.
