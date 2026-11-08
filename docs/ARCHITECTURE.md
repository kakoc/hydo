# Architecture

This page explains how hydo is built: the parts, how one run works, and where each part is in the
repository. To use hydo, start with `README.md`. To build and test it, read `CONTRIBUTING.md`.

## The parts

```
 your project: hydo.yaml or a hydo/ folder
        |
        v
 +----------------------- hydo (the product, Rust) -----------------------+
 | hydo-cli: the hydo command                                              |
 | hydo-build: the image     hydo-explore: many schedules                  |
 | hydo-control: the host controller, which drives the guest              |
 | hydo-schedule: the schedule language    hydo-manifest: the project format|
 | hydo-engine: one interface to the three engines                        |
 +---------+----------------------+-----------------------+---------------+
           |                      |                       |
           v                      v                       v
   hvx (Linux, Intel)      hvm (Apple Silicon)     KVM (Linux, x86_64)
   kernel module hvx.ko    Hypervisor.framework    stock /dev/kvm
   + hydo-hvx (Rust)       hydo-hvm (Rust)         hydo-kvm (Rust)
```

The product compiles a project description into a guest image, runs schedules on an engine, and
checks the results. It is one Rust workspace in `hydo/crates/`.

An engine runs the guest deterministically and applies the faults. The three engines share no
code. Each one reads the same schedule language and gives its own fingerprints. A VM exit is a
return from the guest to the hypervisor. Each engine's clock counts events that happen at the
same point in every run, such as VM exits:

- hvx is the project's own Intel VT-x hypervisor, a Linux kernel module (`hv/hvx/hvx.c`). It owns
  the VMCS, the processor's control structure for a guest. So it controls the guest's reads of
  the time-stamp counter (`RDTSC`), its interrupts, and every VM exit. Its clock counts VM exits.
  When the CPU has a working performance-monitoring unit, the clock can count the guest's retired
  conditional branches instead.
- hvm runs arm64 Linux on an Apple Silicon Mac through Hypervisor.framework, without root. Its
  clock counts VM exits.
- The KVM engine runs the x86_64 images on the KVM that comes with Linux, without the module and
  without root. Its clock counts the VM exits that reach the engine. It boots a guest kernel that
  never reads the time-stamp counter (`hv/kvm/linux/`).

## How one run works

1. `hydo build` turns `hydo.yaml` into an image. On x86_64 the image is an ext4 disk. On arm64 it
   is an initramfs (a root file system that the kernel loads into RAM) and a data disk. The same manifest gives the same bytes on any machine.
2. The engine boots the image. The guest's first process is `hydo-agent`, a small Rust program
   that only carries out commands.
3. The host controller (`hydo-control`, inside `hydo`) sends those commands. It starts the
   services, runs the workload, injects each fault of the schedule at its step, and runs the
   checks. The engine stops the guest at defined points of the guest's clock, so each command
   lands at the same point in every run.
4. The engine takes two snapshots during the first boot. It takes S0 when the root file system is
   mounted, and S1 when the services are ready. Each later schedule starts in a fresh VM restored from S1.
   After a power cut, only the flushed part of the data disk goes into a recovery VM restored
   from S0.
5. The run ends with a verdict (`OK`, `VIOLATION` or `ERROR`) and a fingerprint, a hash of the
   guest's whole final state. The run directory keeps the consoles, the control transcript (every
   command and the point where it was sent), and `result.json`.

`hydo explore` repeats this for many schedules drawn from seeds. It runs each failure again to
check that it reproduces, and shrinks it to a minimal schedule.

The design of the guest agent and the host controller is in `docs/design/guest-control.md`. The
KVM engine's design is in `docs/design/kvm-engine.md`.

## The Rust workspace

Each crate has one responsibility. The table lists the crates in `hydo/crates/`.

| Crate | Responsibility |
|---|---|
| `hydo-cli` | The `hydo` command (package name `hydo`). |
| `hydo-schedule` | The schedule language as types: parse and print a schedule byte for byte. |
| `hydo-manifest` | The project format: `hydo.yaml`, a plan's `test:` section, and the `hydo/` folder. |
| `hydo-build` | The image builder for x86_64 and arm64. It also packs services from Docker or OCI images. |
| `hydo-image` | The image record: the guest kernel by sha256 and the fingerprint epoch. |
| `hydo-control` | The host controller that drives an agent image through a schedule. |
| `hydo-agent` | The guest's first process (its own Cargo workspace; its bytes are pinned). |
| `hydo-agent-proto` | The protocol between the host controller and `hydo-agent`. |
| `hydo-engine` | One interface to the run engines. |
| `hydo-engine-proto` | The calls between hydo and its engines as Rust types. |
| `hydo-hvx`, `hydo-hvm`, `hydo-kvm` | The three run engines. |
| `hydo-explore` | The explorer: strategies, sweeps, re-runs, shrinking, and reports. |
| `hydo-composer` | The test composer: commands composed into rounds, and saved regressions. |
| `hydo-history` | Linearizability and list-append checks of recorded histories. |
| `hydo-errors` | Error log lines as findings, with a reviewed list of known errors. |
| `hydo-results` | The results format: run and exploration documents and their JSON Schema. |
| `hydo-schema-compat` | The check that a format only gains additions after its release. |
| `hydo-config` | Every environment variable hydo reads (`INVENTORY.md`). |
| `hydo-compose` | Imports a `docker-compose.yaml` as a starting `hydo.yaml`. |
| `hydo-agents` | `hydo guide`, `hydo skill install`, and `hydo init`, for AI coding agents. |
| `hydo-world` | The guest program that serves fake outside services and fault proxies. |
| `hydo-read`, `hydo-sql` | Guest programs that read stores and run SQL workloads (own workspaces). |
| `hydo-grpc-gen`, `hydo-grpc` | The gRPC driver generator and the guest's gRPC client. |
| `hydo-bench` | The bug benchmark runner and the re-baseline tool (not shipped). |
| `hydo-tools` | Build helpers for Nix archives without Nix, cpio extraction, and ELF library lists (not shipped). |
| `hydo-release` | The releases' third-party notices and sources archives (not shipped). |

The SDK crates (`sdk/rust`, `sdk/rust-test`) and the Rust examples are not workspace members. They
are compiled into guest programs whose bytes are inside recorded images. Cargo derives symbol
hashes from a package's path and lock file, so making them members would change those bytes.

## Repository layout

| Path | Contents |
|---|---|
| `hv/hvx/` | The kernel module (`hvx.c`, its ABI `hvx_abi.h`), its tests and harnesses, the regression suite `deploy-metal.sh`, the guest images built by hand (`images/`), and the module's safety net (`safety/`, `SAFETY.md`). |
| `hv/hvm/` | The Mac engine's hardware probes, the arm64 guest kernels' build scripts, and notes. |
| `hv/kvm/` | The KVM engine's guest kernel patch and its check script. |
| `hydo/crates/` | The Rust workspace. |
| `hydo/onboard/` | The project format's reference `MANIFEST.md`, the examples, and `recorded.json`. |
| `hydo/guide/` | The guide that `hydo guide <topic>` prints. |
| `hydo/schema/` | The generated JSON Schemas and their released copies. |
| `hydo/bench/` | The bug benchmark: planted defects in the example order service. |
| `hydo/stack/` | The example order service (Go) used by the stack example and the benchmark. |
| `hydo/dist/` | Release packaging for Linux and the Mac, and the release index `releases.txt`. |
| `sdk/` | The assertion SDKs: Rust (the reference), Go, Python, and JavaScript. |
| `skills/` | `hydo-onboard` (for users) and `hydo-docs` (for contributors). |
| `docs/` | `HISTORY.md` (every milestone with its evidence), `REBASELINE.md` (recorded fingerprints), `BUILD-PINS.md`, `CI.md`, the release documents, `STYLE.md`, and `design/`. |
| `ci/` | The scripts that CI runs. |

## Tests are the specification

The formats between the parts are defined by their code and their tests, not by prose. The
schedule language is its parser plus example schedules. The SDKs share example files in
`sdk/cases/` that the Rust SDK's tests write; the Go, Python, and JavaScript SDKs must reproduce
each file byte for byte. The project, results, image, and agent-protocol formats each have a
generated JSON Schema in `hydo/schema/`. A test fails when a schema changes by more than additions
since its released copy in `hydo/schema/released/`.
