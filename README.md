# hydo — deterministic fault testing for distributed systems

hydo runs a whole system (your services, their databases and queues, and the workload that drives
them) inside a virtual machine whose execution is **bit-for-bit deterministic**: the same image and
the same schedule give the same run, every time. On top of that it injects faults from a seed:
process kills and pauses, network loss and delay, one-way partitions, power cuts at a chosen disk
flush, disk errors and lying disks, clock jumps, and faults in fake outside services. It explores
many such schedules, checks the system's properties after each run, and shrinks every failure to
a minimal schedule that replays exactly. A replay needs the same engine and the same image: a
failure found on Linux (x86_64) cannot be replayed on a Mac (arm64), and replay on a second
machine of the same kind is expected but not yet tested (hydo/PRODUCT.md, "Where hydo runs").

Software runs unmodified. hydo has found, for example, etcd 3.5.5's member-revision divergence
(issue 14685) without being told about it, MySQL showing a commit before its redo log was flushed
when the binary log is off, and Kafka deleting acknowledged records after a large clock jump with
its default retention; it reproduces the SQLite 3.51.2 WAL-reset corruption.

**Status: experimental.** The engines and the product work and are tested, but interfaces and
formats may still change between versions.

## Install

On an Apple Silicon Mac with macOS 13 or later, the easiest way is Homebrew:

```sh
brew install kakoc/tap/hydo
hydo doctor
```

The formula (https://github.com/kakoc/homebrew-tap) installs the prebuilt Mac release and runs
`hydo setup`, which checks every file and signs the virtual-machine engine with the hypervisor
entitlement. Alternatively, download `hydo-mac-<version>.tar.gz` (or, for Linux on x86_64 with
Intel VT-x, `hydo-linux-<version>.tar.gz`) from https://github.com/kakoc/hydo/releases, unpack it
and run its `install.sh`; the tarball's README or `QUICKSTART-mac.md` takes it from there.

To onboard a project with an AI coding tool, use hydo's onboarding skill: run
`hydo skill install --agent claude` (or `codex`, `cursor`, `copilot`, `gemini`) in the repository,
then ask the tool to onboard the project with hydo. From hydo 0.2.0 on, Claude Code can also
install it as a plugin (`/plugin marketplace add kakoc/hydo`), and `hydo skill update` fetches a
newer skill. The skill holds the process; it reads every fact about your hydo from `hydo guide`.

## How it fits together

```
 your project ── hydo.yaml or a hydo/ folder ──┐
                                               ▼
 ┌──────────────────────── hydo (the product, Rust) ────────────────────────┐
 │ hydo-cli: the `hydo` command                                             │
 │ hydo-build  hydo-explore  hydo-composer  hydo-errors  hydo-history  ...  │
 │ hydo-manifest (project format)    hydo-schedule (schedule language)      │
 │ hydo-engine: the Engine / Worker interface                               │
 └──────────────┬────────────────────────────────────────┬──────────────────┘
                ▼                                        ▼
   hv/hvx — Linux engine                     hv/hvm — Mac engine
   a VT-x kernel module (/dev/hvx)           arm64 Linux on Apple Silicon
   + the run engine hydo-hvx (Rust)          through Hypervisor.framework (hydo-hvm, Rust)
```

- **The product** builds a guest image from a project description, drives an engine through one
  interface (`hydo-engine`), explores fault schedules, checks the results and reports them. It is
  one Rust workspace (`hydo/crates/`).
- **The engines** run the guest deterministically and apply faults. They share no code; each
  implements the same schedule language. `hvx` is our own Intel VT-x hypervisor as a Linux kernel
  module: it owns the VMCS, so it controls `RDTSC`, interrupts and every VM exit, and its clock
  counts VM exits or the guest's retired branches. `hvm` runs on Apple Silicon Macs without root.

## Repository layout

```
hv/hvx/        the Linux engine: hvx.c (module), hvx_abi.h (ioctl ABI), module tests and
               harnesses, images/ (guest kernel and hand-written images), safety/ (KASAN kernel
               and syzkaller), deploy-metal.sh (regression suite); its run engine is the Rust
               crate hydo/crates/hydo-hvx (M132; the C hvx_explore.c was deleted in M137)
hv/hvm/        the Mac engine's hardware probes, arm64 guest kernels and write-ups; its run engine
               is the Rust crate hydo/crates/hydo-hvm (M143; the C hvm.c and hvmx were deleted in M147)
hydo/crates/   the Rust workspace (see below)
hydo/onboard/  the guest-side init and its extensions (shell, inside every image), MANIFEST.md
               (the project format), examples
hydo/          other product parts: readers/ (store readers),
               grpcgen/ (gRPC driver generator), stack/ (the example order system), bench/ (the
               bug benchmark), dist/ (release packaging)
sdk/           assertion SDKs for programs under test: Rust (the reference), Go, Python,
               JavaScript (docs/PUBLISHING.md: their registry packages)
skills/        hydo-onboard, an Agent Skill that writes a project's hydo/ folder
docs/          HISTORY.md (every milestone with its evidence), REBASELINE.md, BUILD-PINS.md
```

### The Rust workspace

| Crate | Responsibility |
|---|---|
| `hydo-schedule` | The schedule language as types (`disk=cut dseed=1 \| run K=redis@11+1 C=24`), parse and print, byte for byte. |
| `hydo-manifest` | The project format: `hydo.yaml`, a plan's `test:` section, the `hydo/` folder; parsing and validation. |
| `hydo-engine` | The `Engine` and `Worker` traits and the two implementations (`hvx`, `hvm`). |
| `hydo-build` | Builds guest images for x86_64 and arm64, including services from Docker/OCI images. |
| `hydo-explore` | The explorer: strategies, sweeps, re-runs, shrinking, reports. |
| `hydo-composer` | The test composer: commands composed into rounds, stable saved regressions. |
| `hydo-errors` | Error log lines as findings, with a reviewed list of known errors. |
| `hydo-history` | Linearizability and list-append checks of recorded histories. |
| `hydo-results` | The results format: run and exploration documents, the event stream, the lenient reader, the generated JSON Schema. |
| `hydo-compose` | Imports a `docker-compose.yaml` as a starting `hydo.yaml`. |
| `hydo-agents` | `hydo guide`, `hydo skill install`, `hydo init` for AI coding agents. |
| `hydo-world` | The program inside the guest that serves `fakes:` and `proxies:` (HTTP, gRPC, TCP), applies `H=` protocol faults and records what the outside world saw (a static musl binary in every such image). |
| `hydo-bench` | The bug benchmark: how well explorer strategies find the planted defects (not shipped). |
| `hydo-tools` | Build-machine helpers: Nix-cache fetch and unpack, cpio extraction, ELF library lists (not shipped). |
| `hydo-cli` | The `hydo` command (package `hydo`, the name `cargo install hydo` uses; docs/PUBLISHING.md). |

The formats between the parts (the schedule language, the project format, the SDKs' output lines)
are defined by their implementations and their tests; the tests' recorded examples are the
specification.

## Building and testing

```sh
cargo test --locked                        # the whole workspace (Rust 1.90.0, rust-toolchain.toml)
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

- **The `hydo` command from this tree:** `cargo build --release -p hydo` gives `target/release/hydo`,
  which reports this tree's version and commit (`hydo --version`). A checkout has none of a
  release's files (guest kernels, engine programs, tools), so such a `hydo` uses its version's
  release from the install cache, as a `cargo install`ed one does: `target/release/hydo setup --get
  <version>` fetches it once (`hydo doctor` names the command), and the tree's `hydo` then runs over
  it.

- **The Linux engine** needs an x86_64 machine with Intel VT-x and the running kernel's headers:
  `make -C hv/hvx` builds `hvx.ko`; `hv/hvx/deploy-metal.sh` runs the engine's regression suite on
  such a machine. Bare metal is best; a VM works if it exposes nested VT-x, but the exact branch
  clock needs a working PMU and does not work on Intel hybrid CPUs.
- **The Mac engine** needs an Apple Silicon Mac: `cargo build --release -p hydo-hvm` builds the
  program `hydo-hvm`, which hydo signs with the hypervisor entitlement (`hv/hvm/hv.entitlements`)
  before it runs it.
- **Guest images** are reproducible only with the pinned toolchain listed in `docs/BUILD-PINS.md`.

## Using it on a project

`hydo init` creates a project's `hydo/` folder; `hydo guide` prints the onboarding guide, which an
AI coding agent can follow (`hydo skill install --agent claude|codex` installs it as a skill).
`hydo build`, `hydo run`, `hydo explore` and `hydo repro` build the image, run one schedule,
explore many and replay a failure; their results are `result.json`, `exploration.json` and
`events.jsonl` (schema `hydo/schema/results-v1.json`; `hydo/PRODUCT.md`, "Where the results are").
`hydo/onboard/MANIFEST.md` describes the project format and
`hydo/PRODUCT.md` describes hydo from a user's point of view.

## Limits

- The Linux engine supports Intel VT-x only, not AMD.
- Machines with Secure Boot need the module signed before it can be loaded.
- Anyone who can open `/dev/hvx` can run guests through the kernel module, which is as powerful as
  root; give that access only to people you would give sudo.

## Contributing

`AGENTS.md` describes the project's conventions and current state for anyone working in the
repository, people and coding agents alike. The two rules that matter most: a change must keep
every recorded fingerprint unchanged unless it deliberately changes one, and a new feature is
opt-in and inert when off.

## License

Apache License 2.0 (`LICENSE`). The Linux kernel module `hv/hvx/hvx.c` is GPL-2.0-only, and its
interface header `hvx_abi.h` is GPL-2.0-only WITH Linux-syscall-note OR Apache-2.0; see
`hv/hvx/LICENSE.md`.
