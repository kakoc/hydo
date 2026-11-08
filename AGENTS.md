# Instructions for agents and contributors

This file is the instruction file for every coding agent that works in this repository. It
follows the AGENTS.md convention (https://agents.md). `CLAUDE.md` only imports it, so every tool
reads the same text. Update this file, never `CLAUDE.md`.

hydo is a deterministic hypervisor and the testing product built on it. It runs a whole system in
a virtual machine, so the same image and the same schedule give a bit-identical run. A schedule
is the text that names the faults of one run, for example
`disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24`. hydo injects faults from a seed,
checks the system's properties, and shrinks each failure to a minimal schedule.

The repository has three engines that share no code:

- hvx: the project's own Intel VT-x hypervisor, a Linux kernel module (`hv/hvx/`).
- hvm: the Apple Silicon engine on Hypervisor.framework (`hydo/crates/hydo-hvm`).
- KVM: an engine on stock Linux KVM, without the module (`hydo/crates/hydo-kvm`).

The product that uses the engines is in `hydo/`, the SDKs are in `sdk/`, the skills are in
`skills/`, and the project-wide documents are in `docs/`. `README.md` describes hydo for users.
`docs/ARCHITECTURE.md` describes the parts, and `CONTRIBUTING.md` says how to build and test.

## How to record new work

Follow these steps for each milestone:

1. Add a new milestone section with its details and its evidence to the end of `docs/HISTORY.md`.
2. Add a one-line entry to the "Milestone index" below.
3. Update "Current state" below, and "Open items" if something stays open.

Keep this file short. Details, evidence and superseded values belong in `docs/HISTORY.md`. This
file lists only current values. Write every document to `docs/STYLE.md` (the `hydo-docs` skill).

## Machines and how to run

Besides this Mac, the project uses two machines. Tracked files call them only `the bare-metal
machine` and `the build machine`. The bare-metal machine runs hvx. The build machine builds
guest images; it is optional since M140, because Nix runs on the bare-metal machine too.

No tracked file says how to reach a machine. The addresses, the logins, and the command that
reaches each machine are only in two git-ignored files at the repository root:

- `AGENTS.local.md`, which `CLAUDE.md` imports.
- `machines.local.sh`, which defines `BARE_METAL`, `on_bare_metal`, `BUILD_MACHINE` and
  `on_build_machine`.

Every script that works on a machine sources the tracked `machines.sh`, which reads
`machines.local.sh`. In a git worktree without its own copy, it reads the main checkout's file.
It stops with a message that names the file when the file is missing (M151).

### The bare-metal machine

- Hardware and system: a Xeon E3-1270 v6 (not hybrid, with a working performance-monitoring unit,
  the PMU), 32 GB, Ubuntu with
  kernel 6.8.0-146-generic, passwordless sudo. The main tree is `~/hydo`. hvx runs on the host
  itself, without nesting (M33).
- If the bare-metal machine stops responding, only the user can restart it. Ask the user.
- Module locking: every VM run goes through `~/bin/hvx-run`, a shared flock on
  `~/.hvx-module.lock`. Every module reload goes through `~/bin/hvx-reload`, which takes the lock
  exclusively and builds in `~/hydo/hv/hvx`. Use both, so that a reload never removes the module
  from under another session's run (M35).
- The regression suite: run `hv/hvx/deploy-metal.sh [load] [flagships]` from the Mac. It syncs the
  userspace sources and the Rust workspace to `~/$HVX_METAL_TREE` (default `hydo`), builds there,
  runs the module tests and the two pinned reproducers through `hvx-run`, and compares each result
  with its baseline. Its last run printed `ALL PASS (33 checks)`. Since M192 it also runs
  `hvx_hugeram_test 8192` (RAM hash `b71aaf1096f96f02`), so its next run prints 34 checks.
  - It syncs `hvx.c` and `hvx_abi.h` only with `load`, which also rebuilds and reloads the main
    module. `load` refuses when `~/bin/hvx-reload` does not build in `~/$HVX_METAL_TREE/hv/hvx`.
  - `flagships` also runs every real-service verify.
- The main module loaded now is M179's: guest RAM up to 8 GiB, with M156's PMU-clock guard. Its
  srcversion is `869F246124D8A3F84879E74`. The user loaded it on 2026-11-08 with
  `deploy-metal.sh load`. The earlier modules are in `docs/HISTORY.md` (Milestones 140, 156 and
  187).
- Rust: run `. ~/hydo-rust/env.sh` first (rustup with Rust 1.90.0 and the musl target). Build the
  run engine in a tree with `cargo build --release -p hydo-hvx`.
- Nix (since M140): run `. ~/hydo-nixb-work/nixenv.sh` to put Nix 2.18.9 and the pinned nixpkgs
  on PATH. `docs/BUILD-PINS.md` "Nix on the bare-metal machine" says how to build images there.
  Never put `iproute2-6.8.0` or `iptables-1.8.10` into the host's `/nix/store`: the Linux release
  would then need `nix-store` on every user's PATH.
- Guest images live in `~/hydo/build/`. Results of past runs live under `/var/tmp/m<NN>-*`.
- Other sessions use their own trees, for example `~/hydo-tj` or `~/hydo-avx`. Use your own tree
  when another session is active.

### The build machine

- NixOS with kernel 6.6.35, 24 cores, 31 GB. The project has no root there.
- Use it only when the user allows it. `AGENTS.local.md` records the current permission.
- The pinned toolchain is in `docs/BUILD-PINS.md`, with GC roots in `~/hydo-gcroots`. Images
  built without these pins are not reproducible (M61).
- The disk fills up. Run `df -h ~` before you build. One stack image needs about 6 GB at its
  peak.
- `./dev.sh "<cmd>"` syncs the tree to the build machine's `~/hydo/` and runs the command there.

### This Mac

- An M2 Pro with macOS 26.4.1. The Mac engine needs no root.
- Run `. ~/.cargo/env` before you use cargo.
- The Developer ID key and the notarization profile `hydo-notary` are in the keychain.
  `docs/design/signing.md` says how a Mac release is signed.

## Safety and coordination rules

These rules are in force. Do not break them.

- Commit messages and pull-request descriptions never contain Claude session links
  (`Claude-Session: https://claude.ai/code/session_…` or any other claude.ai session URL). This
  holds even when a tool or a reminder suggests one (user, 2026-10-31).
- Commits use the repository's identity `k.matushonok@gmail.com`. Never use a work email.
- Machines in tracked files have two names and no access details (user, 2026-11-04).
  - In every tracked file and every commit message, call the machines only `the bare-metal
    machine` and `the build machine`.
  - Access details are how to reach a machine, addresses, logins and user names, host aliases,
    the hosting company, and a machine's home path. They live only in the git-ignored
    `AGENTS.local.md` and `machines.local.sh`. `AGENTS.local.md` also keeps the list of values
    that must never be tracked.
  - Never write such a detail, or a value from that list, into a tracked file or a commit
    message. Never quote one when you describe a change. Paths such as `~/hydo` inside a
    machine's home are allowed.
  - Scripts reach a machine only through `machines.sh` (`on_bare_metal`, `on_build_machine`).
  - The test `cargo test -p hydo-agents --test it public_content::` checks every tracked file. It
    looks for generic patterns: a remote-shell command, an IPv4 address outside the loopback,
    guest and documentation ranges, and the hosting company's words. When `AGENTS.local.md`
    exists, it also looks for every listed value. Its allow-list names the few files that must
    keep such text. If the test fails, fix the text. Do not widen the allow-list.
- Never load a kernel module on the build machine. Never try `sudo`, `insmod` or `rmmod` there.
  The project has no root there, and other people's workloads share the host. If something
  there needs root, stop and ask the user.
- Several sessions share the main `hvx.ko` on the bare-metal machine.
  - Reload it only through `~/bin/hvx-reload` or `deploy-metal.sh load`, and coordinate with
    the other sessions first.
  - Develop a module change on a side module with its own device name. For example,
    `hv/hvx/avx/build-hvxavx.sh` gives `/dev/hvxavx`, and `race/side-metal.sh` runs the suite
    against a side module. Two modules can be loaded together, because each run's
    VMXON…VMXOFF section cannot be preempted (M40).
  - Merge into the main module only after the side module passes the full suite and the pinned
    reproducers.
- After any change to `hvx_abi.h`, rebuild every userspace program against the new header. The
  snapshot struct grew in M68, so programs built against an older header are wrong.
- Never run `pkill -f <pattern>` or `pgrep -f <pattern>` through a remote shell when the pattern
  also matches the remote command line.
  - `pkill` then ends your own session.
  - A wait loop such as `until ! on_bare_metal 'pgrep -f "<pat>"'; do sleep; done` then never
    ends.
  - Match with `pgrep -x`, a pid file or a done-marker file instead. Or put the `pkill` into a
    script whose path does not match.
- Give every wait loop a timeout. This includes the wait loops of the agents you start.
- Build guest images one at a time on the build machine. Copy each image to the bare-metal
  machine before you build the next, because the disk fills up.
- Delete only your own files. Several sessions share the machines. For anything else, ask the
  user and name the candidates. Leftovers that are safe to regenerate are the
  `~/hydo/build/rootfs-*-root` trees and the `*-ext4.img` and `*-data.img` intermediates.
- Stop your own background jobs by pid file, never by name. On the shared machine,
  `pgrep -x <script>` once matched another session's script of the same name and killed it
  (2026-10-24). Write each job's pid to a file in your own directory, and kill only that pid.
- No other product in hydo's space appears in tracked files (user, 2026-11-07).
  - Tracked files never present hydo in relation to another product in its space. This means no
    `follows`, `in the style of`, `maps to`, or `differences from`. It also means no such name in
    package metadata, documentation, the skills, or code comments.
  - The only allowed mention is a factual citation of published prior work inside research
    notes.
  - The rule's full text and the names it covers are in the git-ignored `AGENTS.local.md`.
  - Open projects and analyses cited as sources (FoundationDB, Jepsen, rr, papers) are named
    factually.
  - Before a commit, `git grep -i` of each name in that list must find only research-note
    citations (M157).
- Keep existing fingerprints stable. A new module feature must be opt-in and inert when off, so
  that every recorded baseline stays the same. Check this with `deploy-metal.sh` and the pinned
  reproducers before and after the change.
- The fingerprint epoch rule. `hydo_image::EPOCH` (now 1, since M155) names the generation of
  fingerprints. Raise it by one in the same commit as any change that moves a recorded
  fingerprint. Such changes are changes of an engine, the host controller, the agent pin or the
  guest kernels. Add a section of `docs/REBASELINE.md` that names the new epoch. Runs of one epoch
  compare fingerprints. Runs of different epochs compare only verdicts.

## Language policy

The user decided this policy (updated 2026-10-30).

- Rust is the project language (user, 2026-10-30). Most code is Rust, and the Rust
  implementation is the reference for everything. There is one `hydo` command in Rust, over one
  engine interface, in the Cargo workspace at the repository root. The Python product layer was
  ported to Rust in M101–M107, and each port was proven identical to the Python version. The
  Python was deleted in M107.
- The kernel module stays in C (`hv/hvx/hvx.c`). Most of its bugs were about hardware semantics
  and determinism, which Rust does not prevent. Its core is VMX instructions and inline assembly
  that would be `unsafe` Rust anyway. A Rust module also needs a kernel built with Rust support,
  which most distribution kernels lack. Its safety net is testing: `hv/hvx/safety/` (`SAFETY.md`,
  M97).
- Do not write prose specifications. Examples and tests are the specification. For example, the
  SDKs share a folder of example files that the Rust SDK's tests generate, and the schedule
  language is defined by its parser and example schedules.
- The Rust SDK is the reference. The Go, Python and JavaScript SDKs follow it by testing against
  the shared example files, with a frozen scope (assertions, BUGGIFY, history records). A format
  change starts in Rust. Add no new SDK languages. Code-first test definitions exist only in Rust
  (`hydo-test`).
- A rewrite must be byte-for-byte neutral. It must give the same fingerprints for the pinned
  reproducers and for every recorded baseline, and the same console and schedule formats. Do not
  rewrite guest-side programs whose bytes are inside recorded images, for example `hvx_seedrng`
  (pinned `c2a440c5…`).
- No Go helpers are left in the product. The hand-written module tests and verify harnesses stay
  in C while they only exercise the module's C ABI.

## Current state

Each paragraph gives the current values and where the details are.

**Releases.** hydo 0.2.0 was published on 2026-11-07 in these places:

- The GitHub release `v0.2.0`: both tarballs, their sources archives, and the Mac signatures.
- The Homebrew tap: `brew install kakoc/tap/hydo`.
- crates.io: the 20 crates of `cargo install hydo`, and `hydo-assert`.
- PyPI and npm: `hydo-assert`. Go: the tag `sdk/go/v0.2.0`.
- The skill release `skill-v1.0.0`.

The release index `hydo/dist/releases.txt` pins every published archive. Since 0.2.0 the Linux
release ships the KVM engine, and the Mac programs are signed and notarized. Checksums and verification:
`docs/HISTORY.md` Milestone 177. How to release: `docs/RELEASING.md` and `docs/PUBLISHING.md`.

**Registry publishing from GitHub Actions.** From the next release on, a tag `v<version>`
starts `.github/workflows/release.yml`. It publishes the crates, the PyPI and npm packages, and the
Go tag with each registry's trusted publishing, so no token is stored. `packages.yml` dry-runs the
packages on every pull request that changes one. The user is doing the one-time setup on each
registry (open item 63); until it is done, a release is published by hand. The details are in
`docs/PUBLISHING.md`, section "Publishing from GitHub Actions" (M186).

**hydo 0.3.0: prepared, not published (M190).** The version is 0.3.0 everywhere it lives. The
release adds M178–M189 and M191 to 0.2.0:

- `hydo inspect` and the failure diagnosis (M178).
- Guest RAM up to 8 GiB (M179).
- `tls:` for fakes, and writable image services (M180).
- Small fixes from the onboardings (M181).
- `hydo sql-gen` and the guest program `hydo-sql` (M182).
- Storage faults of one service, `G=` and `L=` (M183).
- The check that a schedule names only what its image has (M184).
- The KVM engine at x86-64-v3 (M189), and a plan of only checks or regressions keeps the
  manifest's own workload (M191).

Both releases ship `guest/hydo-world` (re-pinned), `guest/hydo-world-tls` and `guest/hydo-sql`,
pinned in `hydo/dist/<platform>/guest-programs.sha256` (`docs/BUILD-PINS.md`). Every example was
re-recorded; the fingerprint epoch stays 1 (`docs/REBASELINE.md` section 18). The release commit
is `76a51408`. It adds the `rehearse.sh` fix that the release workflow needs to `2aa54fa4`, which
the files were first built from (`docs/HISTORY.md` Milestone 190, the addendum). Its five files and
`SHA256SUMS` are in `hydo/dist/out/0.3.0/` of the main checkout (git-ignored; the files built from
`2aa54fa4` are in `before-2aa54fa4/`), each platform built twice to the same bytes:

- `hydo-linux-0.3.0.tar.gz` `d274e519c53dda6b0512080cef0f96e90a86c042493f9da76bef79a6d2805680`;
- `hydo-linux-0.3.0-sources.tar.gz` `00a4c143184199b652216ea6ef201ba17dd43e60c7d5d5b492c8205980e3c4c8`;
- `hydo-mac-0.3.0.tar.gz`, signed and notarized,
  `6eb5ae166013b06be31b39cbd0b8b7e6b2491e53024dcd8af5bf6c95a9e8c55a`;
- `hydo-mac-0.3.0-sources.tar.gz` `b294cca72f5ad2563798a89ca19f7c224e61f8baef9c36df5bba68925cfa29b1`;
- `hydo-mac-0.3.0-signatures.tar.gz` `d93d46efe60b7b30ded96c760c1a4e9b131deeab69b9def2d404269551af8cf8`.

The prepared material (release notes, the Homebrew formula, the release-index lines, the skill
release `skill-v1.2.0`) is in `hydo/dist/out/0.3.0/prepared/`. The checks and the publishing
steps are in `docs/HISTORY.md` Milestone 190.

**Fingerprints.** The fingerprint epoch is 1. The pinned reproducers are
`disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24` under `hydo run`: `1dc3677fb94053cf`
(VIOLATION, order 11) on `explore-ob-stack-old-disk.img` and `ba0aa0612502a1df` (OK) on
`explore-ob-stack-fix-disk.img`, in `~/hydo/build/m155` on the bare-metal machine.
`hydo/onboard/examples/stack/build-reproducers.sh` rebuilds both images. Every recorded example
value is in `hydo/onboard/recorded.json`, and `hydo-bench rebaseline` checks or re-records them
(`docs/REBASELINE.md` "How to re-baseline").

**The hvx engine.** The module (`hv/hvx/hvx.c`, ABI `hvx_abi.h`) boots Linux 6.6.35
deterministically, with guest RAM up to 8 GiB. Its clock counts VM exits, or the guest's retired
conditional branches with the PMU branch clock. Every module feature is opt-in; the engine turns on the
host-call port by default (M140). The run engine is the Rust crate `hydo-hvx`, which runs inside
`hydo` when the process can open `/dev/hvx`. Details:
`docs/HISTORY.md` Milestones 33, 66–69, 132, 137, 138, 156 and 179; the module's safety net is
`hv/hvx/SAFETY.md`.

**The Mac engine.** `hydo/crates/hydo-hvm` runs arm64 Linux 6.6.35 through Hypervisor.framework,
with two guest kernels: `Image-explore`, and `Image-explore-netns` for `network: bridge`. Its
clock counts VM exits. The program is signed with `hv/hvm/hv.entitlements` before it runs.
The details are in Milestones 48, 51, 99, 143, and 147.

**The KVM engine.** `hydo/crates/hydo-kvm` runs the x86_64 images on stock Linux KVM, without the
module or root, with one vCPU and a patched guest kernel (`bzImage-kvm`). The vCPU is at the
x86-64-v2 level, or at x86-64-v3 for `guest.cpu: v3` on a host whose KVM has AVX2 (M189;
`hv/kvm/v3-check.sh`). It runs on Intel and on AMD (GitHub's ubuntu-24.04 runners, 2026-11-08).
Its fingerprints are its own and differ between CPU vendors; the verdicts are the same. Details:
`docs/design/kvm-engine.md`, Milestones 159, 161, 175 and 189, and open item 45 in
`docs/HISTORY.md` Milestone 187.

**Guest control.** Every image is an agent image. The guest agent `hydo-agent` (0.3.0) is PID 1
and runs only primitives. The host controller (`hydo/crates/hydo-control`) drives services,
workload, faults and checks under lock-step control of the engine. Every run leaves a control
transcript. Details: `docs/design/guest-control.md`, Milestones 116–145.

**The product.** The Rust workspace is in `hydo/crates/`. `docs/ARCHITECTURE.md` lists the
crates. The project format (`hydo/schema/project-v1.json`), the results format
(`results-v1.json`), the agent protocol (`agent-v1.json`) and the image records
(`image-v1.json`) are versioned; additions only, checked by `hydo-schema-compat` against
`hydo/schema/released/`. `hydo-config` owns every environment variable
(`hydo/crates/hydo-config/INVENTORY.md`).

**Finding and explaining failures.** The explorer is `hydo-explore`; the bug benchmark is
`hydo/bench/` with `hydo-bench`. `hydo diff` and `hydo explain` compare runs
(`docs/design/explain.md`). `hydo inspect` and the diagnosis in `result.json` show why a run
failed (`docs/design/inspect.md`).

**Onboarding and SDKs.** The skill `skills/hydo-onboard/` holds the process; every hydo fact is
in the guide `hydo/guide/`, printed by `hydo guide <topic>`. The assertion SDKs are in `sdk/`
(Rust is the reference), with the shared cases in `sdk/cases/`. `sdk/README.md` is for SDK
maintainers: the shared cases, and the Go module with its frozen copy in `hydo/stack` (section
"The Go module and the frozen copy in `hydo/stack`"). Every code example on the package pages is a
test in its language (M188). The examples are in `hydo/onboard/examples/`;
`hydo/onboard/MANIFEST.md` is the project format's reference.

**Continuous integration.** `.github/workflows/ci.yml` runs per push: formatting, clippy, and the
`fast` tests of the affected packages. `ci-full.yml` runs every test nightly and before a
release. `docs.yml` runs Vale on the documents (`docs/STYLE.md`). Details: `docs/CI.md`.

**hydo in a team's CI (design only).** `docs/design/ci.md` designs hydo as part of a team's own
continuous integration: findings proven new or fixed by replay, a small history per run, and
seed ranges split across runners. It plans stages 0 to 4. None of it is built yet.

**Documentation.** `docs/STYLE.md` is the writing standard. `sh ci/vale.sh` checks 48 documents
(`sh ci/vale.sh --list`), and `cargo test -p hydo --test it docs::` checks their links and the
facts and commands of `README.md` and the quickstarts. Release step 6a is a docs review by a
fresh reader (`docs/RELEASING.md`). Before M187 this file held the full current state; that text
is kept verbatim in `docs/HISTORY.md` Milestone 187.

## Open items

Items marked [user decision] need the user before anyone acts on them. Each item names the
milestone where it is described. The full text of each item as of 2026-11-08 is in
`docs/HISTORY.md` Milestone 187.

2. [user decision] The single-Kafka image and the onboarding stack example keep Kafka's default
   retention and `skip-log-bin`; fixing them moves unrelated baselines (M63).
3. [user decision] The build machine's disk fills up; cleaning other sessions' outputs needs the
   user.
4. Several vCPUs: no NMIs between CPUs, no logical-destination routing, and at most 4 vCPUs
   (M69). `hydo/explore/TURNS.md` lists the module wishes.
5. Guest AVX: no AVX-512, XSAVEOPT, XSAVEC, XSAVES or PKRU (M68). On KVM, VT-x cannot make
   XSAVEOPT, XSAVEC or XGETBV with ECX=1 fault, so a v3 guest program that uses them without
   checking CPUID sees the core's own behaviour (M189).
6. The benchmark defect d11 is found rarely; measuring its rate needs more repetitions (M67, M72,
   M139).
7. fwupd still runs on the bare-metal machine. Old single outliers were not checked against its
   times (M65).
8. etcd 3.5.5: why issue 14685's second symptom needs a later pause of another member was not
   investigated (M78).
9. Explorer gaps. The oracle reads Kafka before its restart after a kill at the last order (M55).
   The `xt` shrinker keeps a seeded policy (M49). An OK verdict shows only the first invariant's
   details (M47).
10. etcd 3.6.8 against 3.6.9 needs timed pauses aimed at the leader.
11. Images built before M71 drop netem's queue when a delay fault ends.
12. Processor Trace limits: no guest modules, no JIT-compiled Java, and buffers of about 1 GiB can
    fail to allocate (M40, M44).
13. Mac engine: a memory-flag spin in user space with no exits cannot be interrupted
    deterministically (M48, M53, M57).
14. Older limits: the PMU clock is not instruction-exact (M33), host performance counters are off
    while a PMU-clock guest runs (M34), and a read-write ext4 root is not done (M29).
15. Images not rebuilt with the pinned tools: `explore-cc0/1/cc`, `explore-dx` and `richcov`
    (M50, M61, M108).
16. Linux release: the udev rule's group setup and `install.sh` without `--no-module` were never
    run on a machine (M75, M81, M149, M154).
18. Compose import: one bridge for all Compose networks, no per-service hostname or PID
    namespace (M84, M95).
19. gRPC faults: no TLS and no injected trailing metadata (M90). Test composer: `cargo hydo` was
    not run end to end through a release (M86).
20. History checkers: no G-nonadjacent snapshot-isolation cycles; multi-key `txn` only in unit
    tests (M87).
21. Fake services: the world carried across a power cut is limited to 3,000 bytes (M89).
22. Generated gRPC drivers: plaintext only; the Mac release does not ship `hydo-grpc-gen` (M91,
    M113).
23. The skill was run only with Claude Code; other tools are out of scope (user, 2026-10-30).
24. The rust-kv binary depends on the checkout path (M140, M163).
25. Safety net: syzkaller does not know the newer ioctls and module parameters (M97, M138,
    M156).
26. Error logs: no run on the Mac or from a release; no saved error-kind regressions (M94).
27. Platform differences: `hydo module` and udev only on Linux; the static Linux `hydo` refuses
    `--arch arm64` (M107).
28. Example manifests' comments still name Python-era paths; their text is inside images (M107).
29. Python left in build scripts for the build machine (M108).
30. Guest control: several vCPUs and a schedule's own `rb=` are refused for agent images; a hang
    over the serial line stays unexplained (M136, M138, M140).
31. The Rust test SDK still writes history checks in the retired `test.checks` spelling (M114).
32. Results format: only Rust has a reader; `hydo-bench`'s own files are not in the format
    (M115).
33. hydo-world's gRPC proxy sends a trailers-only answer without trailers (M111).
34. The hvx engine: an unreadable console ends a run with `ERROR vm`; Intel PT can fail with
    ENOMEM on the shared machine (M132, M137).
36. [user decision] Copy-on-write VMs, a shared read-only root, and the host-call port on the Mac
    engine are not built (M138, M140).
37. The M48 Mac boot fingerprint was never taken with the Rust engine (M147).
38. NATS JetStream: Jepsen 3.2, 3.3 and 3.5 are not hunted; no x86_64 run (M141).
39. [user decision] `~/hydo/build/explore-binlog-ret-xr-nq-disk.img` and old benchmark image sets
    on the bare-metal machine wait for deletion (M145, M146).
41. [user decision] Public content: freeze-sweep results without the system they were measured on
    (M73, M76, M92).
42. Image records: the benchmark images lack the record; a regression's `# expect: fp=` line has
    no epoch (M155).
43. The suite's recorded values of M121's agent boot check are older than M140 (M156).
44. [user decision] License compliance: the sources archives must stay available; three SPDX
    expressions are approximations; a lawyer's review is open (M158).
45. The KVM engine: one vCPU only. On AMD, RDRAND, RDSEED and TSX cannot be made to fault, so a
    guest program that uses them without checking CPUID sees the host (M159, M161).
46. Homebrew: no Linux formula (M168).
47. The install cache: no pruning; the Linux side ran only against a local server (M162).
48. [user decision] The Rust test SDK is unpublished until the user decides about code-first
    tests (M160, M163).
49. `hydo diff` and `hydo explain`: deeper levels need engine additions (M165).
50. `cargo install hydo` on Linux was checked only with `cargo check` (M169).
51. The GUI experiment is not a general feature yet (M166).
52. The skill: `hydo skill update` never ran against the real GitHub API (M170).
53. The benchmark images and the browser example are not in `recorded.json` (M164, M177).
55. [user decision] The label `full-ci` does not exist on GitHub yet (M171).
56. `hydo inspect` was not run on KVM; no MCP server (M178). The diagnosis's replay into a
    recovery boot ran on the Mac, not on hvx (M192).
57. `hvx_hugeram_test 8192` is in `deploy-metal.sh` since M192, but the suite has not run with it
    yet (M179, M192).
58. `tls:` and `writable:` were not run on hvx or arm64 (M180).
60. Ready-made SQL workloads: the MySQL dialect and arm64 ran only in unit tests (M182).
61. Storage faults: `L=` keeps writes the disk had not flushed (M183).
62. [user decision] `guest.outcomes_after_cut` stays opt-in until an epoch change (M184).
63. [user decision] Trusted publishing needs the user's one-time setup on GitHub, crates.io, PyPI
    and npm. The npm entry expires after two days unused, so add it just before a release. No
    run of `release.yml` or `packages.yml` has happened yet. A crate new to `cargo install hydo`
    needs its first version published by hand (M186).
64. YugabyteDB showed gaps: hvx's CPUID leaf 4 is empty (glibc 2.28 then needs
    `GLIBC_TUNABLES`); readiness gates deadlock a cluster whose health check needs its peers; the
    diagnosis reads only a service's stdout; the recovery clock after a `cf=` cut restarts at
    `clock_start`; the explorer never cuts links between peers of one cluster (M193).

## Milestone index

Details of each milestone are in `docs/HISTORY.md`. Milestones 1–32 ran inside a nested VM on the
build machine, before 2026-10-13.

- M1: VMXON and VMLAUNCH of a trivial guest in the project's own module
- M2: EPT, the guest in its own physical address space
- M3: `/dev/hvx`, a userspace interface like KVM's
- M4: deterministic RDTSC interception
- M5: the MTF single-step instruction clock
- M6: RDTSC tied to the instruction clock
- M7: snapshot and branch-from-snapshot
- M8: instruction-timed interrupt injection
- M9: canonical fingerprint and verify harness
- M10: I/O-port trap and serial console
- M11: guest RAM beyond 2 MiB
- M12: deterministic CPUID and MSR emulation
- M13: privileged-state persistence and `HVX_SET_SREGS`
- M14: first real Linux serial output as L2
- M15: real Linux boots to the initramfs shell
- M16: whole-boot determinism and branch on real Linux
- M17: PMU clock blocked on the hybrid CPU of the build machine (resolved in M33)
- M18: serial input and an interactive shell
- M19: deterministic multi-vCPU foundation on hand-written guests
- M20: deterministic virtio-PCI block device
- M21: real Linux uses the disk over MSI-X
- M22: deterministic disk fault injection
- M23: deterministic virtio-net device
- M24: deterministic network fault injection
- M25: flagship, a durability bug in hydokv found by a seeded fault
- M26: flagship on unmodified SQLite
- M27: flagship on Redis, the AOF durability window
- (no number): file-system images made byte-reproducible
- M28: page-array disk, disk mmap, RAM to 3 GiB
- M29: ext4 read-only root through switch_root
- M30: a two-service system over loopback
- M31: deterministic OpenJDK; Kafka hit a wall
- M32: the JIT is deterministic; file-backed snapshot
- M33: bare metal, Kafka flagship, PMU branch clock
- M34: MySQL flagship, pinned Kafka image, seven hvx fixes
- M35: the four-service stack with one oracle; `deploy-metal.sh`
- M36: service crashes and network faults on the stack
- M37: the order service made safe to retry, each order sent exactly once
- M38: the fault explorer and assertions; first bug found without a hand-picked scenario
- M39: the explorer's bug fixed and the fix proven by the explorer
- M40: exact guest code coverage with Intel Processor Trace
- M41: coverage-guided exploration, negative
- M42: swarm testing and exhaustive sweeps
- M43: tick jitter and mid-run branching
- M44: coverage growth and a portfolio scheduler, mostly negative
- M45: branch points from assertions, negative
- M46: pause, clock-jump and one-way-loss faults, no new bug
- M47: onboarding v1, one `hydo.yaml` per project
- M48: deterministic arm64 Linux on the Mac
- M49: shorter tick period and inserted ticks
- M50: concurrent clients find three bugs in the order service
- M51: the Mac engine explores real projects
- M52: onboarding follow-ups; the SQLite known bug not reproduced
- M53: tick jitter on the Mac, counted in VM exits
- M54: an 8-defect bug benchmark; five strategies compared
- M55: why d7 was missed; end-of-workload sweep
- M56: fault process, BUGGIFY, depth-1 sweep, cut at the k-th flush
- M57: forced preemption points on the Mac
- M58: a real bug found by the flush cut
- M59: guest RAM, liveness properties, Docker/OCI images
- M60: three-node etcd; a known upstream bug found without being told
- M61: re-baseline on the pinned toolchain
- M62: benchmark defects d8–d12
- M63: MySQL binary log on; Kafka retention after a clock jump fixed
- M65: SMIs counted on the branch clock caused rare fingerprint differences; fixed
- M66: several vCPUs, stage 1, exact branch-counted turns
- M67: strategies draw timed kills and k-th-flush cuts
- M68: opt-in guest AVX and AVX2 (x86-64-v3)
- M69: Linux on 2–4 vCPUs; the stack and the explorer on 2 vCPUs
- M70: freeze a service at an exact packet (`F=`)
- M71: a healed network fault drains the netem queue
- M72: benchmark images with the Kafka retention fix
- M73: the recovery boot continues the first boot's clock
- M74: several vCPUs found no race that one vCPU misses
- M75: the Linux release and the `hydo` command
- M76: the freeze sweep
- M77: example and stack images rebuilt with the netem drain
- M78: etcd 14685's second symptom reproduced again
- M79: the 2-vCPU stack baselines re-measured
- M80: the JavaScript SDK and the node-kv example
- M81: runs without sudo through a udev rule; builds independent of the time zone
- M82: the Rust SDK and the rust-kv example
- M83: the Python SDK and the python-kv example
- M84: Docker Compose import
- M85: cheaper vCPU turn ends; the SQLite WAL-reset corruption reproduced
- M86: the test composer
- M87: history checkers on the host
- M88: store readers `hydo-read` and `hydo-check`
- M89: fake outside services and protocol faults
- M90: gRPC faults through an HTTP/2 proxy
- M91: generated gRPC drivers
- M92: the adaptive freeze sweep
- M93: the onboarding skill in the Agent Skills format
- M94: error logs as findings
- M95: a network namespace per service
- M96: arm64 parity on the Mac engine
- M97: the module's safety net; nine module bugs fixed
- M98: stable compositions
- M99: a second Mac guest kernel for service networks
- M100: the folder layout for a public release
- M101: the Rust workspace and one `hydo` command
- M102: the explorer in Rust
- M103: the image builder in Rust
- M104: the test composer's host side and error logs in Rust
- M105: the history checkers in Rust
- M106: the Compose importer and the agent commands in Rust
- M107: the Python deleted
- M108: the last Python helpers ported or deleted
- M110: the SDKs follow Rust through shared cases
- M111: `hydo-world` in Rust
- M112: `hydo-read` and `hydo-check` in Rust
- M113: the gRPC driver generator and client in Rust
- M114: one versioned project format
- M115: one versioned results format
- M116: design of guest control from the host
- M117: the guest agent and its protocol
- M118: lock-step control in the hvx engine
- M119: lock-step control in the Mac engine
- M120: the host controller for agent images
- M121: agent images
- M122: guest control for liveness, clock steps, BUGGIFY, and recovery
- M123: guest control for histories, error logs, and the composer
- M124: guest control for network faults, the freeze, and service networks
- M125: guest control for the fake outside world
- M126: the host controller wired to the engines
- M127: agent images checked on x86_64
- M128: agent images checked on the Mac
- M129: agent images checked on the bug benchmark
- M130: busy-wait counter loops on agent images
- M131: the freeze in a service's network namespace
- M132: the hvx run engine in Rust
- M133: the lost order after a long Kafka pause explained (franz-go)
- M134: restart policies before the switch to agent images
- M135: the bug benchmark built from its manifest
- M136: the switch to agent images
- M137: the Rust hvx engine is the default
- M138: the host-call port and in-place VM reset on a side module
- M139: reactive exploration (`A=`), negative
- M140: the module package switched on; every example re-baselined
- M141: NATS JetStream onboarded; Jepsen's crash finding found blind
- M142: the schedule parsed once; typed engine calls
- M143: the Mac run engine in Rust
- M144: one typed configuration
- M145: one way to run a guest
- M146: the last shell-image leftovers removed
- M147: the C Mac engine deleted
- M148: one additions-only check for every format
- M149: the Linux release rebuilt for agent images
- M150: the Mac release rebuilt
- M151: public-content fixes; machines named only generically
- M152: the JavaScript SDK declares Apache-2.0
- M153: both tarballs rebuilt with M151's wording
- M154: the install logic moved into `hydo setup`
- M155: image records and the fingerprint epoch
- M156: a PMU branch-clock bug after host NMIs fixed
- M157: no competitor in tracked files
- M158: license-compliant releases and sources archives
- M159: the KVM engine
- M160: the SDKs prepared for their registries
- M161: KVM counter loops and shared snapshot memory
- M162: the install cache
- M163: the SDK APIs cleaned up
- M164: re-baselining is one command per platform
- M165: `hydo diff` and `hydo explain`
- M166: a web app tested through a real browser in the guest
- M167: continuous integration on GitHub Actions
- M168: Homebrew
- M169: `cargo install hydo` prepared
- M170: the skill's process apart from hydo's facts
- M171: a fast development loop
- M172: signed Mac programs; KVM in the Linux release
- M173: libSQL onboarded on KVM
- M174: Medusa onboarded on KVM
- M175: KVM snapshots keep the vCPU's run flags
- M176: TigerBeetle onboarded on hvx
- M177: hydo 0.2.0 built and verified
- M178: `hydo inspect` and the failure diagnosis
- M179: guest RAM up to 8 GiB
- M180: HTTPS for fakes; writable image services
- M181: small fixes from the onboardings
- M182: ready-made SQL workloads and checks
- M183: storage faults of one service
- M184: no silent passes
- M185: one test program per crate
- M186: registry packages published from GitHub Actions with trusted publishing
- M187: the writing standard, Vale, a shorter AGENTS.md and a new README
- M188: the package pages rewritten, every code example a test; `sdk/README.md` for SDK maintainers
- M189: the KVM engine at x86-64-v3 (`guest.cpu: v3` on KVM; every v2 value unchanged)
- M190: hydo 0.3.0 prepared for publication (the version, the guest programs `hydo-world-tls` and
  `hydo-sql` in both releases, every example re-recorded, both releases built twice and verified)
- M191: a plan of only `checks/` or `regressions/` keeps the manifest's own `workload.run` (before, its
  saved regressions passed without running it); `W=` is refused on such an image
- M192: small fixes from the onboardings (`hydo explore --schedule` without `dseed=`, the
  diagnosis of a service that is down or does not recover, a failing `.sql` check's rows in the
  verdict, the guide on ordering by the system's own times, `kvm-check.sh` by a relative path,
  `hvx_hugeram_test` in `deploy-metal.sh`)
- M193: YugabyteDB onboarded on hvx (three nodes; the default WAL setting loses acknowledged
  writes at a power cut; a backward clock step stops the tservers); fixes to `hydo-build`'s PAX
  reader, the `debugfs` message and `L=` of an image service

## Where to look

- `docs/HISTORY.md`: every milestone with its evidence, and the superseded values.
- `docs/REBASELINE.md`: the recorded fingerprints by section; section 13 is epoch 1.
- `docs/BUILD-PINS.md`: the pinned toolchain and guest programs.
- `docs/CI.md`: what CI runs, where, and how long it takes.
- `docs/RELEASING.md` and `docs/PUBLISHING.md`: the release and publishing steps.
- `docs/design/`: the design of guest control, the KVM engine, the install cache, signing,
  `hydo explain`, `hydo inspect`, workloads, and storage faults.
- `hydo/PRODUCT.md`: hydo from a user's point of view, with the known gaps.
- `hv/hvx/`: the module (`hvx.c`, `hvx_abi.h`, `hvx_boot.h`), its tests and harnesses, and
  `images/` for the hand-written guest images.
- `hydo/crates/`: the Rust workspace. `hydo/bench/`: the bug benchmark. `hydo/onboard/`: the
  examples and `MANIFEST.md`.

## Useful tools

- Check a change on the real system, not only by reading code. Run `hv/hvx/deploy-metal.sh` for
  hvx, or the Mac engine's `hydo`.
- Review changes to the determinism engines with special care. Keep the baseline fingerprints
  passing.
- To onboard a project, follow the skill `skills/hydo-onboard/SKILL.md`. After a change, run
  `cargo test -p hydo-agents --test it skill_check:: -- --nocapture`. Put a fact about hydo into
  `hydo/guide/`, never into the skill. A skill change raises its `metadata.version` and the
  plugin entry's version together (`docs/RELEASING.md`).
- To write documentation, follow the skill `skills/hydo-docs/SKILL.md` and run `sh ci/vale.sh`.
- For kernel and VMX work, use the Intel SDM Vol. 3C (the VMX chapters), the KVM source, and the
  tutorial series Hypervisor From Scratch.
