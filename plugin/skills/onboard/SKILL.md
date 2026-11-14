---
name: onboard
description: Set up deterministic fault testing with hydo for a software project. Writes the repository's hydo/ folder (the system's services, workload commands and checked properties), starting from a docker-compose file when there is one, so that hydo can run the whole system under process kills, pauses, network faults, power cuts, clock jumps and disk faults and replay every failure bit for bit. Use when asked to onboard, add or test a project, repository, service or Docker Compose stack with hydo, to write or fix a hydo.yaml or hydo/ folder, or to add hydo workload commands or checks.
license: Apache-2.0
compatibility: Needs the hydo command line tool, 0.1.0 or later (x86_64 Linux, or an Apple Silicon Mac); the skill offers to install it. Every hydo fact comes from the installed hydo (hydo guide); the skill itself needs no network.
metadata:
  version: "2.0.1"
  hydo-min: "0.1.0"
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
forever). Then name the project's domain (a shop, a bank, a booking system, a job queue) and ask
which of that domain's standard rules hold for it: read the list for the domain in `hydo guide
workloads`, "Business rules: ask the owner" (for a shop: units sold never exceed units stocked,
refunded never exceeds captured, one payment per order, no money held without an order), and ask
the owner to confirm, reject or add to them. For a message queue, a stream or a pub/sub system,
use the bundles of `hydo guide messages` as that list: each one names the promises of a common
shape (ordering keys, leases, deduplication ids, groups). If you cannot ask, read the project's documentation
and state your assumptions in the report. Do not invent guarantees the project does not make.

## 2. Inventory the project

Follow [references/inventory.md](references/inventory.md): every process, its program, its port,
its data directory, its dependencies, and the file where you found each fact. Show it to the user.

## 3. Write the folder

1. Start the folder as `hydo guide workflow` says (with a Compose file: import it first).
2. Decide where every program comes from: the guest has no network, so each program must be in
   the image (`hydo guide workflow`, "Where each program comes from").
3. One service per long-running process (`hydo guide services`). Give each the restart behaviour
   and the durability settings it has in production.
4. One workload command per client operation (`hydo guide composer`). Each command records what
   the client saw: acknowledged, failed for certain, or unknown (a timeout is unknown). Derive
   every choice from the seed hydo gives, never from the clock. **Before writing a client for a
   SQL database** (PostgreSQL, MySQL, SQLite or libSQL behind sqld), run `hydo sql-gen` (`hydo
   guide workloads`): it writes ready-made workloads with their checks and the outcome rules, and
   claims what the endpoints' roles allow. Write your own commands only for the operations it does
   not cover (the application's own API), and keep the generated ones beside them.
5. Checks: choose them with [references/choosing-checks.md](references/choosing-checks.md), write
   them as `hydo guide checks` says, and prefer hydo's ready-made checkers to hand-written loops:
   a business rule over SQL tables is a `.sql` check (a query that must return no rows), and a
   rule that compares the books with what an outside service saw is one `hydo-sql check` line
   over a `sql` and a `world` source (`hydo guide workloads`).
   Turn on error logs as findings (`hydo guide error-logs`).
6. Outside services the system calls (payments, e-mail, webhooks) become fakes; durability
   settings become knobs; allow only the fault kinds that make sense for the project
   (`hydo guide outside-services`, `hydo guide faults`).
7. Check the project for the pitfalls in [references/pitfalls.md](references/pitfalls.md).

After **every** file you write, run `hydo build hydo/hydo.yaml --check` and fix what it reports.
It must print `OK` before you go on.

## 4. Build and run (when the user wants it)

Use the "Build and run" commands of `hydo guide workflow`: build the image, run one schedule
without faults twice (it must pass, with the same fingerprint both times), then explore. A first
run that fails points at the folder or at a real bug. Before changing anything, read why: the
`hydo: diagnosis:` lines `hydo run` prints and the `diagnosis` in the run directory's
`result.json`; then, when your hydo has `hydo inspect` (`hydo inspect --help`), look inside the
failed run at the stop that matters (`hydo guide inspect`). Do not add debug commands to the
image to find a cause: that changes the run's timing, and the failure can go away.

## 5. Report

Give the user: the folder you wrote; the inventory; **each check, for review** (the checks are
the part that needs human judgment; say which ones you are unsure about and why); the knobs; the
pitfalls you found and what you did about them; what you could not determine (unknown readiness
checks, unclear data directories, missing programs, ambiguous promises), each with what happens if
it stays as it is; the hydo version you used; and the exact build and first-run commands.

## Platforms

A hydo on Linux builds and runs x86_64 guests; a hydo on an Apple Silicon Mac builds and runs
arm64 guests. Programs and images must be built for the guest's architecture. Some features exist
on one platform only: `hydo guide capabilities` says which, for the installed version.
