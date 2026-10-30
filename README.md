# hydo — deterministic fault testing for distributed systems

`hydo` runs a whole system (your services, their databases and queues, the workload that drives
them) inside a virtual machine whose execution is **bit-for-bit deterministic**: the same image and
the same schedule give the same run, every time. On top of that it injects faults (process kills
and pauses, network loss and delay, power cuts at a chosen disk flush, clock jumps, disk errors,
faults in outside services) from a seed, explores many such schedules, checks the system's
properties after each run, and shrinks every failure to a minimal schedule that replays exactly.

## Two engines, one product

hydo has two hypervisors ("engines") and one product on top of them. The engines share no code; the
product drives either through the same schedule strings.

- **`hv/hvx/` — the Linux engine.** `hvx` is our own Intel VT-x hypervisor, a Linux kernel module
  (`hvx.c`, device `/dev/hvx`, ioctl ABI in `hvx_abi.h`). It owns the VMCS, so it controls `RDTSC`,
  interrupts and every VM exit. Its clock counts VM exits, or retired guest branches with the PMU
  branch clock, and it can run several vCPUs in exact, branch-counted turns. It boots unmodified
  x86_64 Linux and real services (MySQL, Kafka, Redis, etcd, the JVM) identically on every run, with
  snapshots and branch-from-snapshot. `hvx_explore.c` is the run engine the explorer drives;
  `images/` builds the guest kernel and the hand-written guest images; `deploy-metal.sh` is the
  regression suite. `ARCHITECTURE.md` describes the engine.
- **`hv/hvm/` — the Mac engine.** `hvm` runs deterministic arm64 Linux on Apple Silicon through
  Hypervisor.framework (`hvm.c`, virtio devices, the explorer server `explore/serve.c` = `hvmx`, the
  guest kernels in `linux/` and `explore/`). See `PROBE.md`, `BOOT.md`, `VIRTIO.md`, `explore/README.md`.
- **`hydo/` — the product both engines use.** `onboard/` turns a project's `hydo.yaml` (or `hydo/`
  folder) into a guest image (`hydo-build`, `MANIFEST.md`) and holds the examples; `explore/` is the
  explorer's notes and research scripts (the explorer itself is the Rust crate
  `crates/hydo-explore`: schedules, fault strategies, shrinking, reports; `hydo explore` runs it); `history/` checks
  recorded histories (linearizability, list-append); `readers/` reads stores after a run;
  `onboard/world/` serves fake outside services and fault proxies; `grpcgen/` generates gRPC
  drivers; `assert/` builds the assertion property table; `stack/` is the example four-service
  order system and the Go assertion SDK; `bench/` is the bug benchmark; `dist/linux` and `dist/mac`
  build the two releases. `PRODUCT.md` describes hydo from a user's point of view.

## Layout

```
hv/hvx/     the Linux engine (kernel module, run engine, tests, guest images, engine tools)
hv/hvm/     the Mac engine (VMM, explorer server, arm64 guest kernels, experiments)
hydo/       the product: onboard/ explore/ history/ readers/ grpcgen/ assert/ stack/ bench/
            knownbugs/ cutflush/ spec/ richcov/ dist/{linux,mac}
sdk/        assertion SDKs for guest programs: JavaScript, Python, Rust, Rust test composer
skills/     the hydo-onboard Agent Skill (writes a project's hydo/ folder)
docs/       HISTORY.md (every milestone), REBASELINE.md (image fingerprints), BUILD-PINS.md
```

## Getting started

- On an Apple Silicon Mac: `hydo/dist/mac/QUICKSTART-mac.md`.
- On an x86_64 Linux machine with Intel VT-x: `hydo/dist/linux/QUICKSTART-linux.md`.
- To onboard a project: `skills/hydo-onboard/SKILL.md` (or `hydo guide` from a release).

The project's history, with the evidence and fingerprints of every milestone, is in
`docs/HISTORY.md`. An earlier Rust VMM on top of KVM (`src/`), which could not get past Linux's
`RDTSC` calibration because KVM does not let userspace intercept `RDTSC`, was removed in Milestone
100; its design and results are in `docs/HISTORY.md`.
