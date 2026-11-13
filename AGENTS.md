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
  with its baseline. It runs `hvx_hugeram_test 8192` (RAM hash `b71aaf1096f96f02`, M192) and
  `hvx_cpucache_test` (M203). Its last run printed `ALL PASS (35 checks)` (M204).
  - It syncs `hvx.c` and `hvx_abi.h` only with `load`, which also rebuilds and reloads the main
    module. `load` refuses when `~/bin/hvx-reload` does not build in `~/$HVX_METAL_TREE/hv/hvx`.
  - `flagships` also runs every real-service verify.
- The main module loaded now is M203's: guest RAM up to 8 GiB, M156's PMU-clock guard, and the
  CPUID cache leaves (`HVX_VM_CPU_CACHE`, `HVX_GET_CAPS`). Its srcversion is
  `91D86B693F4D4035C368F9B`. The user reloaded it on 2026-11-08 from `main` at `8bcf6dc6`, and
  `deploy-metal.sh` printed `ALL PASS (35 checks)`. The earlier modules are in `docs/HISTORY.md`
  (Milestones 140, 156, 187 and 204).
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
- The fingerprint epoch rule. `hydo_image::EPOCH` (now 2, since M204; 1 from M155) names the
  generation of fingerprints. Raise it by one in the same commit as any change that moves a
  recorded fingerprint. Such changes are changes of an engine, the host controller, the agent pin
  or the guest kernels, or of a default that an image's run depends on. Add a section of
  `docs/REBASELINE.md` that names the new epoch. Runs of one epoch compare fingerprints. Runs of
  different epochs compare only verdicts. A default that changes with an epoch is keyed on the
  epoch in the image's record (`hydo_image::defaults`), so that an image built before keeps
  running as it did.

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

**Releases.** hydo 0.3.1 was published on 2026-11-09 and 2026-11-10 from the release commit
`b3f73087`, in these places:

- The GitHub release `v0.3.1`: both tarballs, their sources archives, and the Mac signatures.
- The Homebrew tap: `brew install kakoc/tap/hydo`.
- crates.io: the 20 crates of `cargo install hydo`, and `hydo-assert`.
- PyPI and npm: `hydo-assert`. Go: the tag `sdk/go/v0.3.1`.
- The skill release `skill-v1.2.0` (the skill did not change since 0.3.0).

0.3.1 adds Milestones 192–208 to 0.3.0 (`docs/HISTORY.md` "The 0.3.1 release"). Plans keep
`hydo_version: "0.3"`. The release index `hydo/dist/releases.txt` pins every published archive.
Since 0.2.0 the Linux release ships the KVM engine, and the Mac programs are signed and notarized.
`cargo install hydo` followed by `hydo setup` works on the Mac and, since M210, on Linux with KVM.
How to release: `docs/RELEASING.md` and `docs/PUBLISHING.md`.

**Registry publishing from GitHub Actions.** A tag `v<major>.<minor>.<patch>` starts
`.github/workflows/release.yml`. It publishes the crates, the PyPI and npm packages, and the Go tag
with each registry's trusted publishing, so no token is stored. 0.3.0 and 0.3.1 were published
this way. Other tags, such as the action's moving tag `v1`, do not start it. `packages.yml`
dry-runs the packages on every pull request that changes one. The details are in
`docs/PUBLISHING.md`, section "Publishing from GitHub Actions" (M186).

**Fingerprints.** The fingerprint epoch is 2 (M204; 1 from M155). The pinned reproducers are
`disk=none dseed=4d5cb825dfaab05b | run K=redis@11+1 C=24` under `hydo run`: `a76d7fe857fdb37c`
(VIOLATION, order 11) on `explore-ob-stack-old-disk.img` (`dab0e76c…`) and `efbde7b57264be0f`
(OK) on `explore-ob-stack-fix-disk.img` (`d61cbf00…`), in `~/hydo/build/m204` on the bare-metal
machine. Their epoch-1 images in `~/hydo/build/m155` still give `1dc3677fb94053cf` and
`ba0aa0612502a1df` under this hydo, because an image keeps the defaults of its own epoch
(`hydo_image::defaults`). `hydo/onboard/examples/stack/build-reproducers.sh` rebuilds both
images. Every recorded example value is in `hydo/onboard/recorded.json`, and `hydo-bench
rebaseline` checks or re-records them (`docs/REBASELINE.md` "How to re-baseline").

**The hvx engine.** The module (`hv/hvx/hvx.c`, ABI `hvx_abi.h`) boots Linux 6.6.35
deterministically, with guest RAM up to 8 GiB. Its clock counts VM exits, or the guest's retired
conditional branches with the PMU branch clock. Every module feature is opt-in; the engine turns on the
host-call port by default (M140), also with several vCPUs since M205. The run engine is the Rust
crate `hydo-hvx`, which runs inside `hydo` when the process can open `/dev/hvx`. Details:
`docs/HISTORY.md` Milestones 33, 66–69, 132, 137, 138, 156 and 179; the module's safety net is
`hv/hvx/SAFETY.md`.

**The Mac engine.** `hydo/crates/hydo-hvm` runs arm64 Linux 6.6.35 through Hypervisor.framework,
with two guest kernels: `Image-explore`, and `Image-explore-netns` for `network: bridge`. Its
clock counts VM exits. The program is signed with `hv/hvm/hv.entitlements` before it runs.
The details are in Milestones 48, 51, 99, 143, and 147.

**The KVM engine.** `hydo/crates/hydo-kvm` runs the x86_64 images on stock Linux KVM, without the
module or root, with one vCPU and a patched guest kernel (`bzImage-kvm`). With `--vcpus N` it runs
2 to 8 vCPUs in turns of counted exits with the SMP kernel `bzImage-kvm-smp`
(`docs/design/kvm-vcpus.md`, M219). The vCPU is at the
x86-64-v2 level, or at x86-64-v3 for `guest.cpu: v3` on a host whose KVM has AVX2 (M189;
`hv/kvm/v3-check.sh`). It runs on Intel and on AMD (GitHub's ubuntu-24.04 runners, 2026-11-08).
Its fingerprints are its own and differ between CPU vendors; the verdicts are the same. Details:
`docs/design/kvm-engine.md`, Milestones 159, 161, 175 and 189, and open item 45 in
`docs/HISTORY.md` Milestone 187. Some guest loops make no VM exits and store to memory, for
example TigerBeetle's polling of io_uring. The full-state cycles prove such a loop by the whole
vCPU state and a dirty-page compare (M194; `hydo/explore/RESEARCH-kvm-exit-free.md`). With them
TigerBeetle's single replica gives one fingerprint per schedule on Intel and on three AMD EPYC
models. Since the fingerprint epoch 2 (M204) they are on by default. Every KVM fingerprint now
leaves out what KVM keeps of a finished event. `HYDO_KVM_SPIN_STATE=0` turns the cycles off. An
image built at epoch 1 runs as it did then (`hydo-kvm --image-epoch 1`).

**The CPU's caches (M203, the default since M204).** The key `guest.cpu_cache` (a format-1
addition) makes the CPUID leaves 2, 4 and `0x80000006` describe one fixed hierarchy on hvx and
KVM. It has 32 KiB L1 data and instruction caches, a 256 KiB L2, and an 8 MiB L3. The values are
the same on every host and on both engines. Without it, the glibc 2.28 of Red Hat Enterprise
Linux 8 and its rebuilds crashes in its AVX `memcpy` at `guest.cpu: v3`. It is on for x86_64
images built at the fingerprint epoch 2 and later, and `false` turns it off. An image built at
epoch 1 keeps it off unless its manifest says `true`. On hvx it needs the module flag `HVX_VM_CPU_CACHE` and the
ioctl `HVX_GET_CAPS`, which the main module has since 2026-11-08. The details are in the section
"The CPU's caches" of `hydo/onboard/MANIFEST.md` and in `docs/HISTORY.md` Milestones 203 and 204.

**Guest control.** Every image is an agent image. The guest agent `hydo-agent` (0.3.0) is PID 1
and runs only primitives. The host controller (`hydo/crates/hydo-control`) drives services,
workload, faults and checks under lock-step control of the engine. Every run leaves a control
transcript. Details: `docs/design/guest-control.md`, Milestones 116–145. Since M222 a message
that does not fit in the Mac or KVM engine's 4,096-byte receive queue waits until it fits. A run
holds at most 65,536 messages (`hydo_engine_proto::MAX_SENDS`).

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

**Faults on several services (M195).** A manifest's `groups:` names a set of services, with an
optional `primary:` command that prints the current primary. `K=`, `P=` and `X=` take a list
(`K=n1,n2@5+2`) or a selector over a group (`K=nodes.majority@5+2`, `K=nodes.primary@5+1`), and
`N=` takes a partition (`N=n1/n2,n3@...`) or a group's shape (`halves`, `isolate`, `ring`,
`bridge`, `primary`). The control transcript records each resolution. `hydo explore
--cluster-faults` draws them from their own stream. The reference is the section `Groups of
services and partitions` of `hydo/onboard/MANIFEST.md`.

**Onboarding and SDKs.** The skill `skills/hydo-onboard/` holds the process; every hydo fact is
in the guide `hydo/guide/`, printed by `hydo guide <topic>`. The assertion SDKs are in `sdk/`
(Rust is the reference), with the shared cases in `sdk/cases/`. `sdk/README.md` is for SDK
maintainers: the shared cases, and the Go module with its frozen copy in `hydo/stack` (section
"The Go module and the frozen copy in `hydo/stack`"). Every code example on the package pages is a
test in its language (M188). The examples are in `hydo/onboard/examples/`;
`hydo/onboard/MANIFEST.md` is the project format's reference.

**Compose services built locally (M214).** For a service with `build:`, `hydo compose` builds
the image with Docker (`docker`, else `podman`, or `--docker CLI`) for this machine's guest, with
Compose's build settings. It reads the image's settings from the saved file and pins the file's
digest in the lock file (`local_build: true`). fetch-images.sh builds a missing file with
`hydo-fetch-image build` and stops at a digest other than the pin until `HYDO_ACCEPT_REBUILD=1`.
`--no-build` leaves the builds to the script. Reference: `hydo guide workflow`.

**Business rules: what found what (M215).** On Formance Ledger, rules over the ledger's own
state (balances, moves, effective volumes, overdraft, reverts, bulks, the audit log) found
nothing. A duplicate from a retry without a key, and a payment lost on a lying disk, showed only
to the rule that replays the client's acknowledged operations. A real ledger bug (a `500` when
two transactions first use one account in two assets) showed only in the error logs. The
example is `hydo/onboard/examples/formance-ledger` (RESULTS.md).

**TigerBeetle's issue 3960 (M217).** Two holds whose expiries fall on one nanosecond, and an
expiry pulse at that nanosecond, stop a TigerBeetle replica from 0.16.29 on. The example's `tie/`
makes it happen with ordinary requests and the guest's clock, on hvx, every run; the stopped
replica stops again at every restart. Blind schedules never gave a tie. The details are in the
example's RESULTS.md (the section on issue 3960).

**Lago: business rules on a billing system (M218).** On Lago v1.57.0, seven billing rules over
Lago's tables ran beside one generic rule over the client's acknowledged operations. The faults
were kills, pauses, network faults, power cuts, and clock steps aimed at the billing tick. Both kinds found a
known Lago bug: a subscription cancelled while Redis is down is never invoiced (lago-api issue
6901). Only the generic rule caught usage dropped by a termination after a clock step. The
consistency rules never failed. The example is `hydo/onboard/examples/lago` (RESULTS.md).

**Messaging checks (M196).** A `checks:` entry of `type: messages` checks a
message-passing system. The project declares the claims; hydo checks exactly those and measures
the history, and it knows no messaging system or API. The checker is the module `messages` of
`hydo-history`. Each claim is reported as `<id>.<claim>`, and `result.json` has `messages`. The
Rust SDK records with `hydo_assert::history::messages`. Reference: `hydo guide messages`; design:
`docs/design/messaging.md`; evidence and decisions: `docs/HISTORY.md` Milestone 196.

**Format-1 additions of M202.** `services[].start_after: started` starts a cluster's members
before their readiness checks run. `services[].logs` names a service's own log files for the
diagnosis and `hydo inspect logs`. `root.fix_pyc` makes Python's bytecode caches match the image's
fixed file time; it is on by default since the fingerprint epoch 2 (M204), and `false` turns it
off. With `guest.clock_after_cut`, a `cf=` cut now carries the first
boot's clock too. `hydo init` and `hydo compose` write `guest.clock_start` as the day they ran
(`HYDO_INIT_CLOCK_START`). Details: `docs/HISTORY.md` Milestone 202. Since M204 `hydo compose`
writes `start_after: started` for Compose's short form of `depends_on` too (Compose's meaning,
`service_started`), and it prints how each service starts and why. A service's `restart:` policy
is in force from its start: a process that ends during its readiness check is started again.

**Several vCPUs and flaky tests (M205).** With several vCPUs the hvx engine
reads the host-call port's records at every return of a run call and ends a call every 8,192
exits. Before, such a run never ended, because the in-kernel scheduler does not end a call at a
record. `hydo run`, `repro` and `explain` take `--vcpus N` and `--turn-margin N`, and
`result.json` records them. An exploration's "by hand" lines carry them. `hydo explore --vcpus N
--sweep turns` searches the turn lengths `q=` of one schedule for verdict boundaries
(`exploration.json` `turn_sweep`, a results addition). The format-1 addition `workload.kind:
test` makes a test program first class: one step, no fault kinds unless `faults:` lists them,
and the class `test-failed`. The guide topic is `hydo guide flaky-tests`; the example is
`hydo/onboard/examples/nats-flaky-8363`. Proof runs: `docs/HISTORY.md` Milestone 205. Since M206 an
entry of `hydo/onboard/recorded.json` can name `vcpus` (and `turn_margin`), and the example's
turn-length pair on 2 vCPUs is recorded there.
Since M211 the example `hydo/onboard/examples/etcd-flaky-22796` has a test that fails only
when its whole process gets no CPU for about 40 ms. hydo's timing tokens alone never reach it.
It fails on hvx under a CPU limit in the guest (the manifest's `limit` and `busy`), and so does
the test outside hydo.

**Refused schedules and the preemption record (M208).** An engine refuses a host part it
cannot arm before any VM: `hydo_engine::refusal`. For hvx this is the module's rule in
`hydo_hvx::sched::refusal`: no tick-jitter delay of a tick period or more, so not `tjmax=500000`
with `pp=65536`. For the Mac and KVM engines it is the hvx-only host tokens. `hydo run` stops with
the reason. The explorer records such a run with the class `refused` and counts it in the
results addition `Summary.refused`, never as a failure. On hvx, `--timing --preempt` keeps the
drawn jitter below half of `pp=`'s period, so it draws no refused combination. With
`HYDO_PREEMPT_RECORD=1`, the KVM and Mac engines write `<tag>.preempt.jsonl` for a `pct=` run. It
has one record per forced preemption point: its exit, what it did, and the guest's PC, mode,
page table, thread pointer, and stack pointer there. The engines only read registers, so no
fingerprint moves. `hydo inspect events` shows the points (kind `preempt`, `Timeline.preempt`),
and `hydo diff` names the first point where two runs' records differ (`Diff.first_preempt`). hvx
has no such record yet (open item 77). Proof: `docs/HISTORY.md` Milestone 208.

**Steered search (M209, experimental and opt-in).** It did not help on the one system it was
measured on (below), so treat it as an experiment, not a better default. `hydo explore --steer
<id>` or `--steer-match REGEX` keeps
the schedules whose runs came closest to failing, by a number the system reports. It spends
`--steer-mix` (default 500) of every 1,000 schedules on one change to them. The rest are the plain
exploration's schedules in seed order. The number comes from one of three places:

- the Rust SDK's `HYDO-STEER` records (`hydo_assert::minimize`, `maximize`; the cargo feature
  `steer`, off by default so that rkv keeps its bytes);
- a manifest `steer:` entry (a format-1 addition);
- the pattern of `--steer-match`.

The results format gains `Run.steer` and `Exploration.steer`. Without the options nothing
changes. On Qdrant's flaky test it did not help. On KVM, over five seed ranges, the steered
exploration found the first failure after a median of 118 schedules. The plain exploration found
it after 72. On hvx neither found it. Design and numbers: `docs/design/steered-search.md`;
`docs/HISTORY.md` Milestone 209.

**Pauses of the workload (M216).** The timed pause may name `workload`, the workload's own
process tree. For example, `P=workload@0~4:45` stops every process of the test program for 45 ms,
4 ms of guest time after step 0 starts. A container's CPU limit does the same. The guest's clock
measures both times, so such a pause means the same on every engine. `hydo explore
--pause-workload` draws them from their own stream. The guide topic is `hydo guide faults`,
section "Pause the workload"; the example is `hydo/onboard/examples/etcd-flaky-22796`. Details:
`docs/HISTORY.md` Milestone 216.

**State search (M212, experimental and opt-in).** It did not help on the targets it was
measured on, so treat it as an experiment. `hydo explore --search state` takes snapshots at
stops whose console shows something rare. Half of the later schedules are branches: they start
from such a snapshot and draw new faults for the remaining steps. On KVM and the Mac they also
draw new timing, with the host token `br=` and the engine command `TIME`. The rest are the plain
exploration's schedules in seed order. The search runs on one worker and writes
`state-search.jsonl`; the results formats are unchanged. On hvx, at about the same worker
seconds, it found the benchmark's d7 and d10 in 2 of 5 seed ranges each, and the plain
exploration in 3 of 5. Neither found the second symptom of etcd issue 14685 in 5 ranges each.
Design and numbers: `docs/design/state-search.md`; `docs/HISTORY.md` Milestone 212.

**A failure as one web page (M220).** `hydo report --html [DIR] [-o FILE]` writes one HTML file
for a run directory, an exploration directory or a `hydo ci run` output directory. It holds its
style sheet and one small script (the copy buttons) and loads nothing. The page shows the
summary, each failure with its minimal schedule and replay command, the failing run's timeline
(the events of `hydo inspect events`), and the first difference from a passing run. That run is
`--against RUN_DIR`, `--diff` (a `hydo diff` or `hydo explain` directory), or for an
exploration the minimal schedule's passing shrink candidates. It also shows the property table,
and for a CI run each finding's state against the base. The code is
`hydo/crates/hydo-cli/src/htmlreport.rs`, and `hydo guide inspect` describes the page in its
last section.

**Which versions have a finding (M221).** `hydo versions <run dir | explore dir | "<schedule>">`
replays a finding's minimal schedule on versions of the system, oldest first. The versions are a
manifest parameter's values (`--param KEY=V1,V2`), git revisions (`--git A..B`, a checkout and a
`--build` command each), or images (`--images`). Each version is `reproduced` (the same finding
key), `passes`, `fails-otherwise` or `cannot-run`. The search replays the newest and the oldest
version, then bisects, assuming one change point, and names a contradiction. Images come from a
cache by input key. The answer is `versions.json` (the results addition `VersionsDoc`), and
`hydo report` prints it. Reference: `hydo guide workflow`, "Which versions have a finding";
proof: `docs/HISTORY.md` Milestone 221.

**Continuous integration.** `.github/workflows/ci.yml` runs per push: formatting, clippy, and the
`fast` tests of the affected packages. `ci-full.yml` runs every test nightly and before a
release. `docs.yml` runs Vale on the documents (`docs/STYLE.md`). Details: `docs/CI.md`.

**hydo in a team's CI (stage 1 built, M197).** `docs/design/ci.md` designs hydo as part of a team's
own continuous integration in stages 0 to 4; stage 1 is built. `hydo ci run` builds the image, or
takes it from the image cache by input key, and explores it with the mode's fixed seeds. It decides
every finding against the base commit by replay: `new`, `pre-existing`, `present`, `changed`,
`fixed` or `unverified`. It replays the saved regressions and writes `ci-run.json`, `report.md`
and `junit.xml`. `hydo ci compare` compares two recorded runs offline. The settings are in
`hydo/ci.yaml` (format 1, `hydo/schema/ci-v1.json`). The adapters are the GitHub composite action
`action/action.yml` and the GitLab template `action/gitlab/hydo.gitlab-ci.yml`; `hydo guide ci`
is the recipe for other systems.

**hydo in a team's CI, stage 2 (M223).** Four parts are built in a first form:

- Accepted findings: `hydo/known-findings.yaml` (format 1, `hydo/schema/known-findings-v1.json`,
  not staged into the image) and `hydo ci accept <key> --reason ...`. An accepted finding gets
  the state `accepted` and never fails a run. An entry whose schedule passes is listed for
  removal.
- Time budgets: `budget:` per mode in `hydo/ci.yaml` or `--budget`. The exploration is cut at a
  seed boundary in seed order (`hydo explore --time-limit`), the base explores the same seeds,
  and a partial run never calls a finding fixed.
- Shards: `--shard I/N` takes the I-th contiguous part of the seed range. `hydo ci merge` makes
  one run from the shards by finding key, without a VM; the action's `merge` input runs it.
- History: `hydo ci record` writes a directory store (`v1/<project>/runs/`, `findings.jsonl`,
  `state.json`), and `hydo ci run --history` replays the branch's open findings and marks
  `reappeared` ones.

The history page, the `workflow_run` recorder and budgets from history are not built (open item
67).

**Documentation.** `docs/STYLE.md` is the writing standard. `sh ci/vale.sh` checks 51 documents
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
30. Guest control: a schedule's own `rb=` is refused for agent images; a hang over the serial
    line stays unexplained (M136, M138, M140). Several vCPUs run on agent images since M205.
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
45. The KVM engine with several vCPUs (M213, M219). Four nats schedules on 2 vCPUs gave other
    fingerprints on EPYC 7763 than on 9V74, with equal verdicts; the cause is not known yet. `hydo
    repro` of a run's folder and `hydo explain` were not run on KVM with several vCPUs, and the
    explorer does not draw KVM turn tokens. `bzImage-kvm-smp` is in no release, and Intel ran only
    the probe. On AMD, RDRAND, RDSEED and TSX cannot be made to fault, so a
    guest program that uses them without checking CPUID sees the host (M159, M161). A loop that
    counts in memory is still never proven (M194). The full-state cycles and the fingerprint
    without KVM's stale event fields are the default since M204 (h closed). Of the epoch-2
    values, only the Redis example's two schedules ran on AMD, with the Intel verdicts (M210).
46. Homebrew: no Linux formula (M168).
47. The install cache: no pruning (M162). Its Linux side fetched from GitHub in M210.
48. [user decision] The Rust test SDK is unpublished until the user decides about code-first
    tests (M160, M163).
49. `hydo diff` and `hydo explain`: deeper levels need engine additions (M165).
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
62. [user decision] `guest.outcomes_after_cut` stays opt-in until an epoch change (M184). The
    epoch 2 (M204) did not take it in: it was not among the defaults the user named.
63. [user decision] Trusted publishing works since 0.3.0. The npm entry expires after two days
    unused, so add it just before a release. A crate new to `cargo install hydo` needs its first
    version published by hand (M186).
64. YugabyteDB showed that the explorer never cuts links between peers of one cluster (M193).
    The readiness deadlock, the logs outside stdout and the `cf=` recovery clock are fixed (M202);
    the empty CPUID cache leaves have `guest.cpu_cache` (M203, the default since M204).
65. [user decision] Two identity-corpus schedules keep "the last token wins": a second `N=` on
    one target and a second `T=`. Refusing them, or applying several clock steps, re-records one
    case each. Also left: `G=` and `L=` lists, `O=` partitions, several `N=` windows at once, a
    primary followed during the fault, and runs on hvx and KVM (M195).
66. Messaging checks: no VM ran one yet, and per-window measures, `Exploration.messages` and the
    other SDKs' helpers are not built. The planted-bug example and the real-system onboardings
    come next (M196).
67. The GitHub action ran on 2026-11-13 in a private trial repository with the 0.3.1 Redis
    example. Its 2-CPU runners have KVM. A push passed with 0 findings. A pull request that set
    `appendfsync everysec` failed with 2 new findings in one comment, each with its minimal
    schedule and `hydo diff`. The trial found that the action checked `/dev/kvm` before udev had
    applied its rule; fixed in `500a0b7a`. The tags `action-v1.0.0` and `v1` exist since then,
    and `release.yml` starts only on full version tags. The GitLab template never ran (M197).
    Stage 2 (M223) ran only on this Mac with the Mac engine. The action's `merge` input and the
    matrix example of `hydo guide ci` passed actionlint and shellcheck but never ran on GitHub.
    These parts are not built:
    - the history page (`hydo ci page`) and the `workflow_run` recorder of pull requests;
    - a history branch that the action keeps;
    - budgets turned into seed counts from history, and proposed regression files;
    - one shrink per signature across shards (each shard shrinks its own).
68. Agent environments: the guest has no network and one vCPU. There is no incremental state
    hash, so the full fingerprint costs about 0.8 s per call. A snapshot cannot be dropped, and a
    session on hvx holds only 5 branches (M199).
69. The KVM engine gave two fingerprints for one Ghost schedule
    (`H=mailgun.messages@0-12:status=500,effect=yes`) with the same transcript, consoles and
    verdict; not investigated (M200).
71. The `cf=` clock carry of `guest.clock_after_cut` ran only on the scripted engine; hvx, the
    one engine that applies `cf=`, did not run it. `start_after: started` ran on the Mac only
    (M202). The restart of a service that ends during its readiness check ran only on the
    scripted engine (M204).
75. Several vCPUs and the turn sweep (M205). The sweep bisects only between the lengths it ran,
    so a narrow band between two grid points stays hidden. At epoch 2 the dense sweep of the
    nats-server image stepped over the failing `q=18140`. A test workload with services or with
    `faults:` was not run.
77. The preemption record (M208) exists on the KVM and Mac engines only. hvx's points fire
    inside the kernel module: `pp=` is a shorter tick period and `xt=` adds extra ticks. A record
    there needs a module change: a small log of the ticks the schedule forced, with RIP, CR3, and
    the FS base, read by a new ioctl. Without it, a verdict that flips between two hvx timing
    schedules cannot be traced to the tick that made the difference. The record names a thread
    by its thread pointer, not by its process or thread id; reading the guest kernel's `current`
    task needs its symbols. On the Mac the record ran only with the scripted tests and one Redis
    schedule, not on a flaky test. Once, in the full tier on this Mac, the scripted KVM test
    `the_rewind_mode_changes_nothing_without_a_counter_loop` gave two fingerprints. It then
    passed 50 times, 44 of them under CPU load. The cause was not found; if it is a rare
    nondeterminism of the rewind mode, CI's full tier can meet it again.
78. An image's bytes depend on the file modes of the tree it is built from. `hydo build` copies
    the manifest into the image as `/etc/hydo/hydo.yaml` with the checkout's mode. 0.3.1's first
    hvx re-baseline ran in a tree unpacked under umask 002, and every image got new bytes. Unpack a
    tree for `hydo-bench rebaseline` or a release build under umask 022. Normalising the mode in
    hydo-build would move the bytes of images built from such trees only, but it is a builder
    change for a release that may move image bytes. Also seen in 0.3.1's Linux full tier, once:
    `hydo-compose`'s `compose_equals_python` failed and then passed three times alone; the cause
    was not found.
79. The steered search (M209). What is left:
    - The Go, Python, and JavaScript SDKs have no steered values; their case readers skip
      `sdk/cases/steer/`. Adding them moves the bytes of the python-kv and node-kv images,
      whose SDK files are inside them, so it needs a re-record.
    - The search has one value. Values per code location as in IJON are not built, nor a
      distance that hydo computes itself (a fault's step against the step of a logged event).
    - No run showed a benefit. On Qdrant the failure sits next to passing schedules of any
      length, and on hvx the length does not move. The option stays experimental until a system
      whose value falls step by step towards its failure is found.
80. CPU-limit pauses (M211, M216). `P=workload@<at>~<off>:<dur>` stops the whole workload's
    process tree. Two kinds are not built: a pause of one thread, which Qdrant's race needs (its
    RESULTS.md, item 4), and pauses that repeat in every period of a CPU limit. KVM did not run such
    a pause yet: the one-off workflow on the branch `tmp-workload-pause-kvm` runs the etcd example
    on a GitHub runner. The etcd example is not in `hydo/onboard/recorded.json` yet.
81. Compose builds (M214). A Docker build rarely gives the same bytes twice. Docker 28.4 writes a
    pointer into the history of an `EXPOSE` step, so each rebuild of such an image stops
    fetch-images.sh until `HYDO_ACCEPT_REBUILD=1`. The real build ran on a Mac with Docker only;
    Linux and Podman ran only with a fake command. `ssh`, `secrets`, `dockerfile_inline` and
    `additional_contexts` are not passed to the build. No VM ran a built image.
85. The state search (M212). What is left:
    - The Qdrant flaky test on KVM, the one target where only the timing varies, was not
      measured: the build machine was not reachable.
    - The KVM engine's change (timing kept in a snapshot, and `TIME`) ran only in the scripted
      tests, never on a real KVM host. The Mac engine's change ran on the Mac.
    - The shrinker does not know `br=`: a shrink candidate of a timing branch loses its branch
      point. Explorations with timing branches need `--no-shrink`.
    - hvx has no timing branches: its snapshots of `tj=`, `xt=`, `pp=`, `q=` and `cf=` schedules
      stay refused, so the search cannot branch towards d11.
    - A search runs on one worker, because the snapshots live in one engine process.
86. TigerBeetle's issue 3960 (M217). The directed schedule `tie/` runs one replica only. In a
    three-replica cluster TigerBeetle clamps request timestamps to the cluster's agreed time, so
    the guest-clock construction does not carry over. Whether every replica stops is not
    checked. The release binaries are stripped, so the guest shows `panic: reached unreachable
    code` without the assert; no build with symbols ran. The Mac engine cannot boot TigerBeetle
    (`ERROR boot: SPIN`, open item 13). Since M222 a replica that stops in a run without faults
    gets its own verdict, and the checks run; the `tie/` schedule did not run again with it.
87. `eventually_` commands and checks (M218). The `eventually_` checks poll from the heal until
    their deadline before the `eventually_` commands run. So a command cannot prepare what a
    check waits for, such as Lago's billing tick. The example waits inside the command instead.
88. A seed's client records (M218). `$HYDO_ACKED` and `$HYDO_OUTCOMES` of the `first_` commands
    are not the run's files, so operations made in the seed are not in the client's records. The
    Lago example writes them to the data disk.
89. A check's output (M218). Only a check's last line reaches the verdict and `result.json`. A
    check that lists what differs must also write to `/dev/console`, or each explanation needs a
    replay with `hydo inspect`.
90. One shrink per signature (M218). The explorer shrinks the first failure of each signature,
    so a signature with several causes is explained by one of them. A check has to split its
    own last lines by cause.
91. The importer's job services (M218). `hydo compose` keeps a one-shot job that others wait for
    (`service_completed_successfully`) as a service that sleeps. The explorer draws faults on
    it: 6 of 20 schedules in the Lago example.
92. Clock steps at a time (M218). The explorer steps the clock by fixed amounts only. Lago bills
    at minute 10 of the hour, on the first of the month, so a step to that time needs a hand
    schedule.
94. The web page of `hydo report --html` (M220). Its section "Versions" reads the
    `versions.json` of `hydo versions` by its types since M221. The CI page ran only on the
    recorded example `mac-shop-ci-run`, which has no replay directories, so a CI finding's
    timeline was never shown from a real `hydo ci run`. On hvx the page ran only on the recorded
    runs of `tests/fixtures/inspect/`, and it never ran on a run with forced preemption points.
95. `hydo versions` (M221). A timing-sensitive finding depends on its image's exact bytes. etcd
    14685's schedule of M206 passed on 3.5.5 images built with `--param` outside the
    re-baseline's staging, so no hvx finding reproduced in the proof; `--found-on` warns of this.
    `--git` ran only in the scripted test, and `--all` and contradictions only there and in unit
    tests. Replays run one at a time. `hydo ci run` does not call it yet (stage 2 of
    `docs/design/ci.md` could, for its `new` findings).

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
- M192: small fixes from the onboardings: `hydo explore --schedule` without `dseed=`, the
  diagnosis of a service that is down, a failing `.sql` check's rows, and `hvx_hugeram_test` in
  `deploy-metal.sh`
- M193: the YugabyteDB onboarding on hvx, with three hydo fixes. The default WAL setting loses
  acknowledged writes at a power cut, and a backward clock step stops the tservers
- M194: guest loops without VM exits on KVM: research, and the opt-in prototype
  `HYDO_KVM_SPIN_STATE=1`, with which TigerBeetle's single replica runs on KVM
- M195: lists and group selectors for `K=`, `P=` and `X=`, partition shapes for `N=`, and
  `--cluster-faults`
- M196: messaging checks, `type: messages` (the checker, the declaration with
  `include:` bundles, `Run.messages`, the Rust recorder)
- M197: hydo in a team's CI, stage 1: `hydo ci run` and `hydo ci compare`, `hydo/ci.yaml`,
  the GitHub action and the GitLab template, `hydo guide ci`
- M198: cost tracking as deterministic work counts, an experiment with the opt-in probe
  `HYDO_COST` (`docs/design/cost-tracking.md`)
- M199: deterministic, branchable environments for AI agents, a spike (`hydo inspect exec
  --branch`, `state_fp`, `docs/design/agent-environments.md`)
- M200: the Ghost onboarding on KVM. A donation is recorded once per webhook delivery (known
  upstream), and a newsletter batch whose answer is lost goes out again
- M201: flaky test verdicts of 31 SWE-bench Lite tasks. No verdict flipped natively or under
  hydo, and stale `.pyc` caches came from the fixed file times (`docs/design/flaky-verdicts.md`)
- M202: seven fixes from the YugabyteDB onboarding: `services[].start_after`, the `cf=` clock
  carry under `guest.clock_after_cut`, `services[].logs`, `clock_start` in new projects,
  `root.fix_pyc`, fixed `hydo inspect` output names, and the Compose importer's header and
  per-architecture fetch script; no recorded value moved
- M203: `guest.cpu_cache` makes CPUID describe a fixed cache hierarchy on hvx (the module flag
  `HVX_VM_CPU_CACHE`, on a side module until the main module is reloaded) and on KVM. With it,
  AlmaLinux 8's glibc copies work at v3. It is opt-in, and nothing recorded moved
- M204: the fingerprint epoch 2. `guest.cpu_cache` and `root.fix_pyc` are on by default, the
  KVM engine's full-state cycles too, and its fingerprint drops KVM's stale event fields. Compose's
  short-form `depends_on` imports as `start_after: started`, and restart policies hold from a
  service's start. An image built at epoch 1 keeps its defaults. Every example is re-recorded; on
  hvx three timing-sensitive verdicts changed with the cache leaves (decided in M206)
- M205: several vCPUs with the host-call port, `--vcpus` on `hydo run`, `repro` and
  `explain`, `hydo explore --sweep turns`, and `workload.kind: test` for test programs, from the
  nats-server flaky-test example; no recorded value moved
- M206: the three epoch-2 verdict changes decided. stack-errors-fix's is accepted (the same
  class), etcd 14685 has a new schedule (`K=etcd2@20+1 P=etcd1@26+1`), and miniflux's API check
  was too strict and is fixed. `recorded.json` takes `vcpus`, and the nats-flaky-8363 pair is in it
- M207 (tentative number): open item 76 explained and fixed. hydo-bench's worker pools read a
  composed plan's manifest without its plan meta, the reading failed, and at epoch 2 their guests
  booted without the CPUID cache leaves. Every boot path now reads the guest's settings with
  `hydo_control::agent::image_guest`, and the fast rebaseline also runs each image's first schedule
  in the pool and requires the fresh boot's values. Nothing recorded moved
- M208 (tentative number): from the Qdrant flaky-test example. `hydo explore --timing --preempt`
  on hvx no longer draws tick jitter that `pp=`'s shorter period makes the module refuse, which
  gave false `test-failed` failures. Engines refuse what they cannot arm before any VM, and the
  explorer never counts a refused run as a failure. On KVM and the Mac, `HYDO_PREEMPT_RECORD=1`
  records where each forced preemption point fired, and `hydo diff` names the first point where
  two runs differ: on the Qdrant pair it is the fortieth point. Nothing recorded moved
- The 0.3.1 release (no milestone number): the version raised, and `hydo-world`,
  `hydo-world-tls` and `hydo-grpc` re-pinned. Release step 6a ran on quick candidates, and its
  fixes are in: hydo takes the machine's engine without `--backend`, `hydo doctor` prints the
  state it found, and the pages are corrected. The examples are re-recorded (epoch 2). Both
  releases are built twice to the same bytes, the Mac one signed and notarized, and verified on
  hvx, KVM and the Mac; nothing is published
- M209: an experimental steered search, `hydo explore --steer` or `--steer-match`. It steers by
  a number the system reports about how close a run came to failing: a Rust SDK record, a
  manifest `steer:` entry, or a pattern. It did not help: on Qdrant's flaky test it found the
  failure later than the plain exploration on KVM (median 118 against 72 schedules), and neither
  found it on hvx. Nothing recorded moved
- M210: `hydo <command> --help` prints the command's options without an image, a project, or a
  run (before, `hydo explore --help` looked for an image). `cargo install hydo` and `hydo setup`
  ran on Linux: on a GitHub runner with KVM the Redis example gave the quickstart's verdicts, so
  the README offers cargo on Linux too (open item 50 closed)
- M211: etcd's flaky `TestCacheCompactionResync` (issue 22796). It needs the test process held
  off the CPU for about 40 ms. It never failed natively without a CPU limit (0 of 8,000) or under
  hydo's timing tokens. With a CPU limit in the guest, `disk=none pp=65536 | run` fails on every
  hvx run and `disk=none | run` passes; the test-only fix passes. The same limit fails it natively
  too. A fjall deadlock (issue 345) was tried with a stress program and not reproduced
- M212: an experimental state search, `hydo explore --search state`. Runs take snapshots at
  rare console features, and branches run the rest of a run from them with new faults (and new
  timing on KVM and the Mac, `br=`). On hvx it did not help: d7 and d10 in 2 of 5 seed ranges
  against 3 of 5 for the plain exploration, and neither found etcd 14685's second symptom.
  Nothing recorded moved
- M213 (tentative number): several vCPUs on the KVM engine, stage 1. The design is
  `docs/design/kvm-vcpus.md`: turns of exits on one thread, and an x2APIC per vCPU in the engine.
  A KVM probe ran on three AMD and one Intel runner model with one hash per turn length.
  The one-vCPU pins, the APIC model and the SMP kernel's recipe are ready; nothing recorded moved
- M214 (tentative number): one import for Compose services with `build:`. `hydo compose` builds
  their images with Docker and pins each file's digest; fetch-images.sh builds a missing file and
  stops at other bytes until they are accepted. Nothing recorded moved
- M215 (tentative number): the Formance Ledger onboarding, an experiment in business bugs on the
  Mac engine. Two first uses of one account in two assets race, and one payment fails with
  `500`. It is a ledger bug since v2.4; the error logs found it, no rule did. Duplicates from
  retries without keys, and payments lost to disk faults, show only to the rule over the client's
  records. Nothing recorded moved
- M216 (tentative number): pauses of the workload. The timed pause names the workload's own
  process tree (`P=workload@0~4:45`), and `hydo explore --pause-workload` draws it. etcd's flaky
  test fails on every run of one such pause without a CPU-limit model, on the Mac and on hvx; its
  neighbours pass. Qdrant's race needs a pause of one thread. Nothing recorded moved
- M217 (tentative number): TigerBeetle's issue 3960, two holds that expire on the same
  nanosecond. 150 blind schedules with holds gave no tie. With the guest's clock set by the
  workload, 0.16.29, 0.16.45 and 0.17.9 stop on every run and at every restart. The pulse 1 ns
  later passes, and so does 0.16.28. Two false findings of the new hold check fixed. Nothing recorded
  moved
- M218 (tentative number): the Lago onboarding, an experiment in business bugs on the Mac
  engine. A subscription cancelled while Redis is down is never invoiced (lago-api issue 6901,
  known), found blind by both kinds of rules. A Redis error in Sidekiq's first scheduler cleanup
  stops all retries. Power cuts, a skipped billing day, and backward clock steps drop acknowledged
  usage by documented or environment causes. Nothing recorded moved
- M219 (tentative number): several vCPUs on the KVM engine, stages 2 to 4. `--vcpus N` runs 2 to
  8 vCPUs in turns of counted exits with an x2APIC per vCPU in the engine and the pinned SMP
  kernel `bzImage-kvm-smp`. Redis on 2 and 4 vCPUs gave one fingerprint per schedule on two AMD
  models, and a run from S1 the fresh boot's. nats-server's flaky test passes at 10 of 28 turn
  settings on 2 vCPUs and fails at the rest; one vCPU gave its recorded values; nothing recorded
  moved
- M220 (tentative number): `hydo report --html` writes a failure as one self-contained web page,
  for a run, an exploration, or a CI run. It shows the failing run's timeline and its first
  difference from a passing neighbour. Proven on the Redis example's power cut and a swarm
  exploration on the Mac. Nothing recorded moved
- M221 (tentative number): `hydo versions` replays a finding on versions of the system (a
  manifest parameter, git revisions, or images). It bisects for where the finding was introduced
  and whether the newest version still has it. `versions.json` is a results addition, shown by
  `hydo report` and its web page. Formance's account race: introduced in v2.4.0, v2.3.24
  passes, still in v2.4.18, in 2 replays. Nothing recorded moved
- M222 (tentative number): five fixes from the onboardings. The Mac and KVM engines type a
  message only when it fits in the receive queue, so a recovery boot keeps carried records past
  4 KiB. A run holds 65,536 messages. The default error template blanks request ids. `hydo
  errors accept` writes no login name. A service that stops in a run without faults gets its own
  verdict, and the checks still run. The explorer's goldens leave out git-ignored files. Nothing
  recorded moved
- M223 (tentative number): hydo in a team's CI, stage 2. Accepted findings
  (`hydo/known-findings.yaml`, `hydo ci accept`), a time budget cut at a seed boundary
  (`budget:`, `--budget`, `hydo explore --time-limit`), shards merged without a VM
  (`--shard I/N`, `hydo ci merge`, the action's `merge` input), and a directory history store
  (`hydo ci record`, `--history`, `reappeared`). Proven on the Mac with the Redis example's
  `everysec` change. Nothing recorded moved

## Where to look

- `docs/HISTORY.md`: every milestone with its evidence, and the superseded values.
- `docs/REBASELINE.md`: the recorded fingerprints by section; section 13 is epoch 1, section 19
  is epoch 2.
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
