# hydo as a product: what it offers, what is missing, and what is decided

This page describes hydo from a user's point of view: what a team gets today, the gaps we know of, and what the user decided about each gap (2026-10-24). The engineering history is in `docs/HISTORY.md`; the current state is in `AGENTS.md`.

## What a user gets today

A team describes its system in one `hydo.yaml` (`hydo/onboard/MANIFEST.md`): the services, a workload split into steps, and the invariants to check. hydo runs the whole system inside a deterministic virtual machine, so every run can be replayed bit for bit, and injects faults from a seeded schedule:

- process kills, pauses and timed kills; a freeze at an exact packet of a service's traffic;
- network delay, loss, duplication and reordering, one-way faults, and partitions between services;
- power cuts (also at the k-th disk flush), lying disks, I/O errors;
- clock jumps; changes of thread timing (tick jitter, extra preemption points, several virtual CPUs);
- FoundationDB-style fault points inside the code (BUGGIFY).

For a system whose inputs are gRPC endpoints, `hydo-grpc-gen` writes the workload: from the
`.proto` files (or server reflection) and a small hints file it builds one static client that
sends valid random requests to every RPC from a seeded stream, passes ids from earlier
responses to later calls, retries unknown outcomes like a careful client, and records each
call as ok, failed or unknown (`MANIFEST.md`, "gRPC services").

The explorer (`hydo explore`, the crate `hydo/crates/hydo-explore`) generates schedules with several strategies and sweeps, re-runs every failure to prove it reproduces, and shrinks it to a minimal schedule. Cross-service checks can read the stores with ready-made commands instead of hand-written shell (`hydo-read` prints a MySQL/PostgreSQL query, a Kafka topic, Redis or etcd keys as sorted JSON Lines; `hydo-check` compares two readings: subset, exactly-once, disjoint, count, a cache never holding a wrong value; MANIFEST.md "Reading the stores"). Assertions are properties of five kinds (always, sometimes, reachable, unreachable, eventually; `hydo/ASSERTIONS.md`). It runs on x86_64 Linux with VT-x (`hydo/dist/linux/`, the Linux release) and on Apple Silicon Macs (`hydo/dist/mac/`).

## Where the results are, and how to read them (experimental until hydo 1.0)

Every run and every exploration reports in one versioned format, so a CI job or an AI tool reads one model instead of several ad-hoc files:

- `hydo run` and `hydo repro` write `result.json` in the run directory: the schedule, the image (path and SHA-256), the engine, the verdict, the fingerprints, the findings, the run's property table and the files of the run (its consoles, `engine.log`).
- `hydo explore` writes `events.jsonl` while it runs (one record per line: a `start` record, a `run` record for every run it makes, a `shrink` record per minimal schedule, an `end` record) and `exploration.json` when it is done (configuration, summary, every failure with its re-runs, the minimal schedules, the error kinds, the property table, the liveness summary). `report.md` is the same report for people, made from `exploration.json`; `hydo report` prints it from the document.
- A finding has one shape whatever found it: `kind` (`invariant`, `assertion`, `history-check`, `error-log`), `id` (the property, check or invariant, or the error kind's signature), `status`, `message` and source-specific `details`.

The schema is `hydo/schema/results-v1.json`, generated from the Rust types (`hydo/crates/hydo-results`), and `hydo/schema/examples/results/v1/` holds documents recorded from real runs. Every record carries `"v": 1` and a `"type"`. Within version 1 only additions are made (new optional fields, new record types); renaming, removing or changing a field needs version 2, read alongside version 1. A reader must ignore fields and record types it does not know; the crate's reader does exactly that and is what hydo's own commands use. The format is experimental until hydo 1.0, and it keeps the additions-only rule from version 1 on (a test fails on any other change against the last released schema).

## An image keeps working after hydo is upgraded (M155)

An image records what shapes its runs but is not inside it, so an image built today still runs, and reproduces, after hydo is upgraded:

- Inside every image, `/etc/hydo/image.json` names the guest kernel the image was built for by its SHA-256 (with its file name, Linux version and the kernel command line hydo uses, for people) and the **fingerprint epoch** of the hydo that built it. Nothing that changes with every hydo release is in it, so two hydo versions of one epoch build the same image bytes.
- Next to the image, `hydo build` writes `<image file>.json`: the hydo version and commit that built it, the manifest's SHA-256 and the service images' digests. Every `result.json` carries the same facts in its `image` object, and the running hydo's version and epoch.
- A run boots the kernel the image names, found by its SHA-256 among the kernels this hydo has (a release's `guest/` kernels, a source tree's build outputs). When this hydo does not have it, the run is refused with a message that names the kernel, its version and the hydo version that built the image, and says to install that version's kernel or rebuild the image. An image built before M155 has no record and boots this hydo's default kernel, with a one-line warning.
- The fingerprint epoch increases whenever a change of the engine or the host controller moves fingerprints. Within one epoch, `hydo repro` requires the same fingerprint; across epochs fingerprints are not comparable, and `hydo repro` (also `--transcript`) compares the verdicts instead and says so. The explorer's result cache only answers from its own epoch.

The two records are one versioned format (`hydo/schema/image-v1.json`, crate `hydo/crates/hydo-image`, additions only within version 1).

## Two kinds of projects

**Infrastructure** (message brokers, databases, consensus systems, caches). The properties are generic: acknowledged writes survive, replicas agree, histories are consistent. hydo finds this class well (durability windows, crash consistency, members diverging, as with etcd issue 14685).

**Business software** (orders, payments, workflows, notifications). The bugs are logic bugs under failure and concurrency: a charge taken twice, an order stuck forever, a notification sent after an acknowledgement, a retry that duplicates an effect, services that disagree about one entity. hydo can find these, but today someone has to write the workload and the checks by hand.

## Gaps and decisions

| # | Gap | Decision (2026-10-24) |
|---|---|---|
| 1 | **Test composer.** Users write small commands ("place order", "refund", "webhook arrives"); hydo composes them into random sequences and concurrent mixes, instead of a fixed list of steps. | **Approved; built** (branch `test-composer`, M86, not merged): a plan's `test:` section or the `hydo/` folder, composed in the guest from the schedule token `W=`, shrunk by the explorer; the Rust SDK `hydo-test` writes the plan (see below). |
| 2 | **Interactive debugging of a failing run** (a shell inside the VM at any moment, a merged timeline of all services' logs). | Not needed now: an AI agent does the analysis of a failing run. |
| 3 | **Ready-made checkers:** a history recorder plus linearizability and isolation checkers (Jepsen Knossos/Porcupine/Elle style) for infrastructure; "exactly once / no side effect after X" checkers for business software. | **Done:** history checkers (M87) and store readers with set-style checks (M88); the business side-effect checks with item 4 (M89). |
| 4 | **Faults at the protocol level and fake outside services:** HTTP 500/429, slow answers, dropped answers, duplicated or reordered webhooks, gRPC deadlines; fakes that record every side effect. | **Done:** fake outside services, HTTP-level faults and side-effect checks (M89); gRPC-level faults through a streaming proxy (M90). |
| 5 | **Fast-forwarding virtual time** (run 30 days of business time in minutes). | Not decided yet. |
| 6a | **Assertion SDKs in more languages.** | **Approved.** Rust (M82) and JavaScript (M80) merged; Python done on branch `python-sdk` (M86), not merged yet. |
| 6b | **Docker Compose import** (a starting `hydo.yaml` from `docker-compose.yaml`). | **Approved; in progress** (branch `compose-import`). |
| 6c | **CI mode** (below). | Document only; to be discussed later. |
| 7 | **Scale and where it runs** (below). | Document only; not now. |
| 8 | **Generated gRPC drivers:** no hand-written client per RPC for a gRPC system. | **Approved; done** (branch `grpc-drivers`, `hydo/grpcgen/`; HISTORY Milestone 91). The generated drivers are test-composer commands (one `parallel_driver` per RPC); records are `HYDO-HIST` lines. |

## How a team uses hydo (agreed direction, 2026-10-24)

hydo is used from the team's own repository. Nobody checks out hydo's repository to test their system.

**Two parts.** The *engine* is installed once per machine, like Docker, because it needs a kernel module (Linux) or Hypervisor.framework (Mac) and its own guest kernel; no package manager can deliver that. The *SDK* is an ordinary library dependency of the team's code (Go now; Rust in progress), and it is the part teams work with. Its intended roles:

1. **Assertions inside the services** (always, sometimes, reachable, unreachable, eventually, BUGGIFY fault points). Exists for Go; Rust in progress.
2. **The test written in the team's language:** operations (`place_order`, `refund`, `webhook_arrives`) and properties ("never charged twice", "an order acknowledged is never lost") as ordinary functions, which hydo composes into random sequences and concurrent mixes. This is gap 1 (the test composer) expressed as an SDK; it replaces shell scripts for workloads and checks. **Built for Rust** (`sdk/rust-test`, crate `hydo-test`, branch `test-composer`, M86): a `system()` builder plus `#[driver(parallel)]` / `#[check(eventually, within = 20)]` functions in one binary; `<bin> plan` writes the plan on the host, `<bin> driver|check <name>` runs in the guest. Other languages: not yet (the plan format is language-neutral, so each SDK only has to write the same JSON).
3. **A runner command** in the language's tooling (for example `cargo hydo`, `go run .../hydo`) that builds the services, builds the guest and runs or explores by calling the installed engine, the way `cargo fuzz` relies on the compiler's fuzzing support. **`cargo hydo` built** (M86): it builds the test binary for the host and as a static musl guest binary, writes `.hydo/plan.json`, and calls the installed `hydo build|run|explore|repro|save`. Building the services from source (Compose `build:`) is not part of it yet.

**Assertion SDKs (role 1).** Every SDK writes the same console records as the Go SDK, byte for byte, with the same deduplication and BUGGIFY decisions (`hydo/ASSERTIONS.md`):

| Language | Where | State | Proof of the same output |
|---|---|---|---|
| Go | `sdk/go` (module `github.com/kakoc/hydo/sdk/go`, packages `hydoassert` and `hydohist`; `hydo/stack/hydoassert` is the order service's frozen copy, M159) | on main (M38, M56, M59); published with a release as the tag `sdk/go/v<version>` (docs/PUBLISHING.md) | `cases_test.go` reads the shared cases (`sdk/cases`), Go 1.22 or later, Go 1.26.5 for every case |
| shell (busybox sh) | `hydo/stack/hydoassert/hydo_assert.sh` | on main; no deduplication, no BUGGIFY | |
| Rust | `sdk/rust` (crate `hydo-assert`, with `hydo-test`, `hydo-test-macros` and `cargo-hydo`; for crates.io, published with a release, docs/PUBLISHING.md) | on main (M82); std only, call-site macros with file:line, `declare!`, sync and runtime-agnostic async `eventually`, features `noop` and `serde_json` | the reference (M110): its test `tests/cases` writes the shared cases `sdk/cases`, which equal Go 1.26.5's bytes |
| Python (3.8+) | `sdk/python` (distribution `hydo-assert`, import `hydo_assert`; for PyPI, published with a release, docs/PUBLISHING.md) | on main (M83); standard library only, thread- and asyncio-safe; example `hydo/onboard/examples/python-kv` on the Mac backend | `tests/test_cases.py` reads the shared cases, Python 3.8 and 3.15 |
| JavaScript / TypeScript (Node.js) | `sdk/js` (package `hydo-assert`; for npm, published with a release, docs/PUBLISHING.md) | on main (M80); CommonJS and ESM, types, no dependencies | `test/cases.test.js` reads the shared cases, both entries |

**The `hydo/` folder** (agreed) holds everything hydo needs in the team's repository, next to `docker-compose.yaml`:

```
hydo/
  hydo.yaml          services (imported from Docker Compose), allowed faults, guest settings
  workload/          commands: <kind>_<name> executables (first_, parallel_driver_, serial_driver_,
                     singleton_driver_, anytime_, eventually_, finally_; helper_* ignored)
  checks/            properties: final_<name>, always_<name>, eventually_<name>
  regressions/       <name>.schedule: saved minimal schedules; `hydo repro --regressions` replays them
.hydo/               build cache (git-ignored); `hydo build` writes here for hydo/hydo.yaml
```

Built (M86, branch `test-composer`): `hydo-build` reads the three directories next to the manifest; a code-first SDK writes the same content as a plan file (`.hydo/plan.json`) instead. The format and the kinds are in `hydo/onboard/MANIFEST.md` "The test composer".

**Onboarding** is a skill run from the repository root: it reads `docker-compose.yaml` (the importer, `compose-import`), asks which services to test and what must never happen, writes `hydo/`, builds, runs a first exploration, and explains the failures it finds. Teams use AI tools from different vendors, so the skill follows the open Agent Skills format (https://agentskills.io/specification) instead of one vendor's plugin format (decided 2026-10-26; HISTORY Milestone 93). It ships in both releases (`skill/hydo-onboard/`) and reaches every tool in one of three ways:

- `hydo init` writes the `hydo/` skeleton and a short section in the repository's `AGENTS.md` (the instruction file most tools read): "this repository is tested with hydo; before changing `hydo/`, run `hydo guide`".
- `hydo guide` prints the skill's instructions, so any tool can be told "run `hydo guide` and follow it"; `hydo guide <topic>` prints one reference page and `hydo guide manifest` the schema.
- `hydo skill install --agent claude|codex|cursor|copilot|gemini|all` copies the skill where the tool discovers skills by itself: `.claude/skills/` for Claude Code, `.agents/skills/` for Codex, Cursor, GitHub Copilot and Gemini CLI (or `--user` for the home directory, `--to DIR` for any other place). Vercel's `skills` CLI (`npx skills add`) is a widely used alternative that installs into the same directories.

**Missing for this to work:** building Compose `build:` services from source and importing the images; the SDK's test definitions and runner in languages other than Rust, and `System::from_compose` in the Rust one; release builds that ship every feature the skill describes (HISTORY Milestone 93, "Not done"). CI is skipped for now (see "CI mode"). (Done in M86: the folder, the version pin `hydo_version:`, regressions, the Rust test definitions and `cargo hydo`. Done in M93: the skill in a vendor-neutral format, `hydo init`, `hydo guide`, `hydo skill install`.)

### Code first (decided, 2026-10-24)

The primary way a team uses hydo is **one program in the team's language, written with the SDK** (Rust first). Hand-written `hydo.yaml` stays supported for simple cases, such as the file the Docker Compose importer emits.

```rust
use hydo::prelude::*;

fn system() -> System {
    System::from_compose("docker-compose.yaml")
        .service("payments", |s| s.fault_freeze().logs(errors_as_findings()))
        .fake_http("stripe", |f| f.route("POST /charges", ok_json(/* ... */)))
        .faults(&[Kill, Pause, Network, PowerCut, Clock])
        .known_errors("hydo/known-errors.yaml")
}

#[hydo::driver(parallel)]
fn place_order(ctx: &Ctx) -> Result<()> { /* call the API, record the outcome */ }

#[hydo::check(always)]
fn never_charged_twice(ctx: &Ctx) -> Result<()> { /* read the fake's records */ }

#[hydo::check(eventually)]
fn every_order_settles(ctx: &Ctx) -> Result<()> { /* ... */ }
```

`cargo hydo explore` runs `system()` **on the host, at build time**, to produce a plain **plan file**, builds the guest from that plan, and runs the same binary **inside the guest** for the drivers and checks as hydo's composer schedules them. Rules that follow:

- The plan file (today's manifest and `hydo/` folder format) is the engine's only input: complete, documented, versioned, language-neutral. Every SDK (Rust, Go, later others) produces the same plan, so the engine is never rewritten per language.
- Setup code never runs inside the guest. Only drivers and checks do, so nothing on the host (its time, files, randomness) can leak into a deterministic run.
- A hand-written plan is still valid input; the SDK is the recommended way, not the only one.

### Error logs as findings (proposed, 2026-10-24)

A team can declare which log lines count as a finding (a level, a field such as `actionable=true`, or a pattern). Each service's log is collected with its name; each matching line is reduced to a signature (timestamps, ids, numbers removed) so one root cause is one finding; every new signature gets a reproducer shrunk to the smallest schedule that still produces that line. Triage: "expected" entries go to `hydo/known-errors.yaml` with a reason and who decided, optionally scoped to faults or time windows ("connection refused to mysql is expected only while mysql is down"); "bug" entries become reproducers in `hydo/regressions/`. An AI skill proposes the triage; the user decides. Not built yet; it is also the core of the CI mode's "new findings only".

### CI mode (to be discussed)

What a CI integration needs: run hydo on every merge request or nightly; compare against a recorded baseline; deduplicate findings across runs (one finding per root cause, not per schedule); report only new findings, with the minimal schedule and the command that reproduces it; and check that a fixed bug stays fixed by re-running its saved reproducer on every build. Open questions: how much exploration budget a CI run gets, where images are cached, and how findings are tracked over time.

### Where hydo runs, and scale (not now)

Users are expected to run hydo locally (a developer's Linux machine or Mac) and in CI such as GitHub Actions. That has consequences to keep in mind:

- The Linux backend needs bare metal or a VM with nested VT-x, a kernel module, and root (or the optional udev rule) for the device. Hosted CI runners are usually VMs: nested virtualisation and loading a kernel module may not be allowed, and without a PMU the exact branch clock is not available (the exit-count clock is used instead; fingerprints then differ from PMU-clock runs).
- The Mac backend needs Apple Silicon and no root.
- **Where a failure replays.** A failure replays exactly with the same engine, the same image and the same hydo version. The two engines run different machines (hvx: x86_64 guests, an x86_64 kernel and builds, a clock that counts guest branches; the Mac engine: arm64 guests, an arm64 kernel and builds, a clock that counts VM exits), so a failure found on Linux cannot be replayed on a Mac or the other way round: the same schedule string runs on both, but it is a new run with other fingerprints and maybe another outcome. Every recorded run so far was made on one Intel machine and one M2 Mac; replay on another machine of the same kind should hold (CPUID and the model-specific registers are emulated, not passed through) but has not been tested. A Linux CI run therefore needs a self-hosted bare-metal runner, and a failure it finds replays on another Intel Linux machine with VT-x, not on a developer's Mac.
- The search budget is limited by one machine: today tens to a few hundred runs per hour on a 4-core machine. Spreading the explorer over several machines would multiply the budget, but is not planned now.

### Known blind spots (to be discussed later)

- **Performance problems:** time inside the VM is virtual, so slowness and throughput regressions are not measured.
- **Long-running leaks** (memory, file descriptors, disk growth): found only if the workload runs long enough.
- **Guests larger than 3 GiB of RAM:** not supported yet (the PCI-hole split is not implemented).
- **AMD processors:** not supported by the Linux backend (VT-x only).
- **AVX-512 and some CPU features:** not offered to the guest (x86-64-v3 is the highest level, opt-in).
- **On the Mac:** a pure memory-flag spin in user space with no VM exits cannot be interrupted deterministically, and windows of plain work with no exits are out of reach of timing changes.
- **Bugs that need several machines' kernels to differ** (different kernel versions, real hardware failures beyond disks and network) are out of scope; all services share one guest kernel.
- **Security problems and user interfaces** are not what hydo tests.
