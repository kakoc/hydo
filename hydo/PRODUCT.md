# hydo as a product

This page describes hydo from a user's point of view: what a team gets, where it runs, and the
gaps that are known. It is an explanation, not a reference. The current values are in the
section `Current state` of `AGENTS.md`. Each open gap has a numbered entry in its section `Open
items`, and this page names that number instead of repeating the entry. The evidence for
every feature is in `docs/HISTORY.md`, by milestone.

## What a release contains

hydo 0.3.2 contains every feature on this page unless the page says
otherwise. The releases are:

- the Linux release, with the hvx engine and the KVM engine;
- the Mac release, signed and notarized, with the Mac engine;
- `cargo install hydo`, and the Homebrew formula `kakoc/tap/hydo` for the Mac.

`docs/RELEASING.md` says how a release is built and checked, and `hydo/dist/releases.txt` pins
every published archive by its SHA-256.

## What a team gets

A team describes its system in its own repository, in a `hydo/` folder. hydo runs the whole system
in a deterministic virtual machine, so the same image and the same schedule give the same run, bit
for bit. A schedule is one line of text that names the faults of one run.

```text
hydo/
  hydo.yaml       the services, the allowed faults, the guest settings
  workload/       the workload as commands: parallel_driver_<name>, serial_driver_<name>, ...
  checks/         the properties: final_<name>, always_<name>, eventually_<name>
  regressions/    saved minimal schedules; `hydo repro --regressions` replays them
```

The parts of that description:

- **Services.** `hydo compose` writes `hydo.yaml` from a Docker Compose file. It pins every image
  by digest. The next release also builds the services that have `build:` with Docker
  (Milestone 214, on `main`; `hydo guide workflow`). `hydo/onboard/MANIFEST.md` is the reference of every key.
- **The workload.** The test composer runs the commands in `hydo/workload/` in seeded sequences
  and concurrent mixes (Milestone 86, `hydo guide composer`). For a gRPC system on Linux,
  `hydo-grpc-gen` writes those commands from the `.proto` files (Milestone 91). For a SQL
  database, hydo has ready-made workloads and checks (Milestone 182).
- **The checks.** hydo runs the commands in `hydo/checks/`. It also has these checks of its own:
  - history checkers for linearizability and isolation (Milestone 87);
  - readers of the stores, `hydo-read` and `hydo-check` (Milestone 88);
  - messaging checks (Milestone 196);
  - error logs as findings (Milestone 94).
- **Assertions inside the services.** The SDKs for Rust, Go, Python, and JavaScript write the same
  records (`hydo guide sdks`). Each release publishes them as `hydo-assert` on crates.io, PyPI and
  npm, and as the Go module `github.com/kakoc/hydo/sdk/go`.

hydo injects these faults from a seed:

- process kills, pauses and timed kills, and a freeze of a service at an exact packet;
- network delay, loss, duplication and reordering, one-way faults, and partitions, also over a
  group of services (Milestone 195);
- power cuts, also at the k-th disk flush, lying disks, I/O errors, and storage faults of one
  service (Milestone 183);
- clock jumps, and changes of thread timing (tick jitter, extra preemption points, several
  virtual CPUs on hvx);
- HTTP and gRPC faults from fake outside services and proxies (Milestones 89 and 90);
- fault points inside the code (BUGGIFY), which the SDKs turn on from the schedule.

To find and explain failures:

- `hydo explore` draws schedules, runs each one, runs every failure again to prove that it
  reproduces, and shrinks it to a minimal schedule.
- `hydo diff` and `hydo explain` show where two runs first differ (Milestone 165).
- `hydo inspect` and the diagnosis in `result.json` show why a run failed. `hydo inspect` brings a
  recorded run back to one of its stops and shows its files, logs, processes, and sockets there
  (Milestone 178).
- `hydo ci run` runs hydo in a team's continuous integration. It replays each finding on the base
  commit, to report whether the change made it (Milestone 197, `hydo guide ci`).

The onboarding skill writes the `hydo/` folder with an AI coding tool. It ships in both releases.
`hydo skill install` copies it where a tool reads skills, and `hydo guide` prints it for any other
tool (Milestone 93).

## Results that tools can read

Every run and every exploration writes one versioned format, the results format (schema
`hydo/schema/results-v1.json`). `hydo run` writes `result.json`, and `hydo explore` writes
`events.jsonl` while it runs and `exploration.json` at the end. `report.md` is the same report for
people. Version 1 takes only additions, and a reader must ignore the fields that it does not know.
The format is experimental until hydo 1.0, and `hydo-schema-compat` checks the additions-only rule
against the last released schema.

## An image keeps working after an upgrade

An image records the guest kernel it was built for and the fingerprint epoch of the hydo that
built it (Milestone 155). A fingerprint is a hash of the machine's whole final state, and the
epoch names a generation of hydo versions whose fingerprints compare. A later hydo boots the
kernel that the image names, or names the version to install. An image built at an earlier epoch
keeps that epoch's defaults. Within one epoch, `hydo repro` requires the
same fingerprint. Across epochs it compares only the verdicts, and says so.

## Where hydo runs, and where a failure replays

hydo has three engines, and each one gives its own fingerprints:

| Engine | Where it runs | What it needs |
|---|---|---|
| hvx | x86_64 Linux on Intel CPUs with VT-x, on bare metal or with nested VT-x | the `hvx.ko` kernel module, and root or the udev rule |
| KVM | x86_64 Linux with `/dev/kvm`, on Intel and AMD, also on GitHub's hosted runners | no module and no root |
| Mac | Apple Silicon Macs | no root |

A failure replays exactly with the same engine, the same image, and a hydo of the same epoch. The
same schedule also runs on the other engines, but it is a new run with other fingerprints. A
failure that hvx or KVM found runs x86_64 guests, so it does not replay on a Mac, which runs arm64
guests. KVM's fingerprints also differ between Intel and AMD CPUs, and on Intel between CPU models,
but the verdicts are the same.

## Known gaps

These gaps are what a user meets first. Each names its open item in `AGENTS.md`.

- **Code-first tests in other languages.** Only Rust has test definitions in code (`hydo-test`)
  and the runner `cargo hydo` (Milestone 86). They are not published until the user decides
  (open item 48), and `cargo hydo` has not run through a release (open item 19). The language
  policy in `AGENTS.md` adds no other language for them.
- **Several virtual CPUs.** hvx runs up to 4 virtual CPUs (open item 4). The KVM engine runs one
  (open item 45).
- **CPU features.** The guest gets x86-64-v3 at most, without AVX-512 (open item 5).
- **Spins without VM exits.** On the Mac, a spin on a memory flag in user space cannot be
  interrupted deterministically (open item 13). On KVM, a loop that counts in memory is not
  proven (open item 45).
- **Docker Compose import.** All Compose networks share one bridge, and a service has no
  hostname or PID namespace of its own (open item 18).
- **Stops of a process for some guest time.** No fault token stops a process the way a CPU limit
  does (open item 80).
- **Continuous integration.** Stage 2 (history, accepted findings, budgets, sharding) is not
  built, and the GitLab template has not run (open item 67).

These are outside what hydo tests, by design:

- Performance and throughput: time in the guest is virtual, so slowness is not measured.
- Leaks of memory, file descriptors or disk space: found only when the workload runs long
  enough.
- Bugs that need different kernels or real hardware failures: all services share one guest
  kernel, and only disks and the network fail.
- Security problems and user interfaces.

Two directions are not decided: fast-forwarding virtual time (running days of business time in
minutes), and spreading one exploration over several machines.
