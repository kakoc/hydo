---
name: hydo-onboard
description: Set up deterministic fault testing with hydo for a software project. Writes the repository's hydo/ folder (hydo/hydo.yaml with the services, workload commands in hydo/workload/, properties in hydo/checks/), starting from a docker-compose file when there is one, so that hydo can run the whole system under process kills, pauses, network faults, power cuts, clock jumps and disk faults and replay every failure bit for bit. Use when asked to onboard, add or test a project, repository, service or Docker Compose stack with hydo, to write or fix a hydo.yaml or hydo/ folder, or to add hydo workload commands or checks.
compatibility: Needs the hydo command line tool (x86_64 Linux with VT-x, or an Apple Silicon Mac) or a checkout of hydo's source repository, a POSIX shell and Python 3.9 or later. Writing the hydo/ folder needs no network; importing a Compose file resolves image digests once.
metadata:
  hydo-version: "0.1"
---

# Onboard a project into hydo

hydo runs a whole system (several services in one deterministic virtual machine) under seeded
faults and checks its properties. Every run can be replayed bit for bit, and the explorer
shrinks each failure to a minimal schedule. Your job is to write the project's `hydo/` folder:

```
hydo/
  hydo.yaml        the system: files, services, data disk, guest settings, knobs, allowed faults
  workload/        commands, one executable per operation: parallel_driver_<name>, first_<name>, ...
  checks/          properties: final_<name>, always_<name>, eventually_<name>
  regressions/     saved minimal schedules (<name>.schedule), replayed by `hydo repro --regressions`
.hydo/             build cache (git-ignored)
```

hydo composes the commands into seeded sequences and concurrent mixes (the "test composer"),
places faults between and inside them, and runs the checks. Write commands and checks, not one
fixed script. The older "numbered steps" workload (`workload.run`) is still supported; use it
only in the cases listed in [references/numbered-steps.md](references/numbered-steps.md).

## Where the documentation is

- `hydo guide` prints this file. `hydo guide <topic>` prints one reference file of this skill
  (`composer`, `checks`, `invariant-patterns`, `outside-services`, `faults`, `hazards`, `sdks`,
  `capabilities`, `numbered-steps`), `hydo guide manifest` prints `MANIFEST.md`, which explains
  the project format (`hydo.yaml` and the `hydo/` folder are one model, `format: 1`), and `hydo
  guide schema` prints the format's JSON Schema. **The schema is authoritative: read the
  sections you use before you write them.** `hydo guide --list` lists every topic.
- Write `format: 1` in every `hydo.yaml` (the skeleton has it). Within format 1 hydo only adds
  keys; an unknown key is an error, so a typo fails the build. `hydo migrate` rewrites an older
  file in the current spelling (for example a history check written in `test.checks` moves to
  the top-level `checks:`); a newer format is refused with the format it needs.
- Without the `hydo` command, inside hydo's own source repository, the same files are
  `skills/hydo-onboard/`, `hydo/onboard/MANIFEST.md` and `hydo/schema/project-v1.json`, and
  the `hydo-build` program (`cargo build --release -p hydo-build`, then
  `target/release/hydo-build <manifest> --check`) replaces `hydo build <manifest> --check`.
- `hydo doctor` shows which backend this machine has: the Linux release builds x86_64 guests,
  the Mac release builds arm64 guests. Some features exist on one backend only, or only when
  building from hydo's repository. Read [references/capabilities.md](references/capabilities.md)
  before you use `addr:`, `fakes:`, `proxies:`, `hydo-read`, `checks:`, `buggify:`, `params:`,
  `faults: [freeze]` or `guest.cpu`.
- Complete examples in this skill: `assets/example-redis-composed/hydo/` (the `hydo/` folder
  with commands and checks) and `assets/example-miniflux.yaml` (two services, numbered steps).

## Talk to the user first

Ask, in one short message, which services make up the system under test, which operations its
clients perform, and what must never happen (lost acknowledged data, a double charge, an order
stuck forever). If you cannot ask, read the project's documentation and state your assumptions
in the report. Do not invent guarantees that the project does not make.

## Steps

### 1. Start the folder

Run at the repository root:

```sh
hydo init
```

It creates `hydo/hydo.yaml` (a skeleton with `TODO` markers), `hydo/workload/`, `hydo/checks/`
and `hydo/regressions/`, adds `.hydo/` and `hydo/images/` to `.gitignore`, and adds a short hydo
section to `AGENTS.md` between marker comments. It never overwrites a file that exists, so it is
safe to run again. The skeleton's two placeholder files (`workload/parallel_driver_todo`,
`checks/final_todo`) make every run fail on purpose until you replace them.

**If the project has a `docker-compose.yaml`,** import it next. The import replaces the
skeleton's `hydo.yaml`:

```sh
hydo compose docker-compose.yaml --out hydo/hydo.yaml --images-dir images
sh hydo/fetch-images.sh        # copies every image by digest into hydo/images/ (network, once)
```

The importer writes the services (images pinned by digest, commands, environment, readiness
from the healthchecks, ports, volumes on the data disk, and `network: bridge`: each service in
its own network namespace at its own address, as in Compose) and prints warnings for what it
could not map. Read every warning: bind mounts that the program writes to need a hand fix (and,
with `--network loopback` or `--no-addr`, port collisions). It also writes a numbered-steps `workload.run` and a
`TODO` invariant. **Delete `workload.run` and the `TODO` invariant** (keep `workload.steps` and
`workload.timeout`), then add a `test:` section as in step 5. Keep the generated comments that
say where each value came from. The reference is the section "From Docker Compose" of
`hydo guide manifest`.

### 2. Inventory how the project runs

Read these, in roughly this order, and note what each says about processes, ports, data and
configuration: `docker-compose*.yml`, `compose*.yaml`, `.devcontainer/`; `Dockerfile*` (the
binary, `CMD`/`ENTRYPOINT`, `EXPOSE`, `HEALTHCHECK`); `Procfile`, systemd units, Helm charts;
the build files (`Makefile`, `package.json`, `go.mod`, `Cargo.toml`, `pom.xml`); the README's
"run" or "self-host" section and the configuration parser (which settings exist, and which of
them are durability or timeout settings); database migrations and schemas (the keys and
constraints that checks can use); `.proto` files and API specifications; existing integration
or end-to-end tests (candidate workload commands).

Write a short inventory for the user: each process, its binary, its port, its data directory,
what it depends on, and the file where you found each fact.

### 3. Decide where every binary comes from

The guest has no network beyond loopback, so nothing can be fetched at run time. Each binary is
one of these:

- **A Docker/OCI image file** (the simplest choice when the project ships images): `image:` on
  the service, a `docker save` tar or an OCI layout under `hydo/images/`, pinned by digest. The
  service runs chrooted in its own image root. Commands and checks run its tools with
  `hydo-oci <service> <command>`. Schema section "Services from a Docker/OCI image".
- **A static binary** built from the project's source (Go: `CGO_ENABLED=0 go build`; Rust: a
  musl target; C: a static musl compiler), listed under `root.files` with `src:` relative to
  `hydo.yaml`. Keep build outputs out of git.
- **A Nix store path** under `root.nix`, only on a machine with Nix (the whole closure is copied).

### 4. Map each process to a service

Write one `services:` entry for every long-running process. The schema lists every key. The
rules that matter most:

- `name`: lowercase. Fault tokens use it (`K=db@5+2` kills `db` before round 5).
- `cmd`: runs in the foreground (no daemonizing, no `&`). Its data goes under `/data` (the
  persistent data disk and the fault target; list the directories in `data.dirs`), and scratch
  files go under `/tmp`. The root filesystem is read-only.
- `setup`: one-time initialization, written to be idempotent, because it runs before every
  start (also after a kill and in the recovery boot after a power cut).
- `ready`: a cheap command that exits 0 when the service answers (a CLI ping, `SELECT 1`, an
  HTTP health URL, or `grep -qa "<started line>" $HYDO_LOG`).
- `port`: the port that clients use (network faults act on it). For a system of several
  services that call each other, prefer `network: bridge` with an `addr:` (10.89.0.x) per service:
  each service gets its own network namespace and private localhost, and faults on the link
  between two services act on their real traffic (MANIFEST.md "Service networks"; on a Mac it
  needs a release with the second guest kernel, references/capabilities.md).
  Without it, `addr:` is a loopback address (127.x.y.z) that a link fault only sees if the
  program's outgoing connections leave from it.
- `depends_on`; `recovery: false` for a stateless tier that the final checks do not need after a
  power cut.
- `provision`: schemas, topics, users: what must exist before the window under test. Seed data
  that belongs to the test is a `first_<name>` command instead.
- Guest settings: `guest.ram_mib` (default 1024) must hold every service; `guest.cpu: v3` when a
  binary needs AVX or AVX2 (x86_64 only); `guest.clock_after_cut: 60` when a check compares times
  written before and after a power cut.

A project without a long-running service (a library, an embedded database, a CLI) is fine:
leave `services:` empty and let the commands run the program.

### 5. Write the workload as commands (`hydo/workload/`)

Read [references/composer.md](references/composer.md) first. In short:

- Write one executable per operation that a client performs, named `<kind>_<name>`:
  `parallel_driver_<op>` (most operations: run repeatedly, several at once),
  `serial_driver_<op>` (never alongside other drivers: compaction, migrations, admin actions),
  `singleton_driver_<op>`, `anytime_<op>` (reads that may run at any time), `first_<name>` (seed
  data, once, before any fault), and `eventually_<name>` / `finally_<name>` (end-of-run
  commands). Shared shell code goes in `helper_*` files, which are not commands.
- Every file is executable (`chmod +x`) and starts with a `#!` line. A line
  `# hydo: timeout=30` near the top sets the command's timeout.
- One command run is one operation. Use `$HYDO_CMD_ID` (unique in a run) as the operation's id.
  Derive every choice (which account, how much) from `$HYDO_SEED`, never from the clock or
  `$RANDOM`. Give every client call a deadline (`timeout 2 ...`).
- Record what the client saw: `echo $id >> $HYDO_ACKED` only when the system acknowledged the
  operation, and `echo "$id ack|failed|unknown" >> $HYDO_OUTCOMES` for every attempt. Write
  `failed` only when the system said for certain that nothing was applied; a timeout or a lost
  connection is `unknown`.
- Exit status: 0 = the operation succeeded; 1 (or any other code) = it failed, which is normal
  under faults; **100 = the command itself saw a violation**, which fails the run.
- In `hydo.yaml`, `test:` sets how many commands run at once (`parallel`), how many start per
  round (`per_round`) and the round length (`round_ms`). `workload.steps` is the number of
  rounds; faults are placed at round numbers.

**gRPC systems:** generate the drivers instead of writing them. `hydo-grpc-gen` writes one
`parallel_driver_<rpc>` per RPC from the `.proto` files and a hints file; see "Generated gRPC
drivers" in [references/outside-services.md](references/outside-services.md).

**Code-first alternative:** a team that prefers its own language can write the commands and the
checks as functions with an SDK (Rust: the `hydo-test` crate and `cargo hydo`), which writes the
same plan; see [references/sdks.md](references/sdks.md). The `hydo/` folder is the default.

### 6. Write the checks (`hydo/checks/`)

Read [references/checks.md](references/checks.md) and
[references/invariant-patterns.md](references/invariant-patterns.md). Prefer the ready-made
checkers to hand-written loops:

- File names: `final_<name>` (once at the end, also in the recovery boot after a power cut),
  `always_<name>` (also periodically during the run; header `# hydo: every=1`),
  `eventually_<name>` (bounded liveness; header `# hydo: within=20`), and
  `linearizable_<name>` / `list_append_<name>` (a recorded history, checked on the host).
- Exit status: 0 = holds, 1 = violated, 2 = cannot be evaluated now (for example the store is
  down). The last output line is the explanation. A header line `# hydo-msg: <one line>` is the
  property's description.
- Store contents: `hydo-read` and `hydo-check` (subset, exactly-once, disjoint, count,
  never-wrong-value) replace hand-written read-back loops. Side effects at fake outside services:
  `hydo-world check at-most-once|exactly-once|effect-for-acked|none-after`.

Derive every property from something the system promises (its documentation, its API contract,
its schema constraints), not from what the code happens to do. Typical properties: every
acknowledged write survives; every acknowledged operation is applied exactly once; an operation
reported as failed was not applied; domain rules hold (no negative balance, totals conserved); a
version that a client saw never goes back; a cache never holds a value that the store does not.

Then tell the user plainly: **the checks are the part that needs human judgment; please review
each one**, and say which ones you are unsure about and why.

Also turn on error logs as findings: `errors: true` in `hydo/hydo.yaml`, plus `errors.files:` for
every log file a service writes on its own and `format: redis` for Redis. Unexpected error lines
then become findings even when every check passes; the user triages them into
`hydo/known-errors.yaml` (`hydo errors accept`). See [references/error-logs.md](references/error-logs.md).

### 7. Outside services, faults and knobs

- Services outside the system (a payment provider, an email API, webhooks) become **fakes** that
  answer and record every request. A **proxy** puts protocol faults in front of a real service.
  See [references/outside-services.md](references/outside-services.md).
- `faults:` lists the fault kinds that make sense for the project: `kill, pause, network, oneway,
  cut, clock, disk`, plus `freeze` (stop a service at an exact packet of its traffic) and
  `protocol` (faults on fakes and proxies). See [references/faults.md](references/faults.md).
- `knobs:` lists settings that trade durability for speed (fsync policies, commit modes,
  replication acknowledgements): the safe value as `default`, a `set` command that changes it at
  run time, and one capital letter as `token` that is not a built-in token.
- `buggify: true` when the code carries BUGGIFY fault points from an assertion SDK.

### 8. Determinism hazards

Check every item in [references/hazards.md](references/hazards.md) (network access at start,
listening addresses, writable paths, CPU level, memory, one CPU, JVM flags, wall-clock values in
data, daemonizing, services that refuse to run as root), and report what you found and what you
did about it.

### 9. Validate, build and run

Run this command until it prints `OK`:

```sh
hydo build hydo/hydo.yaml --check
```

It checks the schema, the name, executable bit and header of every command and check file, and
the guest RAM limit. The full build also checks the backend's feature limits (the arm64 build
refuses `addr:`, `fakes:`, `params:` and others; see references/capabilities.md). Then, if the
user wants a first run now:

```sh
hydo build hydo/hydo.yaml                     # the guest image (x86_64 on Linux, arm64 on a Mac)
hydo run "disk=none dseed=1 | run"            # one run without faults: must end HYDO-ORACLE: OK
hydo run "disk=none dseed=1 | run"            # again: the fingerprint (fp=) must be identical
hydo explore --seeds 1-20 --strategy process  # schedules with faults; failures re-run and shrunk
hydo report                                   # the findings, each with a minimal schedule
hydo save <explore dir>                       # the minimal schedules -> hydo/regressions/
```

On the Mac a schedule without host tokens is `disk=none | run`. If the first run without faults
fails, the cause is the folder or a real bug: read the console files that `hydo run` names.

### 10. Report

Give the user: the folder you wrote; the inventory; each check, for review; the knobs; the
hazards and what you did about them; what you could NOT determine (unknown ready checks, unclear
data directories, missing binaries, ambiguous promises), each with what happens if it stays as
it is; and the exact build and first-run commands.
