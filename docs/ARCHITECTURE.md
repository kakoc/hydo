# Architecture

This page explains how hydo is built: the parts, how one run works, why a run repeats, and where
each part is in the repository. To use hydo, start with [README.md](../README.md). To build and
test it, read [CONTRIBUTING.md](../CONTRIBUTING.md).

## The parts

```
 your project: hydo.yaml or a hydo/ folder
        |
        v
 +-------------------- hydo, the program on the host (Rust) --------------------+
 | hydo-cli: the hydo command          hydo-build: makes the image              |
 | hydo-explore: many schedules        hydo-control: drives the guest           |
 | hydo-schedule: schedule language    hydo-manifest: project format            |
 | hydo-results: run and exploration documents                                  |
 | hydo-engine: one interface to the three engines                              |
 +-----------+--------------------------+--------------------------+------------+
             |                          |                          |
             v                          v                          v
   hvx (Linux, Intel)          hvm (Apple Silicon)        KVM (Linux, Intel or AMD)
   kernel module hvx.ko        Hypervisor.framework       stock /dev/kvm
   + hydo-hvx (Rust)           hydo-hvm (Rust)            hydo-kvm (Rust)
             |                          |                          |
             +--------------------------+--------------------------+
                                        |
                                        v
 +------------------------ the guest: one virtual machine ----------------------+
 | Linux, with hydo-agent as its first process                                  |
 | your services, your workload commands, your checks                           |
 +------------------------------------------------------------------------------+
```

The host program compiles a project's `hydo.yaml` into a guest image, runs schedules on an engine,
and checks the results. It is one Rust workspace in [hydo/crates/](../hydo/crates/).

An engine runs the guest deterministically and applies the faults. Each engine reads the same
schedule language and gives its own fingerprints. The KVM engine reuses code of the other two.
From `hydo-hvx` it takes the channel that carries the host controller's commands, and the loader
of the guest kernel. From `hydo-hvm` it takes the virtio disk and network devices.

- hvx is the project's own Intel VT-x hypervisor, a Linux kernel module
  ([hv/hvx/hvx.c](../hv/hvx/hvx.c)). It programs the processor's VT-x virtualization itself and
  does not use KVM.
- hvm runs arm64 Linux on an Apple Silicon Mac through Hypervisor.framework, without root.
- The KVM engine runs the x86_64 images on the KVM that comes with Linux. It needs neither the
  module nor root.

## Why a run repeats

A run repeats when the guest sees the same inputs at the same points of its execution. hydo
controls each input:

- Time. The guest gets time only from the engine's clock. A VM exit is a return from the guest to
  the hypervisor. Every engine's clock counts VM exits, and hvx can also count branches (below).
  - hvx traps the guest's reads of the time-stamp counter (`RDTSC`).
  - hvm traps the guest's reads of the ARM counter register `CNTPCT` and answers with its own
    count. The guest kernel is patched to read only that register.
  - The KVM engine boots a guest kernel that never reads the time-stamp counter
    ([hv/kvm/linux/](../hv/kvm/linux/)).
- Interrupts and host commands. The engine delivers timer interrupts, and the host's commands to
  the guest, only at fixed points of its clock.
- Device input. The disks and the network cards are the engine's own device models. The engine
  handles each request when the guest's VM exit reaches it, so the order of requests and the
  time they complete follow the guest's clock. Disk faults are a function of the
  schedule's seed and the request's number.
- Randomness. The engines do not advertise the processor's random-number instructions to the
  guest. A small program in the image, `hvx_seedrng`, seeds the guest kernel's random pool from
  a fixed value. On AMD CPUs, KVM cannot make the instructions `RDRAND`, `RDSEED`, and TSX fault,
  so a guest program that uses them without checking the CPU's feature list sees the host.

The engines differ in what they give up:

- hvx has the PMU branch clock: the CPU's performance-monitoring unit counts the guest's retired
  conditional branches, so a tick lands at an exact instruction. With it, hvx can also run
  several virtual CPUs in exact turns.
- hvm and the KVM engine count VM exits only, and give the guest one virtual CPU. Code
  that runs without a VM exit takes no guest time. A loop that waits on memory without exits is
  found by a spin observer and resolved at a point taken from the loop's own state.

## How one run works

1. `hydo build` turns `hydo.yaml` into an image. On x86_64 the image is an ext4 disk. On arm64 it
   is an initramfs (a root file system that the kernel loads into RAM) and a data disk. The same
   `hydo.yaml` gives the same bytes on any machine.
2. The engine boots the image once. The guest's first process is `hydo-agent`, a small Rust
   program that only carries out commands. An image with this agent is an agent image; every
   image that `hydo build` makes is one. During this boot the engine takes two snapshots. It takes
   S0 when the root file system is mounted, and S1 when the services are ready.
3. Every schedule starts in a fresh virtual machine restored from S1. The host controller
   (`hydo-control`, inside `hydo`) sends the agent its commands. It runs the workload, injects
   each fault of the schedule at its step, and runs the checks. Every command lands at a fixed
   point of the guest's clock.
4. After a power cut, only the data that reached the disk goes into a recovery virtual machine
   restored from S0. The checks run there.
5. The run ends with a verdict and a fingerprint, a hash of the guest's whole final state. The
   verdict is `OK` (every invariant held), `VIOLATION` (an invariant failed), or `ERROR` (the run
   itself failed, for example a service did not start). The run directory keeps the consoles, the
   control transcript (every command and the point where it was sent), and `result.json`.

`hydo explore` repeats this for many schedules drawn from seeds. It runs each failure again to
check that it reproduces, and shrinks it to a minimal schedule.

A change to an engine, the host controller, the agent, or a guest kernel can move every
fingerprint. The fingerprint epoch (`hydo_image::EPOCH`) numbers these generations. Runs of one
epoch compare fingerprints; runs of different epochs compare only verdicts.
[hydo/onboard/recorded.json](../hydo/onboard/recorded.json) lists every recorded example run with
its fingerprint, and `hydo-bench rebaseline` checks them.

The design of the agent and the host controller is in
[docs/design/guest-control.md](design/guest-control.md). The KVM engine's design is in
[docs/design/kvm-engine.md](design/kvm-engine.md).

## The Rust workspace

| Crate | Responsibility |
|---|---|
| `hydo-cli` | The `hydo` command (package name `hydo`). |
| `hydo-schedule` | The schedule language as types: parse and print a schedule byte for byte. |
| `hydo-manifest` | The project format: `hydo.yaml`, a plan's `test:` section, and the `hydo/` folder. |
| `hydo-build` | The image builder for x86_64 and arm64. It also packs Docker or OCI images. |
| `hydo-image` | The image record: the guest kernel by sha256 and the fingerprint epoch. |
| `hydo-control` | The host controller that drives an agent image through a schedule. |
| `hydo-agent` | The guest's first process. It has its own Cargo workspace, and the image builder accepts only the build with a recorded sha256. |
| `hydo-agent-proto` | The protocol between the host controller and `hydo-agent`. |
| `hydo-engine` | One interface to the run engines. |
| `hydo-engine-proto` | The calls between hydo and its engines as Rust types. |
| `hydo-hvx`, `hydo-hvm`, `hydo-kvm` | The three run engines. |
| `hydo-explore` | The explorer: strategies, sweeps, re-runs, shrinking, and reports. |
| `hydo-composer` | The test composer, which composes workload commands into seeded rounds. |
| `hydo-history` | Linearizability and list-append checks of recorded histories. |
| `hydo-errors` | Error log lines as findings, with a reviewed list of known errors. |
| `hydo-results` | The results format: run and exploration documents and their JSON Schema. |
| `hydo-schema-compat` | The check that a format only gains additions after its release. |
| `hydo-config` | Every environment variable that hydo reads (`INVENTORY.md`). |
| `hydo-compose` | Imports a `docker-compose.yaml` as a starting `hydo.yaml`. |
| `hydo-agents` | `hydo guide`, `hydo skill install`, and `hydo init` for AI coding tools (not the guest agent). |
| `hydo-world` | The guest program that serves fake outside services and fault proxies. |
| `hydo-read`, `hydo-sql` | Guest programs that read stores and run SQL workloads (own workspaces). |
| `hydo-grpc-gen`, `hydo-grpc` | The gRPC driver generator and the guest's gRPC client. |
| `hydo-bench` | The bug benchmark runner and the re-baseline tool (not shipped). |
| `hydo-tools` | Build helpers for Nix archives, cpio extraction, and ELF library lists (not shipped). |
| `hydo-release` | The releases' third-party notices and sources archives (not shipped). |

A fake outside service is a program in the guest that stands in for a service your system calls,
such as a payment API. A fault proxy sits in front of a real service and changes its answers.
Both are `hydo-world`, configured by `fakes:` and `proxies:` in `hydo.yaml`.

The SDK crates (`sdk/rust`, `sdk/rust-test`) and the Rust examples are not workspace members. They
are compiled into guest programs whose bytes are inside recorded images. Cargo derives symbol
hashes from a package's path and lock file, so making them members would change those bytes.

## Repository layout

| Path | Contents |
|---|---|
| [hv/hvx/](../hv/hvx/) | The kernel module (`hvx.c`, its ABI `hvx_abi.h`), its tests and harnesses, the regression suite `deploy-metal.sh`, the hand-made guest images (`images/`), and the module's safety net (`safety/`, `SAFETY.md`). |
| [hv/hvm/](../hv/hvm/) | The Mac engine's hardware probes, the arm64 guest kernels' build scripts, and notes. |
| [hv/kvm/](../hv/kvm/) | The KVM engine's guest kernel patch and its check script. |
| [hydo/crates/](../hydo/crates/) | The Rust workspace. |
| [hydo/onboard/](../hydo/onboard/) | The project format's reference `MANIFEST.md`, the examples, and `recorded.json`. |
| [hydo/guide/](../hydo/guide/) | The guide that `hydo guide <topic>` prints. |
| [hydo/schema/](../hydo/schema/) | The generated JSON Schemas and their released copies. |
| [hydo/bench/](../hydo/bench/) | The bug benchmark: planted defects in the example order service. |
| [hydo/stack/](../hydo/stack/) | The example order service (Go) of the stack example and the benchmark. |
| [hydo/dist/](../hydo/dist/) | Release packaging for Linux and the Mac, and the release index `releases.txt`. |
| [sdk/](../sdk/) | The assertion SDKs: Rust (the reference), Go, Python, and JavaScript. |
| [skills/](../skills/) | `hydo-onboard` (for users) and `hydo-docs` (for contributors). |
| [docs/](.) | `HISTORY.md` (every milestone with its evidence), `REBASELINE.md` (recorded fingerprints), `BUILD-PINS.md`, `CI.md`, the release documents, `STYLE.md`, and `design/`. |
| [ci/](../ci/) | The scripts that CI runs. |

## Tests are the specification

The formats between the parts are defined by their code and their tests, not by prose:

- The schedule language is its parser in [hydo-schedule](../hydo/crates/hydo-schedule/), with
  1,667 example schedules in
  [tests/sample.txt](../hydo/crates/hydo-schedule/tests/sample.txt).
- [hydo/onboard/MANIFEST.md](../hydo/onboard/MANIFEST.md) explains the project format, and its
  schema [hydo/schema/project-v1.json](../hydo/schema/project-v1.json) decides it.
- The SDKs share example files in [sdk/cases/](../sdk/cases/) that the Rust SDK's tests write.
  The Go, Python, and JavaScript SDKs must reproduce each file byte for byte.
- The project, results, image, and agent-protocol formats each have a generated JSON Schema in
  [hydo/schema/](../hydo/schema/). A test fails when a schema changes by more than additions
  since its released copy in [hydo/schema/released/](../hydo/schema/released/).
